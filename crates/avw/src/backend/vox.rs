use avw_core::{Error, Result, artifacts::read_bounded, audio::Audio, control::Control, types::TtsOptions};
use burn::prelude::Backend;
use voxcpm_rs::{VoxCPM, Prompt, PromptAudio, GenerateOptions, CancelToken};
use std::{path::Path, sync::{Arc, atomic::{AtomicBool, Ordering}}, thread, time::Duration};

/// Unlike upstream from_local, fail closed on missing parameters or load errors.
pub fn load<B: Backend>(directory: &Path) -> Result<VoxCPM<B>> where B::Device: Default {
    let config = serde_json::from_slice(&read_bounded(&directory.join("config.json"), 4 * 1024 * 1024)?)?;
    let tokenizer = voxcpm_rs::tokenizer::TextTokenizer::from_local(directory).map_err(model_error)?;
    let device = B::Device::default();
    let mut model = VoxCPM::<B>::from_config(config, tokenizer, &device);
    let receipt = voxcpm_rs::weights::load_pretrained(&mut model.model, directory).map_err(model_error)?;
    if receipt.applied.is_empty() || !receipt.missing.is_empty() || !receipt.errors.is_empty() {
        return Err(Error::Model(format!("incomplete VoxCPM2 weights: applied={}, missing={}, errors={}", receipt.applied.len(), receipt.missing.len(), receipt.errors.len())));
    }
    Ok(model)
}
fn model_error(error: impl std::fmt::Display) -> Error { Error::Model(error.to_string()) }
struct Watchdog { done: Arc<AtomicBool>, thread: Option<thread::JoinHandle<()>> }
impl Drop for Watchdog {
    fn drop(&mut self) { self.done.store(true, Ordering::Release); if let Some(t) = self.thread.take() { let _ = t.join(); } }
}
pub fn generate<B: Backend>(model: &VoxCPM<B>, text: &str, reference: Option<Audio>, options: &TtsOptions, control: &Control) -> Result<Audio> {
    control.check()?;
    let cancel = CancelToken::new(); let done = Arc::new(AtomicBool::new(false));
    let worker_done = done.clone(); let worker_cancel = cancel.clone(); let worker_control = control.clone();
    let thread = thread::Builder::new().name("avw-tts-cancel".into()).spawn(move || {
        while !worker_done.load(Ordering::Acquire) {
            if worker_control.check().is_err() { worker_cancel.cancel(); break; }
            thread::sleep(Duration::from_millis(20));
        }
    })?;
    let _watchdog = Watchdog { done, thread: Some(thread) };
    let prompt = reference.map_or(Prompt::None, |audio| Prompt::Reference { audio: PromptAudio::Pcm { samples:audio.samples, sample_rate:audio.sample_rate } });
    let generated = model.generate(text, GenerateOptions {
        cfg_value:options.guidance, inference_timesteps:options.steps, min_len:2, max_len:options.max_patches,
        prompt, cancel:Some(cancel), chunk_patches:5, parallel_segments:None,
    });
    control.check()?;
    Audio::new(model.sample_rate(), generated.map_err(model_error)?)
}
