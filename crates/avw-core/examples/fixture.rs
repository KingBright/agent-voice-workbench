//! Deterministic DSP test fixture; this is a sine wave, NOT text-to-speech.
use avw_core::{audio::Audio, artifacts::atomic_create};
fn main() -> Result<(),Box<dyn std::error::Error>> {
    let output=std::env::args_os().nth(1).ok_or("usage: cargo run -p avw-core --example fixture -- OUTPUT.wav")?;
    let path=std::path::PathBuf::from(output); let path=if path.is_absolute() { path } else { std::env::current_dir()?.join(path) };
    let samples=(0..16000).map(|i| 0.25*(2.0*std::f32::consts::PI*440.0*i as f32/16000.0).sin()).collect();
    let audio=Audio::new(16000,samples)?;
    if !atomic_create(&path,&audio.to_wav()?)? { return Err("output already exists".into()); }
    eprintln!("wrote a 1-second 440-Hz sine-wave DSP fixture to {}",path.display()); Ok(())
}
