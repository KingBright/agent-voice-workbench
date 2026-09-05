use avw_core::{Error, Result, audio::Audio, control::Control, transcript::{Transcript, Segment, TimingSource}, types::{AssetId, Device}};
use qwen3_asr::{AudioInput, Batch, TranscribeOptions, Qwen3Asr, forced_aligner::Qwen3ForcedAligner};

pub fn device(device: Device) -> Result<candle_core::Device> {
    match device {
        Device::Cpu => Ok(candle_core::Device::Cpu),
        #[cfg(all(feature="metal", target_os="macos"))]
        Device::Metal => candle_core::Device::new_metal(0).map_err(|e| Error::Model(e.to_string())),
        _ => Err(Error::Unsupported("Qwen backend requires CPU or a Metal-enabled macOS build".into())),
    }
}
/// Explicit chunk-level ASR timing. We never fabricate word timing or speakers.
pub fn transcribe(model: &Qwen3Asr, audio: &Audio, source: AssetId, language: Option<String>, control: &Control) -> Result<Transcript> {
    if audio.sample_rate != 16000 { return Err(Error::Invalid("ASR requires pre-resampled 16k PCM".into())); }
    let mut segments = vec![]; let mut detected = language.clone();
    for (index, chunk) in audio.samples.chunks(20 * 16000).enumerate() {
        control.check()?;
        let output = model.transcribe(vec![AudioInput::Waveform { samples:chunk, sample_rate:16000 }], TranscribeOptions {
            language:Batch::one(language.clone()), max_new_tokens:512, max_batch_size:1, return_timestamps:false, ..Default::default()
        }).map_err(|e| Error::Model(e.to_string()))?;
        control.check()?;
        if output.len() != 1 { return Err(Error::Model("ASR returned an unexpected batch length".into())); }
        let item = output.into_iter().next().ok_or_else(|| Error::Model("empty ASR result".into()))?;
        if detected.is_none() && !item.language.is_empty() { detected = Some(item.language); }
        if !item.text.trim().is_empty() {
            let start_ms = index as u64 * 20_000;
            let end_ms = start_ms + ((chunk.len() as u64 * 1000).div_ceil(16000));
            segments.push(Segment { start_ms, end_ms, text:item.text, speaker:None });
        }
    }
    let text = segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n");
    let result = Transcript { schema_version:1, source_audio:Some(source), language:detected, text, timing_source:TimingSource::AudioChunks, segments };
    result.validate()?; Ok(result)
}
pub fn align(model: &Qwen3ForcedAligner, audio: &Audio, source: AssetId, text: &str, language: &str, control: &Control) -> Result<Transcript> {
    control.check()?;
    let mut output = model.align(&[AudioInput::Waveform { samples:&audio.samples, sample_rate:audio.sample_rate }], &[text.to_owned()], &[language.to_owned()]).map_err(|e| Error::Model(e.to_string()))?;
    control.check()?;
    if output.len() != 1 { return Err(Error::Model("aligner returned an unexpected batch length".into())); }
    let result = output.pop().ok_or_else(|| Error::Model("empty alignment result".into()))?;
    let duration = audio.samples.len() as f64 / audio.sample_rate as f64;
    let mut segments = Vec::with_capacity(result.items.len());
    for word in result.items {
        if !word.start_time.is_finite() || !word.end_time.is_finite() || word.start_time < 0.0 || word.end_time <= word.start_time || word.end_time > duration + 0.1 {
            return Err(Error::Model("aligner emitted invalid/out-of-range word timing; refusing to invent replacement timestamps".into()));
        }
        segments.push(Segment { start_ms:(word.start_time * 1000.0).floor() as u64, end_ms:(word.end_time * 1000.0).ceil() as u64, text:word.text, speaker:None });
    }
    if segments.is_empty() { return Err(Error::Model("aligner emitted no words".into())); }
    let transcript = Transcript { schema_version:1, source_audio:Some(source), language:Some(language.into()), text:text.into(), timing_source:TimingSource::ForcedAligner, segments };
    transcript.validate()?; Ok(transcript)
}
