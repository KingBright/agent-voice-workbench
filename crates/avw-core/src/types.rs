use crate::{Error, Result};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A SHA-256 digest. Deserialization validates it before any filesystem use.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct AssetId(String);
impl TryFrom<String> for AssetId {
    type Error = String;
    fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
        if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("asset ID must be 64 lowercase hexadecimal characters".into());
        }
        Ok(Self(value))
    }
}
impl From<AssetId> for String { fn from(id: AssetId) -> Self { id.0 } }
impl fmt::Display for AssetId { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { self.0.fmt(f) } }
impl AssetId { pub fn as_str(&self) -> &str { &self.0 } }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModelId {
    Voxcpm2,
    MossTts15,
    MossTranscribeDiarize,
    Qwen3Asr,
    Qwen3ForcedAligner,
}
impl ModelId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Voxcpm2 => "voxcpm2", Self::MossTts15 => "moss_tts15",
            Self::MossTranscribeDiarize => "moss_transcribe_diarize",
            Self::Qwen3Asr => "qwen3_asr", Self::Qwen3ForcedAligner => "qwen3_forced_aligner",
        }
    }
}
impl fmt::Display for ModelId { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) } }
impl std::str::FromStr for ModelId {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        serde_json::from_value(serde_json::Value::String(s.to_owned())).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Device { #[default] Cpu, Wgpu, Metal, Auto }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TtsOptions {
    #[serde(default = "default_steps")] pub steps: usize,
    #[serde(default = "default_cfg")] pub guidance: f32,
    #[serde(default = "default_patches")] pub max_patches: usize,
}
fn default_steps() -> usize { 10 }
fn default_cfg() -> f32 { 2.0 }
fn default_patches() -> usize { 750 }
impl Default for TtsOptions { fn default() -> Self { Self { steps: 10, guidance: 2.0, max_patches: 750 } } }

/// Clip placement is expressed in integer milliseconds, never accumulated floats.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Clip {
    pub asset: AssetId,
    #[serde(default)] pub at_ms: u64,
    #[serde(default)] pub trim_start_ms: u64,
    pub trim_end_ms: Option<u64>,
    #[serde(default)] pub gain_db: f32,
    #[serde(default)] pub fade_in_ms: u64,
    #[serde(default)] pub fade_out_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Task {
    Synthesize {
        model: ModelId,
        text: String,
        reference: Option<AssetId>,
        /// Required for cloning; retained in the job's provenance record.
        reference_rights: Option<String>,
        #[serde(default)] options: TtsOptions,
    },
    Transcribe { model: ModelId, audio: AssetId, language: Option<String> },
    Align { model: ModelId, audio: AssetId, text: String, language: String },
    Prepare {
        audio: AssetId,
        sample_rate: Option<u32>,
        #[serde(default)] trim_start_ms: u64,
        trim_end_ms: Option<u64>,
        peak_dbfs: Option<f32>,
        #[serde(default)] fade_ms: u64,
    },
    Render { sample_rate: u32, clips: Vec<Clip>, peak_dbfs: Option<f32> },
    ExportSubtitles { transcript: AssetId, format: SubtitleFormat },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleFormat { Srt, Vtt }

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub task: Task,
    pub idempotency_key: Option<String>,
    #[serde(default = "default_timeout")] pub timeout_secs: u64,
}
fn default_timeout() -> u64 { 600 }

impl Submission {
    pub fn validate(&self) -> Result<()> {
        if !(1..=3600).contains(&self.timeout_secs) { return Err(Error::Invalid("timeout_secs must be 1..=3600".into())); }
        if let Some(k) = &self.idempotency_key {
            if k.is_empty() || k.len() > 128 || k.chars().any(char::is_control) { return Err(Error::Invalid("idempotency_key must be 1..=128 bytes without control characters".into())); }
        }
        self.task.validate()
    }
}
fn text_ok(s: &str) -> Result<()> {
    if s.trim().is_empty() || s.len() > 16_384 || s.contains('\0') { return Err(Error::Invalid("text must be nonempty, NUL-free and at most 16 KiB".into())); }
    Ok(())
}
pub fn check_peak(peak: Option<f32>) -> Result<()> {
    if peak.is_some_and(|v| !v.is_finite() || !(-60.0..=0.0).contains(&v)) { return Err(Error::Invalid("peak_dbfs must be finite and -60..=0".into())); }
    Ok(())
}
pub fn check_rate(rate: u32) -> Result<()> {
    if !(8_000..=192_000).contains(&rate) { return Err(Error::Invalid("sample rate must be 8000..=192000 Hz".into())); }
    Ok(())
}
impl Task {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Synthesize { model, text, reference, reference_rights, options } => {
                if !matches!(model, ModelId::Voxcpm2 | ModelId::MossTts15) { return Err(Error::Invalid("model does not provide TTS".into())); }
                text_ok(text)?;
                if reference_rights.as_ref().is_some_and(|v| v.len() > 1024 || v.contains('\0')) { return Err(Error::Invalid("reference_rights must be NUL-free and at most 1024 bytes".into())); }
                if reference.is_some() && reference_rights.as_ref().is_none_or(|v| v.trim().is_empty() || v.len() > 1024) {
                    return Err(Error::Invalid("reference_rights is required for reference voice audio".into()));
                }
                if !(1..=50).contains(&options.steps) || !(2..=2000).contains(&options.max_patches) || !options.guidance.is_finite() || !(0.1..=10.0).contains(&options.guidance) {
                    return Err(Error::Invalid("invalid TTS generation limits".into()));
                }
            }
            Self::Transcribe { model, language, .. } => {
                if !matches!(model, ModelId::MossTranscribeDiarize | ModelId::Qwen3Asr) { return Err(Error::Invalid("model does not provide ASR".into())); }
                if language.as_ref().is_some_and(|v| v.trim().is_empty() || v.len() > 64 || v.chars().any(char::is_control)) { return Err(Error::Invalid("invalid language".into())); }
            }
            Self::Align { model, text, language, .. } => {
                if *model != ModelId::Qwen3ForcedAligner { return Err(Error::Invalid("model does not provide forced alignment".into())); }
                text_ok(text)?;
                if language.trim().is_empty() || language.len() > 64 || language.chars().any(char::is_control) { return Err(Error::Invalid("invalid language".into())); }
            }
            Self::Prepare { sample_rate, trim_start_ms, trim_end_ms, peak_dbfs, .. } => {
                if let Some(r) = sample_rate { check_rate(*r)?; }
                if trim_end_ms.is_some_and(|end| end <= *trim_start_ms) { return Err(Error::Invalid("trim end must exceed start".into())); }
                check_peak(*peak_dbfs)?;
            }
            Self::Render { sample_rate, clips, peak_dbfs } => {
                check_rate(*sample_rate)?; check_peak(*peak_dbfs)?;
                if clips.is_empty() || clips.len() > 256 { return Err(Error::Invalid("render requires 1..=256 clips".into())); }
                for c in clips {
                    if !c.gain_db.is_finite() || !(-60.0..=24.0).contains(&c.gain_db) { return Err(Error::Invalid("clip gain_db must be -60..=24".into())); }
                    if c.at_ms > 3_600_000 || c.trim_end_ms.is_some_and(|end| end <= c.trim_start_ms) { return Err(Error::Invalid("invalid clip placement or trim".into())); }
                }
            }
            Self::ExportSubtitles { .. } => {}
        }
        Ok(())
    }
    pub fn input_assets(&self) -> Vec<&AssetId> {
        match self {
            Self::Synthesize { reference, .. } => reference.iter().collect(),
            Self::Transcribe { audio, .. } | Self::Align { audio, .. } | Self::Prepare { audio, .. } => vec![audio],
            Self::Render { clips, .. } => clips.iter().map(|c| &c.asset).collect(),
            Self::ExportSubtitles { transcript, .. } => vec![transcript],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn rejects_asset_paths() { for s in ["../secret", "", "/etc/passwd", &"A".repeat(64)] { assert!(AssetId::try_from(s.to_string()).is_err()); } }
    #[test] fn accepts_digest() { assert!(AssetId::try_from("a".repeat(64)).is_ok()); }
    #[test] fn deserializer_validates_asset_ids() { assert!(serde_json::from_str::<AssetId>("\"../secret\"").is_err()); }
    #[test] fn rejects_nonfinite_peak() { assert!(check_peak(Some(f32::NAN)).is_err()); }
    #[test] fn requires_cloning_rights() {
        let task = Task::Synthesize { model: ModelId::Voxcpm2, text: "你好".into(), reference: Some(AssetId::try_from("0".repeat(64)).unwrap()), reference_rights: None, options: TtsOptions::default() };
        assert!(task.validate().is_err());
    }
    #[test] fn rejects_unknown_task_fields() {
        assert!(serde_json::from_str::<Task>(r#"{"kind":"export_subtitles","transcript":"0000000000000000000000000000000000000000000000000000000000000000","format":"srt","shell":"bad"}"#).is_err());
    }
}
