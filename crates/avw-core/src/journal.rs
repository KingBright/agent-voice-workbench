//! A single-writer, checksummed, fsynced job journal.
//! Only an incomplete final frame is truncated on restart; checksum corruption
//! is an error. The process lock is held for the lifetime of the journal.
use crate::{Error, Result, artifacts::{AssetMeta, digest}, control::now_ms, error::Failure, types::Submission};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, HashMap}, fs::{File, OpenOptions}, io::{Read, Seek, SeekFrom, Write}, path::Path};

const MAGIC: &[u8; 8] = b"AVWJ0001";
const MAX_RECORD: usize = 1024 * 1024;
const MAX_JOURNAL: usize = 128 * 1024 * 1024;
const MAX_JOBS: usize = 10_000;
const MAX_PENDING: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState { Queued, Running, Cancelling, Succeeded, Failed, Cancelled }
impl JobState {
    pub fn terminal(self) -> bool { matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled) }
    fn allows(self, next: Self) -> bool {
        match self {
            Self::Queued => matches!(next, Self::Running | Self::Failed | Self::Cancelled),
            Self::Running => matches!(next, Self::Cancelling | Self::Succeeded | Self::Failed | Self::Cancelled),
            Self::Cancelling => next == Self::Cancelled,
            Self::Succeeded | Self::Failed | Self::Cancelled => false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub artifacts: Vec<AssetMeta>,
    pub summary: serde_json::Value,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobInfo {
    pub id: String,
    pub state: JobState,
    pub submitted_seq: u64,
    pub revision: u64,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
    pub deadline_ms: u64,
    pub result: Option<ExecutionResult>,
    pub failure: Option<Failure>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub info: JobInfo,
    pub submission: Submission,
    pub request_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum EventKind {
    Submitted { job: Box<Job> },
    Changed { id: String, state: JobState, result: Option<ExecutionResult>, failure: Option<Failure>, reason: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Event { seq: u64, at_ms: u64, #[serde(flatten)] kind: EventKind }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventView { pub seq: u64, pub at_ms: u64, pub job_id: String, pub state: JobState, pub detail: String }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Page<T> { pub items: Vec<T>, pub next_after: u64, pub has_more: bool }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submitted { pub job: JobInfo, pub reused: bool }

#[derive(Debug)]
pub struct Journal {
    file: File,
    _lock: File,
    jobs: BTreeMap<String, Job>,
    keys: HashMap<String, String>,
    events: Vec<EventView>,
    next_seq: u64,
    bytes: usize,
    poisoned: bool,
}
impl Journal {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        for name in ["workbench.lock", "jobs.avwj"] {
            if root.join(name).symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) { return Err(Error::Integrity("journal paths cannot be symlinks".into())); }
        }
        let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(root.join("workbench.lock"))?;
        FileExt::try_lock_exclusive(&lock).map_err(|e| {
            if e.kind() == std::io::ErrorKind::WouldBlock { Error::Conflict("workspace is in use; use the running server's HTTP API".into()) } else { Error::Io(e) }
        })?;
        let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(root.join("jobs.avwj"))?;
        if file.metadata()?.len() == 0 { file.write_all(MAGIC)?; file.sync_all()?; }
        if file.metadata()?.len() > MAX_JOURNAL as u64 { return Err(Error::Capacity("job journal exceeds 128 MiB".into())); }
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new(); file.read_to_end(&mut bytes)?;
        if !bytes.starts_with(MAGIC) { return Err(Error::Integrity("invalid journal magic/version".into())); }
        let mut journal = Self { file, _lock: lock, jobs: BTreeMap::new(), keys: HashMap::new(), events: Vec::new(), next_seq: 1, bytes: 8, poisoned: false };
        let mut offset = 8usize;
        while offset < bytes.len() {
            if bytes.len() - offset < 8 { break; }
            let len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().map_err(|_| Error::Integrity("bad journal frame".into()))?) as usize;
            let inverse = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().map_err(|_| Error::Integrity("bad journal length guard".into()))?);
            if inverse != !(len as u32) { return Err(Error::Integrity("journal length guard mismatch".into())); }
            if len == 0 || len > MAX_RECORD { return Err(Error::Integrity("invalid journal frame length".into())); }
            let end = offset + 8 + 32 + len;
            if end > bytes.len() { break; }
            let payload = &bytes[offset + 40..end];
            if Sha256::digest(payload)[..] != bytes[offset + 8..offset + 40] { return Err(Error::Integrity(format!("journal checksum mismatch at byte {offset}"))); }
            let event: Event = serde_json::from_slice(payload)?;
            journal.validate_event(&event)?;
            journal.project(&event)?;
            offset = end;
        }
        if offset != bytes.len() { journal.file.set_len(offset as u64)?; journal.file.sync_all()?; }
        journal.bytes = offset;
        journal.file.seek(SeekFrom::End(0))?;
        Ok(journal)
    }
    pub fn get(&self, id: &str) -> Result<JobInfo> { Ok(self.get_full(id)?.info.clone()) }
    pub fn get_full(&self, id: &str) -> Result<&Job> { self.jobs.get(id).ok_or_else(|| Error::NotFound(format!("job {id}"))) }
    pub fn list(&self, after: u64, limit: usize) -> Page<JobInfo> {
        let mut entries: Vec<_> = self.jobs.values().filter(|j| j.info.submitted_seq > after).map(|j| j.info.clone()).collect();
        entries.sort_by_key(|j| j.submitted_seq);
        let limit = limit.clamp(1, 100); let has_more = entries.len() > limit; entries.truncate(limit);
        let next_after = entries.last().map_or(after, |j| j.submitted_seq);
        Page { items: entries, next_after, has_more }
    }
    pub fn events(&self, id: &str, after: u64, limit: usize) -> Result<Page<EventView>> {
        self.get_full(id)?;
        let mut iter = self.events.iter().filter(|e| e.job_id == id && e.seq > after);
        let entries: Vec<_> = iter.by_ref().take(limit.clamp(1, 100)).cloned().collect();
        let has_more = iter.next().is_some();
        let next_after = entries.last().map_or(after, |e| e.seq);
        Ok(Page { items: entries, next_after, has_more })
    }
    pub fn submit(&mut self, submission: Submission) -> Result<Submitted> {
        submission.validate()?;
        let hash = digest(&serde_json::to_vec(&(&submission.task, submission.timeout_secs))?);
        if let Some(id) = submission.idempotency_key.as_ref().and_then(|key| self.keys.get(key)) {
            let existing = self.get_full(id)?;
            if existing.request_hash != hash { return Err(Error::Conflict("idempotency key already belongs to a different request".into())); }
            return Ok(Submitted { job: existing.info.clone(), reused: true });
        }
        // Reserve 8 MiB for bounded terminal/error events of up to 256 admitted jobs.
        if self.bytes > MAX_JOURNAL - 9 * 1024 * 1024 { return Err(Error::Capacity("journal is nearly full; use a new workspace after current jobs finish".into())); }
        if self.jobs.len() >= MAX_JOBS || self.jobs.values().filter(|j| !j.info.state.terminal()).count() >= MAX_PENDING {
            return Err(Error::Capacity("workspace job or queue limit reached".into()));
        }
        let created_ms = now_ms();
        let id = uuid::Uuid::new_v4().to_string();
        let info = JobInfo { id: id.clone(), state: JobState::Queued, submitted_seq: self.next_seq, revision: self.next_seq, created_ms, updated_ms: created_ms, started_ms: None, ended_ms: None,
            deadline_ms: created_ms.saturating_add(submission.timeout_secs * 1000), result: None, failure: None };
        self.append(EventKind::Submitted { job: Box::new(Job { info, submission, request_hash: hash }) })?;
        Ok(Submitted { job: self.get(&id)?, reused: false })
    }
    pub fn claim_next(&mut self) -> Result<Option<Job>> {
        loop {
            let id = self.jobs.values().filter(|j| j.info.state == JobState::Queued).min_by_key(|j| j.info.submitted_seq).map(|j| j.info.id.clone());
            let Some(id) = id else { return Ok(None); };
            if self.get(&id)?.deadline_ms <= now_ms() {
                self.change(&id, JobState::Failed, None, Some(Error::Deadline.failure()), "expired in queue")?;
                continue;
            }
            self.change(&id, JobState::Running, None, None, "worker claimed job")?;
            return Ok(Some(self.get_full(&id)?.clone()));
        }
    }
    pub fn cancel(&mut self, id: &str) -> Result<JobInfo> {
        match self.get(id)?.state {
            JobState::Queued => self.change(id, JobState::Cancelled, None, None, "cancelled before execution")?,
            JobState::Running => self.change(id, JobState::Cancelling, None, None, "cooperative cancellation requested")?,
            _ => {}
        }
        self.get(id)
    }
    pub fn finish(&mut self, id: &str, result: Result<ExecutionResult>) -> Result<JobInfo> {
        let result = match result {
            Ok(value) if serde_json::to_vec(&value)?.len() > 16 * 1024 => Err(Error::Capacity("job summary exceeds 16 KiB; store payloads as assets".into())),
            other => other,
        };
        if self.get(id)?.state == JobState::Cancelling {
            self.change(id, JobState::Cancelled, None, None, "worker acknowledged cancellation")?;
        } else {
            match result {
                Ok(result) => self.change(id, JobState::Succeeded, Some(result), None, "execution completed")?,
                Err(Error::Cancelled) => self.change(id, JobState::Cancelled, None, None, "execution cancelled")?,
                Err(error) => self.change(id, JobState::Failed, None, Some(error.failure()), "execution failed")?,
            }
        }
        self.get(id)
    }
    pub fn recover_interrupted(&mut self) -> Result<usize> {
        let pending: Vec<_> = self.jobs.values().filter(|j| matches!(j.info.state, JobState::Running | JobState::Cancelling)).map(|j| j.info.id.clone()).collect();
        for id in &pending {
            if self.get(id)?.state == JobState::Cancelling {
                self.change(id, JobState::Cancelled, None, None, "cancellation recovered after restart")?;
            } else {
                let failure = Failure { code: "process_interrupted".into(), message: "execution was interrupted; not automatically retried".into(), retryable: true };
                self.change(id, JobState::Failed, None, Some(failure), "recovered interrupted execution")?;
            }
        }
        Ok(pending.len())
    }
    fn change(&mut self, id: &str, state: JobState, result: Option<ExecutionResult>, failure: Option<Failure>, reason: &str) -> Result<()> {
        self.append(EventKind::Changed { id: id.into(), state, result, failure, reason: reason.into() })
    }
    fn append(&mut self, kind: EventKind) -> Result<()> {
        if self.poisoned { return Err(Error::Integrity("journal write failed earlier; restart required".into())); }
        let event = Event { seq: self.next_seq, at_ms: now_ms(), kind };
        self.validate_event(&event)?;
        let payload = serde_json::to_vec(&event)?;
        if payload.len() > MAX_RECORD || self.bytes + 40 + payload.len() > MAX_JOURNAL { return Err(Error::Capacity("journal storage limit reached".into())); }
        let mut frame = Vec::with_capacity(40 + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        frame.extend_from_slice(&(!(payload.len() as u32)).to_le_bytes());
        frame.extend_from_slice(&Sha256::digest(&payload)); frame.extend_from_slice(&payload);
        if let Err(error) = self.file.write_all(&frame).and_then(|_| self.file.sync_all()) { self.poisoned = true; return Err(Error::Io(error)); }
        if let Err(error) = self.project(&event) { self.poisoned = true; return Err(error); }
        self.bytes += frame.len(); Ok(())
    }
    fn validate_event(&self, event: &Event) -> Result<()> {
        if event.seq != self.next_seq { return Err(Error::Integrity("journal event sequence is not contiguous".into())); }
        match &event.kind {
            EventKind::Submitted { job } => {
                job.submission.validate()?;
                if self.jobs.contains_key(&job.info.id) || job.info.state != JobState::Queued || job.info.submitted_seq != event.seq || uuid::Uuid::parse_str(&job.info.id).is_err() {
                    return Err(Error::Integrity("invalid submitted job".into()));
                }
                if job.submission.idempotency_key.as_ref().is_some_and(|k| self.keys.contains_key(k)) { return Err(Error::Integrity("duplicate journal idempotency key".into())); }
                if digest(&serde_json::to_vec(&(&job.submission.task, job.submission.timeout_secs))?) != job.request_hash { return Err(Error::Integrity("request hash mismatch".into())); }
            }
            EventKind::Changed { id, state, result, failure, .. } => {
                if !self.get_full(id)?.info.state.allows(*state) { return Err(Error::Conflict("illegal job state transition".into())); }
                if (*state == JobState::Succeeded) != result.is_some() || (*state == JobState::Failed) != failure.is_some() { return Err(Error::Integrity("state/result mismatch".into())); }
            }
        }
        Ok(())
    }
    fn project(&mut self, event: &Event) -> Result<()> {
        let view = match &event.kind {
            EventKind::Submitted { job } => {
                if let Some(k) = &job.submission.idempotency_key { self.keys.insert(k.clone(), job.info.id.clone()); }
                self.jobs.insert(job.info.id.clone(), (**job).clone());
                EventView { seq: event.seq, at_ms: event.at_ms, job_id: job.info.id.clone(), state: JobState::Queued, detail: "submitted".into() }
            }
            EventKind::Changed { id, state, result, failure, reason } => {
                let job = self.jobs.get_mut(id).ok_or_else(|| Error::Integrity("event references missing job".into()))?;
                job.info.state = *state; job.info.revision = event.seq; job.info.updated_ms = event.at_ms;
                if *state == JobState::Running { job.info.started_ms = Some(event.at_ms); }
                if state.terminal() { job.info.ended_ms = Some(event.at_ms); }
                job.info.result = result.clone(); job.info.failure = failure.clone();
                EventView { seq: event.seq, at_ms: event.at_ms, job_id: id.clone(), state: *state, detail: reason.clone() }
            }
        };
        self.events.push(view); self.next_seq += 1; Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Task, SubtitleFormat, AssetId};
    fn submission(key: &str) -> Submission { Submission { task: Task::ExportSubtitles { transcript: AssetId::try_from("0".repeat(64)).unwrap(), format: SubtitleFormat::Srt }, idempotency_key: Some(key.into()), timeout_secs: 60 } }
    fn output() -> ExecutionResult { ExecutionResult { artifacts: vec![], summary: serde_json::json!({"test": true}), warnings: vec![] } }
    #[test] fn idempotency_is_durable() {
        let dir = tempfile::tempdir().unwrap(); let id;
        { let mut j = Journal::open(dir.path()).unwrap(); id = j.submit(submission("same")).unwrap().job.id; }
        let mut j = Journal::open(dir.path()).unwrap(); let s = j.submit(submission("same")).unwrap(); assert!(s.reused); assert_eq!(s.job.id, id);
    }
    #[test] fn same_key_different_body_conflicts() { let dir = tempfile::tempdir().unwrap(); let mut j = Journal::open(dir.path()).unwrap(); j.submit(submission("same")).unwrap(); let mut s = submission("same"); s.timeout_secs = 61; assert!(matches!(j.submit(s), Err(Error::Conflict(_)))); }
    #[test] fn holds_exclusive_workspace_lock() { let dir = tempfile::tempdir().unwrap(); let _j = Journal::open(dir.path()).unwrap(); assert!(Journal::open(dir.path()).is_err()); }
    #[test] fn success_is_terminal() {
        let dir = tempfile::tempdir().unwrap(); let mut j = Journal::open(dir.path()).unwrap(); let id = j.submit(submission("x")).unwrap().job.id; j.claim_next().unwrap(); j.finish(&id, Ok(output())).unwrap();
        assert!(j.finish(&id, Ok(output())).is_err()); assert_eq!(j.cancel(&id).unwrap().state, JobState::Succeeded);
    }
    #[test] fn cancel_queued_never_runs() { let dir = tempfile::tempdir().unwrap(); let mut j = Journal::open(dir.path()).unwrap(); let id = j.submit(submission("x")).unwrap().job.id; assert_eq!(j.cancel(&id).unwrap().state, JobState::Cancelled); assert!(j.claim_next().unwrap().is_none()); }
    #[test] fn cancellation_wins_completion_race() {
        let dir = tempfile::tempdir().unwrap(); let mut j = Journal::open(dir.path()).unwrap(); let id = j.submit(submission("x")).unwrap().job.id; j.claim_next().unwrap(); j.cancel(&id).unwrap();
        let result = j.finish(&id, Ok(output())).unwrap(); assert_eq!(result.state, JobState::Cancelled); assert!(result.result.is_none());
    }
    #[test] fn interruption_does_not_silently_retry() {
        let dir = tempfile::tempdir().unwrap(); let id;
        { let mut j = Journal::open(dir.path()).unwrap(); id = j.submit(submission("x")).unwrap().job.id; j.claim_next().unwrap(); }
        let mut j = Journal::open(dir.path()).unwrap(); assert_eq!(j.recover_interrupted().unwrap(), 1); assert_eq!(j.get(&id).unwrap().failure.unwrap().code, "process_interrupted"); assert!(j.claim_next().unwrap().is_none());
    }
    #[test] fn recovers_only_incomplete_tail() {
        let dir = tempfile::tempdir().unwrap(); let id;
        { let mut j = Journal::open(dir.path()).unwrap(); id = j.submit(submission("x")).unwrap().job.id; }
        let path = dir.path().join("jobs.avwj"); let length = std::fs::metadata(&path).unwrap().len();
        OpenOptions::new().append(true).open(&path).unwrap().write_all(&[100, 0, 0, 0, 1, 2, 3]).unwrap();
        let j = Journal::open(dir.path()).unwrap(); assert!(j.get(&id).is_ok()); assert_eq!(std::fs::metadata(&path).unwrap().len(), length);
    }
    #[test] fn rejects_full_frame_checksum_corruption() {
        let dir = tempfile::tempdir().unwrap(); { Journal::open(dir.path()).unwrap().submit(submission("x")).unwrap(); }
        let path = dir.path().join("jobs.avwj"); let mut bytes = std::fs::read(&path).unwrap(); bytes[16] ^= 1; std::fs::write(path, bytes).unwrap();
        assert!(matches!(Journal::open(dir.path()), Err(Error::Integrity(_))));
    }
    #[test] fn corrupted_length_is_not_mistaken_for_incomplete_tail() {
        let dir = tempfile::tempdir().unwrap(); { Journal::open(dir.path()).unwrap().submit(submission("x")).unwrap(); }
        let path = dir.path().join("jobs.avwj"); let mut bytes = std::fs::read(&path).unwrap(); let length = bytes.len();
        bytes[8] ^= 1; std::fs::write(&path, bytes).unwrap();
        assert!(matches!(Journal::open(dir.path()), Err(Error::Integrity(_))));
        assert_eq!(std::fs::metadata(path).unwrap().len(), length as u64);
    }
    #[test] fn events_are_cursor_paged_without_prompt_text() {
        let dir = tempfile::tempdir().unwrap(); let mut j = Journal::open(dir.path()).unwrap(); let id = j.submit(submission("x")).unwrap().job.id; j.claim_next().unwrap(); j.finish(&id, Ok(output())).unwrap();
        let page = j.events(&id, 0, 2).unwrap(); assert!(page.has_more); assert_eq!(page.items.len(), 2); let tail = j.events(&id, page.next_after, 2).unwrap(); assert!(!tail.has_more); assert_eq!(tail.items.len(), 1);
    }
    #[test] fn claims_fifo() { let dir = tempfile::tempdir().unwrap(); let mut j = Journal::open(dir.path()).unwrap(); let a = j.submit(submission("a")).unwrap(); j.submit(submission("b")).unwrap(); assert_eq!(j.claim_next().unwrap().unwrap().info.id, a.job.id); }
}
