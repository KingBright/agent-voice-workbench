use avw_core::{Error, Result, artifacts::{Artifacts, AssetMeta}, audio::render, control::Control, journal::ExecutionResult, runtime::Executor, types::{Task, Device, ModelId}};
use crate::models::{Registry, ensure_supported, effective_device, task_model};
#[cfg(feature="native-voxcpm")] mod vox;
#[cfg(feature="native-qwen")] mod qwen;

// Only one neural model is resident at a time. DSP jobs leave the cache intact.
#[cfg(any(feature="native-voxcpm", feature="native-qwen"))]
enum Loaded {
    Empty,
    #[cfg(feature="native-voxcpm")] VoxCpu(Box<voxcpm_rs::VoxCPM<burn::backend::NdArray<f32>>>),
    #[cfg(feature="wgpu")] VoxWgpu(Box<voxcpm_rs::VoxCPM<burn::backend::Wgpu<f32>>>),
    #[cfg(feature="native-qwen")] Asr(Box<qwen3_asr::Qwen3Asr>),
    #[cfg(feature="native-qwen")] Aligner(Box<qwen3_asr::forced_aligner::Qwen3ForcedAligner>),
}
pub struct NativeExecutor { registry: Registry, device: Device, #[cfg(any(feature="native-voxcpm", feature="native-qwen"))] loaded: Loaded, loaded_id: Option<ModelId>, revision: Option<String> }
impl NativeExecutor {
    pub fn new(registry: Registry, device: Device) -> Self { Self { registry, device, #[cfg(any(feature="native-voxcpm", feature="native-qwen"))] loaded: Loaded::Empty, loaded_id: None, revision: None } }
    #[cfg(not(any(feature="native-voxcpm", feature="native-qwen")))]
    fn ensure_loaded(&mut self, id: ModelId, control: &Control) -> Result<()> { control.check()?; ensure_supported(id, self.device) }
    #[cfg(any(feature="native-voxcpm", feature="native-qwen"))]
    fn ensure_loaded(&mut self, id: ModelId, control: &Control) -> Result<()> {
        ensure_supported(id, self.device)?; control.check()?;
        if self.loaded_id == Some(id) { return Ok(()); }
        self.loaded = Loaded::Empty; self.loaded_id = None; self.revision = None;
        let manifest = self.registry.verify(id)?; control.check()?;
        let device = effective_device(id, self.device);
        match id {
            #[cfg(feature="native-voxcpm")]
            ModelId::Voxcpm2 => {
                self.loaded = match device {
                    Device::Cpu => Loaded::VoxCpu(Box::new(vox::load::<burn::backend::NdArray<f32>>(&manifest.directory)?)),
                    #[cfg(feature="wgpu")]
                    Device::Wgpu => Loaded::VoxWgpu(Box::new(vox::load::<burn::backend::Wgpu<f32>>(&manifest.directory)?)),
                    _ => return Err(Error::Unsupported("VoxCPM device not compiled".into())),
                };
            }
            #[cfg(feature="native-qwen")]
            ModelId::Qwen3Asr | ModelId::Qwen3ForcedAligner => {
                let device = qwen::device(device)?;
                let path = manifest.directory.to_str().ok_or_else(|| Error::Invalid("checkpoint path must be UTF-8".into()))?;
                let options = qwen3_asr::LoadOptions::default();
                self.loaded = if id == ModelId::Qwen3Asr {
                    Loaded::Asr(Box::new(qwen3_asr::Qwen3Asr::from_pretrained(path, &device, &options).map_err(|e| Error::Model(e.to_string()))?))
                } else {
                    Loaded::Aligner(Box::new(qwen3_asr::forced_aligner::Qwen3ForcedAligner::from_pretrained(path, &device, &options).map_err(|e| Error::Model(e.to_string()))?))
                };
            }
            _ => return Err(Error::Unsupported(format!("no native graph for {id}"))),
        }
        self.loaded_id = Some(id); self.revision = Some(manifest.revision); control.check()
    }
    fn result(&self, meta: AssetMeta, extra: serde_json::Value, warnings: Vec<String>, model_task: bool) -> ExecutionResult {
        ExecutionResult { artifacts: vec![meta], summary: serde_json::json!({"details":extra,
            "model":if model_task { self.loaded_id } else { None },
            "checkpoint_revision":if model_task { self.revision.as_deref() } else { None },
            "device":if model_task { self.loaded_id.map(|id| effective_device(id, self.device)).unwrap_or(Device::Cpu) } else { Device::Cpu }}), warnings }
    }
}
impl Executor for NativeExecutor {
    fn execute(&mut self, task: &Task, assets: &Artifacts, control: &Control) -> Result<ExecutionResult> {
        task.validate()?; control.check()?;
        if let Some(model) = task_model(task) { self.ensure_loaded(model, control)?; }
        match task {
            Task::Prepare { audio, sample_rate, trim_start_ms, trim_end_ms, peak_dbfs, fade_ms } => {
                let mut audio = assets.audio(audio)?.trim(*trim_start_ms, *trim_end_ms)?;
                if let Some(rate) = sample_rate { audio = audio.resample(*rate, control)?; }
                audio.fade(*fade_ms, *fade_ms)?;
                if let Some(peak) = peak_dbfs { audio.normalize_peak(*peak)?; }
                control.check()?; let stats = audio.stats(); let meta = assets.put_audio(&audio)?;
                Ok(self.result(meta, serde_json::to_value(stats)?, vec![], false))
            }
            Task::Render { sample_rate, clips, peak_dbfs } => {
                let audio = render(*sample_rate, clips, *peak_dbfs, control, |id| assets.audio(id))?;
                control.check()?; let stats = audio.stats(); let meta = assets.put_audio(&audio)?;
                let warnings = if stats.peak > 1.0 { vec!["Mix exceeds 0 dBFS; float WAV preserves samples. Set peak_dbfs to prevent clipping in integer playback.".into()] } else { vec![] };
                Ok(self.result(meta, serde_json::to_value(stats)?, warnings, false))
            }
            Task::ExportSubtitles { transcript, format } => {
                let transcript = assets.transcript(transcript)?; let count = transcript.segments.len();
                let meta = assets.put_subtitles(&transcript, *format)?;
                Ok(self.result(meta, serde_json::json!({"cues":count,"timing_source":transcript.timing_source}), vec![], false))
            }
            Task::Synthesize { text, reference, options, .. } => {
                #[cfg(feature="native-voxcpm")]
                {
                    let reference = reference.as_ref().map(|id| assets.audio(id)).transpose()?;
                    if reference.as_ref().is_some_and(|a| a.stats().duration_ms > 30_000) { return Err(Error::Invalid("reference audio is limited to 30 seconds by workbench policy".into())); }
                    let audio = match &self.loaded {
                        Loaded::VoxCpu(model) => vox::generate(model, text, reference, options, control)?,
                        #[cfg(feature="wgpu")]
                        Loaded::VoxWgpu(model) => vox::generate(model, text, reference, options, control)?,
                        _ => return Err(Error::Internal("TTS model cache mismatch".into())),
                    };
                    control.check()?; let stats = audio.stats(); let meta = assets.put_audio(&audio)?;
                    return Ok(self.result(meta, serde_json::to_value(stats)?, vec!["Sampling is not guaranteed bit-reproducible; no seed support is advertised.".into()], true));
                }
                #[cfg(not(feature="native-voxcpm"))]
                { let _ = (text, reference, options); Err(Error::Unsupported("build with native-voxcpm".into())) }
            }
            Task::Transcribe { audio, language, .. } => {
                #[cfg(feature="native-qwen")]
                {
                    let pcm = assets.audio(audio)?.resample(16000, control)?;
                    let model = match &self.loaded { Loaded::Asr(model) => model, _ => return Err(Error::Internal("ASR cache mismatch".into())) };
                    let transcript = qwen::transcribe(model, &pcm, audio.clone(), language.clone(), control)?;
                    let count = transcript.segments.len(); control.check()?; let meta = assets.put_transcript(&transcript)?;
                    return Ok(self.result(meta, serde_json::json!({"segments":count,"timing_source":"audio_chunks"}), vec!["ASR segment times are 20-second input chunk boundaries, not aligned speech/word timestamps. No diarization is performed.".into()], true));
                }
                #[cfg(not(feature="native-qwen"))]
                { let _ = (audio, language); Err(Error::Unsupported("build with native-qwen".into())) }
            }
            Task::Align { audio, text, language, .. } => {
                #[cfg(feature="native-qwen")]
                {
                    let pcm = assets.audio(audio)?.resample(16000, control)?;
                    if pcm.stats().duration_ms > 30_000 { return Err(Error::Invalid("align each known transcript/audio segment separately; alignment is limited to 30 seconds".into())); }
                    let model = match &self.loaded { Loaded::Aligner(model) => model, _ => return Err(Error::Internal("aligner cache mismatch".into())) };
                    let transcript = qwen::align(model, &pcm, audio.clone(), text, language, control)?;
                    let count = transcript.segments.len(); control.check()?; let meta = assets.put_transcript(&transcript)?;
                    return Ok(self.result(meta, serde_json::json!({"segments":count,"timing_source":"forced_aligner"}), vec![], true));
                }
                #[cfg(not(feature="native-qwen"))]
                { let _ = (audio, text, language); Err(Error::Unsupported("build with native-qwen".into())) }
            }
        }
    }
}
