use crate::{backend::NativeExecutor, models::{Registry, ensure_supported, task_model}};
use avw_core::{Error, Result, runtime::Runtime, types::{AssetId, Device, Submission}};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use schemars::{JsonSchema, schema_for};
use std::{path::Path, sync::Arc};

#[derive(Clone)]
pub struct App { pub runtime: Arc<Runtime>, pub registry: Registry, pub device: Device }
impl App {
    pub fn open(root: &Path, device: Device) -> Result<Self> {
        let registry = Registry::new(root); let worker_registry = registry.clone();
        let runtime = Arc::new(Runtime::open(root, move || Box::new(NativeExecutor::new(worker_registry, device)))?);
        Ok(Self { runtime, registry, device })
    }
    pub fn submit(&self, submission: Submission) -> Result<avw_core::journal::Submitted> {
        submission.validate()?;
        if let Some(model) = task_model(&submission.task) { ensure_supported(model, self.device)?; self.registry.get(model)?; }
        self.runtime.submit(submission)
    }
    pub fn dispatch(&self, name: &str, arguments: Value) -> Result<Value> {
        match name {
            "capabilities" => { parse::<Empty>(arguments)?; Ok(self.registry.capabilities(self.device)) }
            "jobs_submit" => value(self.submit(parse(arguments)?)?),
            "jobs_get" => { let args: JobArgs = parse(arguments)?; value(self.runtime.get(&args.job_id)?) }
            "jobs_cancel" => { let args: JobArgs = parse(arguments)?; value(self.runtime.cancel(&args.job_id)?) }
            "jobs_list" => { let args: Paging = parse(arguments)?; value(self.runtime.list(args.after, args.limit)?) }
            "jobs_events" => { let args: EventsArgs = parse(arguments)?; value(self.runtime.events(&args.job_id, args.after, args.limit)?) }
            "audio_import" => { let args: ImportArgs = parse(arguments)?; value(self.runtime.artifacts().import_inbox(&args.path)?) }
            "assets_get" => { let args: AssetArgs = parse(arguments)?; value(self.runtime.artifacts().meta(&args.asset_id)?) }
            "transcript_read" => {
                let args: TranscriptArgs = parse(arguments)?;
                let transcript = self.runtime.artifacts().transcript(&args.asset_id)?;
                let start = args.offset.min(transcript.segments.len()); let end = (start + args.limit.clamp(1, 100)).min(transcript.segments.len());
                Ok(json!({"asset_id":args.asset_id,"language":transcript.language,"timing_source":transcript.timing_source,
                    "segments":&transcript.segments[start..end],"total_segments":transcript.segments.len(),"next_offset":end,"has_more":end < transcript.segments.len()}))
            }
            _ => Err(Error::Unsupported(format!("unknown tool: {name}"))),
        }
    }
}
fn value<T: Serialize>(v: T) -> Result<Value> { Ok(serde_json::to_value(v)?) }
fn parse<T: serde::de::DeserializeOwned>(v: Value) -> Result<T> { serde_json::from_value(v).map_err(|e| Error::Invalid(e.to_string())) }
#[derive(Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct Empty {}
#[derive(Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct JobArgs { pub job_id: String }
#[derive(Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct AssetArgs { pub asset_id: AssetId }
#[derive(Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct ImportArgs { pub path: String }
#[derive(Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct Paging { #[serde(default)] pub after: u64, #[serde(default="page_size")] pub limit: usize }
#[derive(Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct EventsArgs { pub job_id:String, #[serde(default)] pub after:u64, #[serde(default="page_size")] pub limit:usize }
#[derive(Deserialize, JsonSchema)] #[serde(deny_unknown_fields)] pub struct TranscriptArgs { pub asset_id:AssetId, #[serde(default)] pub offset:usize, #[serde(default="page_size")] pub limit:usize }
fn page_size() -> usize { 20 }
fn tool<T: JsonSchema>(name: &str, description: &str, read_only: bool) -> Value {
    json!({"name":name,"description":description,"inputSchema":schema_for!(T),"annotations":{"readOnlyHint":read_only,"destructiveHint":false,"openWorldHint":false}})
}
pub fn tools() -> Value {
    json!({"tools":[
        tool::<Empty>("capabilities", "Inspect compiled adapters, registered checkpoints, device support and limitations before requesting inference.", true),
        tool::<Submission>("jobs_submit", "Submit a durable audio job. Reuse an idempotency_key only with the identical task and timeout. Returns job ID, never audio bytes.", false),
        tool::<JobArgs>("jobs_get", "Read compact job state, result asset IDs and structured failures.", true),
        tool::<JobArgs>("jobs_cancel", "Request cooperative cancellation; poll until terminal. Loading and kernels cannot be interrupted immediately.", false),
        tool::<Paging>("jobs_list", "List jobs in submission order; use next_after to avoid repeating history.", true),
        tool::<EventsArgs>("jobs_events", "Read only events after a cursor; payloads omit the original prompt.", true),
        tool::<ImportArgs>("audio_import", "Import a WAV relative to workspace/inbox. Absolute paths, symlink escapes and path traversal are rejected.", false),
        tool::<AssetArgs>("assets_get", "Read audio/transcript/subtitle metadata by SHA-256 asset ID.", true),
        tool::<TranscriptArgs>("transcript_read", "Read a bounded page of transcript segments. Audio chunk timing is not word alignment.", true)
    ]})
}

#[cfg(test)]
mod tests {
    use super::*;
    use avw_core::{audio::Audio, journal::JobState, types::Task, transcript::{Transcript,Segment,TimingSource}};
    #[test] fn real_dsp_roundtrip_through_durable_worker() {
        let d=tempfile::tempdir().unwrap(); let app=App::open(d.path(),Device::Cpu).unwrap();
        let source=app.runtime.artifacts().put_audio(&Audio::new(16000,vec![0.25;1600]).unwrap()).unwrap();
        let job=app.submit(Submission { task:Task::Prepare { audio:source.id,sample_rate:Some(8000),trim_start_ms:0,trim_end_ms:None,peak_dbfs:Some(-6.0),fade_ms:1 },idempotency_key:Some("dsp-e2e".into()),timeout_secs:30 }).unwrap();
        let done=app.runtime.wait(&job.job.id).unwrap(); assert_eq!(done.state,JobState::Succeeded);
        let output=app.runtime.artifacts().audio(&done.result.unwrap().artifacts[0].id).unwrap();
        assert_eq!(output.sample_rate,8000); assert_eq!(output.samples.len(),800); assert!((output.stats().peak_dbfs.unwrap()+6.0).abs()<0.01);
    }
    #[test] fn unsupported_model_is_rejected_before_queueing() {
        let d=tempfile::tempdir().unwrap(); let app=App::open(d.path(),Device::Cpu).unwrap();
        let request=json!({"task":{"kind":"synthesize","model":"moss_tts15","text":"你好"},"timeout_secs":30});
        assert!(app.dispatch("jobs_submit",request).is_err()); assert!(app.runtime.list(0,20).unwrap().items.is_empty());
    }
    #[test] fn actual_subtitle_export_job() {
        let d=tempfile::tempdir().unwrap(); let app=App::open(d.path(),Device::Cpu).unwrap();
        let tr=Transcript { schema_version:1,source_audio:None,language:Some("Chinese".into()),text:"你好".into(),timing_source:TimingSource::Imported,segments:vec![Segment { start_ms:100,end_ms:900,text:"你好".into(),speaker:Some("S01".into()) }] };
        let asset=app.runtime.artifacts().put_transcript(&tr).unwrap();
        let request=Submission { task:Task::ExportSubtitles { transcript:asset.id,format:avw_core::types::SubtitleFormat::Srt },idempotency_key:None,timeout_secs:30 };
        let id=app.submit(request).unwrap().job.id; let result=app.runtime.wait(&id).unwrap().result.unwrap();
        let (_,bytes)=app.runtime.artifacts().read(&result.artifacts[0].id).unwrap(); assert!(String::from_utf8(bytes).unwrap().contains("00:00:00,100 --> 00:00:00,900"));
    }
}
