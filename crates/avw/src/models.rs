//! Local checkpoints only. No Python, remote-code execution or implicit model substitution.
use avw_core::{Error, Result, artifacts::{atomic_create, read_bounded}, types::{Device, ModelId, Task}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs::File, io::Read, path::{Component, Path, PathBuf}};

pub const MODEL_IDS: [ModelId; 5] = [ModelId::Voxcpm2, ModelId::Qwen3Asr, ModelId::Qwen3ForcedAligner, ModelId::MossTts15, ModelId::MossTranscribeDiarize];
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelFile { pub path: String, pub bytes: u64, pub sha256: String }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest { pub schema_version: u32, pub id: ModelId, pub directory: PathBuf, pub revision: String, pub files: Vec<ModelFile> }
#[derive(Debug, Clone)]
pub struct Registry { root: PathBuf }
impl Registry {
    pub fn new(workspace: &Path) -> Self { Self { root: workspace.join("models") } }
    fn path(&self, id: ModelId) -> PathBuf { self.root.join(format!("{id}.json")) }
    pub fn get(&self, id: ModelId) -> Result<ModelManifest> {
        let path = self.path(id);
        if path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) { return Err(Error::Integrity("model manifests cannot be symlinks".into())); }
        let data = read_bounded(&path, 2 * 1024 * 1024).map_err(|e| match e { Error::Io(e) if e.kind() == std::io::ErrorKind::NotFound => Error::NotFound(format!("model {id} is not registered")), e => e })?;
        let manifest: ModelManifest = serde_json::from_slice(&data)?;
        if manifest.schema_version != 1 || manifest.id != id || !manifest.directory.is_absolute() { return Err(Error::Integrity("invalid model manifest".into())); }
        validate_revision(&manifest.revision)?;
        if manifest.files.is_empty() || manifest.files.len() > 1024 { return Err(Error::Integrity("invalid model file count".into())); }
        Ok(manifest)
    }
    pub fn add(&self, id: ModelId, directory: &Path, revision: String) -> Result<ModelManifest> {
        validate_revision(&revision)?;
        if matches!(id, ModelId::MossTts15 | ModelId::MossTranscribeDiarize) { return Err(Error::Unsupported("MOSS native graphs are not implemented in this release".into())); }
        let directory = directory.canonicalize()?;
        if !directory.is_dir() { return Err(Error::Invalid("checkpoint must be a directory".into())); }
        let files = scan(&directory)?;
        validate_layout(id, &directory, &files)?;
        let manifest = ModelManifest { schema_version: 1, id, directory, revision, files };
        std::fs::create_dir_all(&self.root)?;
        let encoded = serde_json::to_vec_pretty(&manifest)?;
        if !atomic_create(&self.path(id), &encoded)? { return Err(Error::Conflict(format!("{id} is already registered; use another workspace to change immutable model registration"))); }
        Ok(manifest)
    }
    /// Re-hash immediately before a cold model load. Cached model weights are not re-read.
    pub fn verify(&self, id: ModelId) -> Result<ModelManifest> {
        let manifest = self.get(id)?;
        if manifest.directory.canonicalize()? != manifest.directory { return Err(Error::Integrity("checkpoint directory moved or became a symlink".into())); }
        let current = scan(&manifest.directory)?;
        if serde_json::to_vec(&current)? != serde_json::to_vec(&manifest.files)? { return Err(Error::Integrity("checkpoint files differ from registration; refusing model load".into())); }
        validate_layout(id, &manifest.directory, &current)?;
        Ok(manifest)
    }
    pub fn capabilities(&self, device: Device) -> serde_json::Value {
        let models: Vec<_> = MODEL_IDS.iter().map(|id| {
            let support = ensure_supported(*id, device);
            let registered = self.get(*id).map(|m| serde_json::json!({"revision":m.revision,"file_count":m.files.len()}));
            serde_json::json!({"id":id,"effective_device":effective_device(*id,device),"adapter_compiled":support.is_ok(),"registration":registered.ok(),
                "reason":support.err().map(|e| e.to_string()),"model_inference_validated":false})
        }).collect();
        serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"device":device,"models":models,
            "operations":["prepare","render","export_subtitles"],"audio_formats":["wav"],"output":"mono float32 WAV",
            "cancellation":"cooperative; model loading and individual kernels are not preemptible",
            "asr_timing":"audio chunk boundaries; forced alignment is a separate job",
            "limits":{"request_bytes":1048576,"audio_bytes":134217728,"decoded_mono_samples":48000000,"pending_jobs":256},
            "not_implemented":["moss_tts15","moss_transcribe_diarize","realtime_microphone","neural_voice_activity_detection","hard_process_isolation","loudness_lufs","time_stretch"]})
    }
}
pub fn task_model(task: &Task) -> Option<ModelId> {
    match task { Task::Synthesize { model, .. } | Task::Transcribe { model, .. } | Task::Align { model, .. } => Some(*model), _ => None }
}
/// An explicit compile-feature routing policy, not a successful hardware probe.
/// GPU initialization failures are errors, never silently converted into CPU jobs.
pub fn effective_device(id: ModelId, requested: Device) -> Device {
    if requested != Device::Auto { return requested; }
    match id {
        ModelId::Voxcpm2 if cfg!(feature="wgpu") => Device::Wgpu,
        ModelId::Qwen3Asr | ModelId::Qwen3ForcedAligner if cfg!(all(feature="metal", target_os="macos")) => Device::Metal,
        _ => Device::Cpu,
    }
}
pub fn ensure_supported(id: ModelId, device: Device) -> Result<()> {
    let device = effective_device(id, device);
    let supported = match id {
        ModelId::Voxcpm2 => cfg!(feature="native-voxcpm") && (device == Device::Cpu || (device == Device::Wgpu && cfg!(feature="wgpu"))),
        ModelId::Qwen3Asr | ModelId::Qwen3ForcedAligner => cfg!(feature="native-qwen") && (device == Device::Cpu || (device == Device::Metal && cfg!(all(feature="metal", target_os="macos")))),
        ModelId::MossTts15 | ModelId::MossTranscribeDiarize => false,
    };
    if supported { Ok(()) } else { Err(Error::Unsupported(format!("{id} on {device:?} is unavailable in this build; inspect capabilities"))) }
}
fn validate_revision(revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) { return Err(Error::Invalid("revision must be the checkpoint's 40-character lowercase commit hash, not main".into())); }
    Ok(())
}
fn scan(root: &Path) -> Result<Vec<ModelFile>> {
    let mut files = vec![];
    for entry in std::fs::read_dir(root)? {
        let entry = entry?; let ty = entry.file_type()?;
        let name = entry.file_name().into_string().map_err(|_| Error::Invalid("model filenames must be UTF-8".into()))?;
        let supported = name.ends_with(".json") || name.ends_with(".safetensors") || name.ends_with(".model") || name.ends_with(".pth") || name.ends_with(".pt");
        if !supported { continue; }
        if !ty.is_file() || ty.is_symlink() { return Err(Error::Invalid(format!("model data must be regular files, not links: {name}"))); }
        if files.len() >= 1024 { return Err(Error::Capacity("too many checkpoint files".into())); }
        let mut file = File::open(entry.path())?;
        let before = file.metadata()?.len();
        if before > 64 * 1024 * 1024 * 1024 { return Err(Error::Capacity("checkpoint file exceeds 64 GiB".into())); }
        let mut hash = Sha256::new(); let mut buf = vec![0u8; 1024 * 1024]; let mut total = 0;
        loop { let n = file.read(&mut buf)?; if n == 0 { break; } total += n as u64; if total > before { return Err(Error::Integrity("checkpoint changed while hashing".into())); } hash.update(&buf[..n]); }
        if total != before { return Err(Error::Integrity("checkpoint changed while hashing".into())); }
        files.push(ModelFile { path: name, bytes: total, sha256: hex::encode(hash.finalize()) });
    }
    files.sort_by(|a,b| a.path.cmp(&b.path)); Ok(files)
}
fn validate_layout(id: ModelId, root: &Path, files: &[ModelFile]) -> Result<()> {
    let names: BTreeSet<_> = files.iter().map(|f| f.path.as_str()).collect();
    for name in ["config.json", "tokenizer.json"] {
        if !names.contains(name) { return Err(Error::Invalid(format!("checkpoint missing {name}"))); }
    }
    if id == ModelId::Voxcpm2 {
        if !["model.safetensors", "model.pth", "model.pt"].iter().any(|n| names.contains(n)) {
            return Err(Error::Invalid("VoxCPM2 requires model.safetensors, model.pth or model.pt".into()));
        }
        if !["audiovae.safetensors", "audiovae.pth"].iter().any(|n| names.contains(n)) {
            return Err(Error::Invalid("VoxCPM2 requires audiovae.safetensors or audiovae.pth; random VAE fallback is forbidden".into()));
        }
    } else if names.contains("model.safetensors.index.json") {
        let index: serde_json::Value = serde_json::from_slice(&read_bounded(&root.join("model.safetensors.index.json"), 16 * 1024 * 1024)?)?;
        let map = index.get("weight_map").and_then(|v| v.as_object()).ok_or_else(|| Error::Invalid("weight index is missing weight_map".into()))?;
        if map.is_empty() { return Err(Error::Invalid("weight_map is empty".into())); }
        for value in map.values() {
            let name = value.as_str().ok_or_else(|| Error::Invalid("shard filename must be a string".into()))?;
            if !matches!(Path::new(name).components().next(), Some(Component::Normal(_))) || Path::new(name).components().count() != 1 || !name.ends_with(".safetensors") || !names.contains(name) { return Err(Error::Invalid(format!("missing or unsafe shard: {name}"))); }
        }
    } else if !names.contains("model.safetensors") { return Err(Error::Invalid("checkpoint missing model.safetensors or complete shard index".into())); }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn no_moss_substitution() { assert!(ensure_supported(ModelId::MossTts15, Device::Cpu).is_err()); }
    #[test] fn rejects_floating_revision() { assert!(validate_revision("main").is_err()); }
    #[test] fn accepts_fixed_revision() { assert!(validate_revision(&"a".repeat(40)).is_ok()); }
}
