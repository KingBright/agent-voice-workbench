use crate::{Error, Result, audio::{Audio, AudioStats, MAX_AUDIO_BYTES}, control::now_ms, transcript::Transcript, types::{AssetId, SubtitleFormat}};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::{self, File}, io::{Read, Write}, path::{Component, Path, PathBuf}};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind { Audio, Transcript, Subtitle }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetMeta {
    pub id: AssetId,
    pub kind: AssetKind,
    pub media_type: String,
    pub bytes: usize,
    pub created_ms: u64,
    pub audio: Option<AudioStats>,
}
#[derive(Debug, Clone)]
pub struct Artifacts { root: PathBuf }

pub fn digest(bytes: &[u8]) -> String { hex::encode(Sha256::digest(bytes)) }

pub fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut file = File::open(path)?;
    if file.metadata()?.len() > limit as u64 { return Err(Error::Capacity(format!("file exceeds {limit} bytes"))); }
    let mut data = Vec::new();
    Read::by_ref(&mut file).take(limit as u64 + 1).read_to_end(&mut data)?;
    if data.len() > limit { return Err(Error::Capacity("file grew beyond size limit".into())); }
    Ok(data)
}

/// Atomic create, never overwrite. All temporary files stay on the same volume.
pub fn atomic_create(path: &Path, bytes: &[u8]) -> Result<bool> {
    let parent = path.parent().ok_or_else(|| Error::Invalid("destination has no parent".into()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    match temp.persist_noclobber(path) {
        Ok(_) => { sync_directory(parent)?; Ok(true) }
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(Error::Io(e.error)),
    }
}
fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)] { File::open(path)?.sync_all()?; }
    #[cfg(not(unix))] { let _ = path; }
    Ok(())
}

impl Artifacts {
    pub fn open(workspace: &Path) -> Result<Self> {
        fs::create_dir_all(workspace)?;
        let root = workspace.canonicalize()?;
        for name in ["objects", "metadata", "inbox", "models"] {
            let path = root.join(name); fs::create_dir_all(&path)?;
            if path.symlink_metadata()?.file_type().is_symlink() || !path.canonicalize()?.starts_with(&root) {
                return Err(Error::Invalid(format!("workspace {name} directory cannot be a symlink")));
            }
        }
        Ok(Self { root })
    }
    pub fn root(&self) -> &Path { &self.root }
    pub fn object_path(&self, id: &AssetId) -> Result<PathBuf> {
        let path = self.root.join("objects").join(id.as_str());
        if path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) { return Err(Error::Integrity("asset is a symlink".into())); }
        Ok(path)
    }
    pub fn meta(&self, id: &AssetId) -> Result<AssetMeta> {
        let path = self.root.join("metadata").join(format!("{id}.json"));
        if path.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) { return Err(Error::Integrity("asset metadata is a symlink".into())); }
        let bytes = match read_bounded(&path, 64 * 1024) { Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::NotFound(format!("asset {id}"))), other => other? };
        let meta: AssetMeta = serde_json::from_slice(&bytes)?;
        if &meta.id != id || meta.bytes > MAX_AUDIO_BYTES { return Err(Error::Integrity("invalid asset metadata".into())); }
        Ok(meta)
    }
    pub fn read(&self, id: &AssetId) -> Result<(AssetMeta, Vec<u8>)> {
        let meta = self.meta(id)?;
        let bytes = read_bounded(&self.object_path(id)?, MAX_AUDIO_BYTES)?;
        if bytes.len() != meta.bytes || digest(&bytes) != id.as_str() { return Err(Error::Integrity(format!("asset {id} failed SHA-256 verification"))); }
        Ok((meta, bytes))
    }
    pub fn audio(&self, id: &AssetId) -> Result<Audio> {
        let (meta, bytes) = self.read(id)?;
        if meta.kind != AssetKind::Audio { return Err(Error::Invalid("asset is not audio".into())); }
        Audio::from_wav(&bytes)
    }
    pub fn transcript(&self, id: &AssetId) -> Result<Transcript> {
        let (meta, bytes) = self.read(id)?;
        if meta.kind != AssetKind::Transcript { return Err(Error::Invalid("asset is not a transcript".into())); }
        let transcript: Transcript = serde_json::from_slice(&bytes)?;
        transcript.validate()?; Ok(transcript)
    }
    pub fn put_audio(&self, audio: &Audio) -> Result<AssetMeta> {
        self.put(&audio.to_wav()?, AssetKind::Audio, "audio/wav", Some(audio.stats()))
    }
    /// Imports a WAV as canonical mono float32; the source file is never modified.
    pub fn import_wav(&self, bytes: &[u8]) -> Result<AssetMeta> { self.put_audio(&Audio::from_wav(bytes)?) }
    pub fn put_transcript(&self, transcript: &Transcript) -> Result<AssetMeta> {
        transcript.validate()?;
        self.put(&serde_json::to_vec(transcript)?, AssetKind::Transcript, "application/json", None)
    }
    pub fn put_subtitles(&self, transcript: &Transcript, format: SubtitleFormat) -> Result<AssetMeta> {
        let text = transcript.subtitles(format)?;
        self.put(text.as_bytes(), AssetKind::Subtitle, match format { SubtitleFormat::Srt => "application/x-subrip", SubtitleFormat::Vtt => "text/vtt" }, None)
    }
    pub fn import_inbox(&self, relative: &str) -> Result<AssetMeta> {
        let path = Path::new(relative);
        if path.as_os_str().is_empty() || path.components().any(|c| !matches!(c, Component::Normal(_))) { return Err(Error::Invalid("inbox path must contain only normal relative components".into())); }
        let inbox = self.root.join("inbox").canonicalize()?;
        let source = inbox.join(path).canonicalize()?;
        if !source.starts_with(&inbox) || !source.is_file() { return Err(Error::Invalid("source must be a regular file within the workspace inbox".into())); }
        self.import_wav(&read_bounded(&source, MAX_AUDIO_BYTES)?)
    }
    fn put(&self, bytes: &[u8], kind: AssetKind, media_type: &str, audio: Option<AudioStats>) -> Result<AssetMeta> {
        if bytes.len() > MAX_AUDIO_BYTES { return Err(Error::Capacity("artifact exceeds 128 MiB".into())); }
        let id = AssetId::try_from(digest(bytes)).map_err(Error::Internal)?;
        let object_path = self.object_path(&id)?;
        if !atomic_create(&object_path, bytes)? && read_bounded(&object_path, MAX_AUDIO_BYTES)? != bytes {
            return Err(Error::Integrity("existing content-addressed object differs".into()));
        }
        let meta = AssetMeta { id: id.clone(), kind, media_type: media_type.into(), bytes: bytes.len(), created_ms: now_ms(), audio };
        let meta_path = self.root.join("metadata").join(format!("{id}.json"));
        atomic_create(&meta_path, &serde_json::to_vec(&meta)?)?;
        let saved = self.meta(&id)?;
        if saved.kind != kind || saved.bytes != bytes.len() || saved.media_type != media_type { return Err(Error::Integrity("existing artifact metadata differs".into())); }
        Ok(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn content_addressing_is_idempotent() {
        let dir = tempfile::tempdir().unwrap(); let s = Artifacts::open(dir.path()).unwrap(); let a = Audio::new(16000, vec![0.0; 160]).unwrap();
        let first = s.put_audio(&a).unwrap(); let second = s.put_audio(&a).unwrap();
        assert_eq!(first.id, second.id); assert_eq!(first.created_ms, second.created_ms); assert_eq!(s.audio(&first.id).unwrap().samples.len(), 160);
    }
    #[test] fn detects_tampering() {
        let dir = tempfile::tempdir().unwrap(); let s = Artifacts::open(dir.path()).unwrap(); let a = s.put_audio(&Audio::new(16000, vec![0.0; 160]).unwrap()).unwrap();
        fs::write(s.object_path(&a.id).unwrap(), b"corrupt").unwrap(); assert!(matches!(s.read(&a.id), Err(Error::Integrity(_))));
    }
    #[test] fn atomic_create_does_not_replace() {
        let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("object"); assert!(atomic_create(&path, b"first").unwrap()); assert!(!atomic_create(&path, b"second").unwrap()); assert_eq!(fs::read(path).unwrap(), b"first");
    }
    #[test] fn rejects_traversal_inbox() {
        let dir = tempfile::tempdir().unwrap(); let s = Artifacts::open(dir.path()).unwrap(); for path in ["../secret", "/etc/passwd", ""] { assert!(s.import_inbox(path).is_err()); }
    }
    #[cfg(unix)]
    #[test] fn rejects_escaping_inbox_symlink() {
        let dir = tempfile::tempdir().unwrap(); let outside = tempfile::NamedTempFile::new().unwrap(); let s = Artifacts::open(dir.path()).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("inbox/x.wav")).unwrap(); assert!(s.import_inbox("x.wav").is_err());
    }
}
