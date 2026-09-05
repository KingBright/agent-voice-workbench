//! One model-owning worker, a durable FIFO, and cancellation under one mutex.
use crate::{Error, Result, artifacts::Artifacts, control::Control, journal::{ExecutionResult, Journal, JobInfo, EventView, Page, Submitted}, types::{Submission, Task}};
use std::{path::Path, sync::{Arc, Condvar, Mutex, MutexGuard}, thread::{self, JoinHandle}, time::Duration};

pub trait Executor {
    fn execute(&mut self, task: &Task, assets: &Artifacts, control: &Control) -> Result<ExecutionResult>;
}
struct State { journal: Journal, active: Option<(String, Control)>, stopping: bool, fatal: Option<String> }
struct Shared { state: Mutex<State>, wake: Condvar, assets: Artifacts }
/// Do not drop without shutdown: the owner must cancel and join its worker.
pub struct Runtime { shared: Arc<Shared>, worker: Mutex<Option<JoinHandle<()>>> }
impl Runtime {
    pub fn open<F>(root: &Path, factory: F) -> Result<Self>
    where F: FnOnce() -> Box<dyn Executor> + Send + 'static {
        let assets = Artifacts::open(root)?;
        let mut journal = Journal::open(assets.root())?;
        journal.recover_interrupted()?;
        let shared = Arc::new(Shared { state: Mutex::new(State { journal, active: None, stopping: false, fatal: None }), wake: Condvar::new(), assets });
        let worker_shared = shared.clone();
        let worker = thread::Builder::new().name("avw-model-worker".into()).spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut executor = factory();
                worker_loop(&worker_shared, executor.as_mut())
            }));
            let error = match outcome { Ok(Ok(())) => None, Ok(Err(e)) => Some(e.to_string()), Err(_) => Some("model worker panicked; restart required".into()) };
            if let Some(error) = error {
                if let Ok(mut state) = worker_shared.state.lock() {
                    if let Some((id, _)) = state.active.take() {
                        let _ = state.journal.finish(&id, Err(Error::Internal(error.clone())));
                    }
                    state.fatal = Some(error);
                }
                worker_shared.wake.notify_all();
            }
        })?;
        Ok(Self { shared, worker: Mutex::new(Some(worker)) })
    }
    fn state(&self) -> Result<MutexGuard<'_, State>> { self.shared.state.lock().map_err(|_| Error::Internal("runtime mutex poisoned".into())) }
    pub fn artifacts(&self) -> &Artifacts { &self.shared.assets }
    pub fn health(&self) -> Result<()> {
        let state = self.state()?;
        if let Some(error) = &state.fatal { return Err(Error::Internal(error.clone())); }
        if state.stopping { return Err(Error::Conflict("worker is shutting down".into())); }
        Ok(())
    }
    pub fn submit(&self, submission: Submission) -> Result<Submitted> {
        submission.validate()?;
        for id in submission.task.input_assets() { self.shared.assets.meta(id)?; }
        let mut state = self.state()?;
        if let Some(error) = &state.fatal { return Err(Error::Internal(error.clone())); }
        if state.stopping { return Err(Error::Conflict("worker is shutting down".into())); }
        let result = state.journal.submit(submission)?;
        self.shared.wake.notify_one();
        Ok(result)
    }
    pub fn get(&self, id: &str) -> Result<JobInfo> { self.state()?.journal.get(id) }
    pub fn list(&self, after: u64, limit: usize) -> Result<Page<JobInfo>> { Ok(self.state()?.journal.list(after, limit)) }
    pub fn events(&self, id: &str, after: u64, limit: usize) -> Result<Page<EventView>> { self.state()?.journal.events(id, after, limit) }
    pub fn cancel(&self, id: &str) -> Result<JobInfo> {
        let mut state = self.state()?;
        let info = state.journal.cancel(id)?;
        if let Some((active, control)) = &state.active { if active == id { control.cancel(); } }
        self.shared.wake.notify_all();
        Ok(info)
    }
    pub fn wait(&self, id: &str) -> Result<JobInfo> {
        let mut state = self.state()?;
        loop {
            let info = state.journal.get(id)?;
            if info.state.terminal() { return Ok(info); }
            if let Some(error) = &state.fatal { return Err(Error::Internal(error.clone())); }
            if state.stopping { return Err(Error::Conflict("worker is stopping".into())); }
            let (next, _) = self.shared.wake.wait_timeout(state, Duration::from_millis(200)).map_err(|_| Error::Internal("runtime mutex poisoned".into()))?;
            state = next;
        }
    }
    pub fn shutdown(&self) -> Result<()> {
        {
            let mut state = self.state()?;
            state.stopping = true;
            if let Some((id, control)) = state.active.clone() { control.cancel(); state.journal.cancel(&id)?; }
            self.shared.wake.notify_all();
        }
        if let Some(worker) = self.worker.lock().map_err(|_| Error::Internal("worker handle poisoned".into()))?.take() {
            worker.join().map_err(|_| Error::Internal("worker join failed".into()))?;
        }
        Ok(())
    }
}
impl Drop for Runtime { fn drop(&mut self) { let _ = self.shutdown(); } }
fn worker_loop(shared: &Shared, executor: &mut dyn Executor) -> Result<()> {
    loop {
        let (job, control) = {
            let mut state = shared.state.lock().map_err(|_| Error::Internal("runtime mutex poisoned".into()))?;
            loop {
                if state.stopping { return Ok(()); }
                if let Some(job) = state.journal.claim_next()? {
                    let control = Control::new(job.info.deadline_ms);
                    state.active = Some((job.info.id.clone(), control.clone()));
                    break (job, control);
                }
                let (next, _) = shared.wake.wait_timeout(state, Duration::from_millis(250)).map_err(|_| Error::Internal("runtime mutex poisoned".into()))?;
                state = next;
            }
        };
        let outcome = control.check().and_then(|_| executor.execute(&job.submission.task, &shared.assets, &control));
        // Successful model completion after the deadline is not a successful job.
        let outcome = control.check().and(outcome);
        let mut state = shared.state.lock().map_err(|_| Error::Internal("runtime mutex poisoned".into()))?;
        state.journal.finish(&job.info.id, outcome)?;
        state.active = None;
        shared.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{journal::JobState, types::{ModelId, TtsOptions}};
    struct TestExecutor;
    impl Executor for TestExecutor {
        fn execute(&mut self, _: &Task, _: &Artifacts, control: &Control) -> Result<ExecutionResult> {
            for _ in 0..10 { control.check()?; thread::sleep(Duration::from_millis(2)); }
            Ok(ExecutionResult { artifacts: vec![], summary: serde_json::json!({"test_only":true}), warnings: vec![] })
        }
    }
    fn request() -> Submission { Submission { task: Task::Synthesize { model: ModelId::Voxcpm2, text:"test".into(), reference:None, reference_rights:None, options:TtsOptions::default() }, idempotency_key:None, timeout_secs:30 } }
    #[test] fn runs_without_polling_executor_from_client() {
        let temp = tempfile::tempdir().unwrap(); let rt = Runtime::open(temp.path(), || Box::new(TestExecutor)).unwrap();
        let id = rt.submit(request()).unwrap().job.id; assert_eq!(rt.wait(&id).unwrap().state, JobState::Succeeded); rt.shutdown().unwrap();
    }
    #[test] fn cancellation_wins() {
        let temp = tempfile::tempdir().unwrap(); let rt = Runtime::open(temp.path(), || Box::new(TestExecutor)).unwrap();
        let id = rt.submit(request()).unwrap().job.id; rt.cancel(&id).unwrap(); assert_eq!(rt.wait(&id).unwrap().state, JobState::Cancelled);
    }
}
