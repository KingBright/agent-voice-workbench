//! Bounded mono DSP. WAV decode/downmix, windowed-sinc resampling, fades and mixing.
//! RMS/peak are not EBU R128 integrated loudness and are not labelled LUFS.
use crate::{Error, Result, control::Control, types::{Clip, check_peak, check_rate}};
use serde::{Deserialize, Serialize};
use std::io::Cursor;

pub const MAX_SAMPLES: usize = 48_000_000;
pub const MAX_AUDIO_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Audio { pub sample_rate: u32, pub samples: Vec<f32> }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioStats {
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: usize,
    pub duration_ms: u64,
    pub peak: f32,
    pub peak_dbfs: Option<f32>,
    pub rms_dbfs: Option<f32>,
    pub clipping_fraction: f64,
    pub dc_offset: f64,
}

pub fn sample_at(ms: u64, rate: u32) -> Result<usize> {
    let frames = u128::from(ms) * u128::from(rate) / 1000;
    usize::try_from(frames).map_err(|_| Error::Capacity("sample index overflow".into()))
}

impl Audio {
    pub fn new(sample_rate: u32, samples: Vec<f32>) -> Result<Self> {
        check_rate(sample_rate)?;
        if samples.is_empty() { return Err(Error::Invalid("audio is empty".into())); }
        if samples.len() > MAX_SAMPLES { return Err(Error::Capacity("decoded audio exceeds 48 million mono samples".into())); }
        if samples.iter().any(|s| !s.is_finite()) { return Err(Error::Invalid("audio contains NaN or infinity".into())); }
        Ok(Self { sample_rate, samples })
    }
    /// Explicitly downmixes all source channels by arithmetic average.
    pub fn from_wav(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_AUDIO_BYTES { return Err(Error::Capacity("WAV input exceeds 128 MiB".into())); }
        let mut reader = hound::WavReader::new(Cursor::new(bytes))?;
        let spec = reader.spec();
        check_rate(spec.sample_rate)?;
        if spec.channels == 0 || spec.channels > 8 { return Err(Error::Invalid("WAV requires 1..=8 channels".into())); }
        if reader.duration() as usize > MAX_SAMPLES { return Err(Error::Capacity("WAV duration exceeds decoded sample limit".into())); }
        let channels = usize::from(spec.channels);
        let mut mono = Vec::with_capacity(reader.duration() as usize);
        let mut channel = 0usize;
        let mut sum = 0.0f64;
        let mut push = |sample: f32| -> Result<()> {
            if !sample.is_finite() { return Err(Error::Invalid("nonfinite WAV sample".into())); }
            sum += f64::from(sample);
            channel += 1;
            if channel == channels {
                if mono.len() >= MAX_SAMPLES { return Err(Error::Capacity("decoded sample limit".into())); }
                mono.push((sum / channels as f64) as f32);
                channel = 0; sum = 0.0;
            }
            Ok(())
        };
        match spec.sample_format {
            hound::SampleFormat::Float => {
                if spec.bits_per_sample != 32 { return Err(Error::Unsupported("only float32 WAV is supported".into())); }
                for value in reader.samples::<f32>() { push(value?)?; }
            }
            hound::SampleFormat::Int => {
                if ![8, 16, 24, 32].contains(&spec.bits_per_sample) { return Err(Error::Unsupported("PCM WAV requires 8/16/24/32 bits".into())); }
                let scale = (1u64 << (spec.bits_per_sample - 1)) as f64;
                for value in reader.samples::<i32>() { push((f64::from(value?) / scale) as f32)?; }
            }
        }
        if channel != 0 { return Err(Error::Invalid("incomplete interleaved WAV frame".into())); }
        Self::new(spec.sample_rate, mono)
    }
    pub fn to_wav(&self) -> Result<Vec<u8>> {
        let bytes = self.samples.len().checked_mul(4).and_then(|n| n.checked_add(128)).ok_or_else(|| Error::Capacity("WAV size overflow".into()))?;
        if bytes > MAX_AUDIO_BYTES { return Err(Error::Capacity("encoded WAV exceeds 128 MiB".into())); }
        let mut out = Cursor::new(Vec::with_capacity(bytes));
        {
            let spec = hound::WavSpec { channels: 1, sample_rate: self.sample_rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
            let mut writer = hound::WavWriter::new(&mut out, spec)?;
            for &s in &self.samples { writer.write_sample(s)?; }
            writer.finalize()?;
        }
        Ok(out.into_inner())
    }
    pub fn stats(&self) -> AudioStats {
        let peak = self.samples.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        let squares: f64 = self.samples.iter().map(|v| f64::from(*v).powi(2)).sum();
        let rms = (squares / self.samples.len() as f64).sqrt();
        AudioStats {
            sample_rate: self.sample_rate, channels: 1, frames: self.samples.len(),
            duration_ms: (self.samples.len() as u128 * 1000 / u128::from(self.sample_rate)) as u64,
            peak,
            peak_dbfs: (peak > 0.0).then(|| 20.0 * peak.log10()),
            rms_dbfs: (rms > 0.0).then(|| (20.0 * rms.log10()) as f32),
            clipping_fraction: self.samples.iter().filter(|v| v.abs() >= 1.0).count() as f64 / self.samples.len() as f64,
            dc_offset: self.samples.iter().map(|v| f64::from(*v)).sum::<f64>() / self.samples.len() as f64,
        }
    }
    pub fn trim(&self, start_ms: u64, end_ms: Option<u64>) -> Result<Self> {
        let start = sample_at(start_ms, self.sample_rate)?;
        let end = match end_ms { Some(ms) => sample_at(ms, self.sample_rate)?, None => self.samples.len() };
        if start >= end || end > self.samples.len() { return Err(Error::Invalid("trim range is outside the audio".into())); }
        Self::new(self.sample_rate, self.samples[start..end].to_vec())
    }
    pub fn normalize_peak(&mut self, target_dbfs: f32) -> Result<()> {
        check_peak(Some(target_dbfs))?;
        let peak = self.samples.iter().fold(0.0f32, |p, s| p.max(s.abs()));
        if peak > 0.0 {
            let gain = 10f64.powf(f64::from(target_dbfs) / 20.0) / f64::from(peak);
            for s in &mut self.samples { *s = (f64::from(*s) * gain) as f32; }
        }
        Ok(())
    }
    pub fn fade(&mut self, in_ms: u64, out_ms: u64) -> Result<()> {
        let len = self.samples.len();
        let fade_in = sample_at(in_ms, self.sample_rate)?.min(len);
        let fade_out = sample_at(out_ms, self.sample_rate)?.min(len);
        for (i, sample) in self.samples.iter_mut().enumerate() {
            let a = if i < fade_in { i as f32 / fade_in.saturating_sub(1).max(1) as f32 } else { 1.0 };
            let remaining = len - 1 - i;
            let b = if remaining < fade_out { remaining as f32 / fade_out.saturating_sub(1).max(1) as f32 } else { 1.0 };
            *sample *= a.min(1.0) * b.min(1.0);
        }
        Ok(())
    }
    /// Band-limited offline resampling with a Blackman-windowed sinc kernel.
    /// Downsampling widens the input-domain support to retain anti-aliasing.
    pub fn resample(&self, target_rate: u32, control: &Control) -> Result<Self> {
        check_rate(target_rate)?;
        if target_rate == self.sample_rate { return Ok(self.clone()); }
        let out_len = ((self.samples.len() as u128 * u128::from(target_rate) + u128::from(self.sample_rate) / 2) / u128::from(self.sample_rate)) as usize;
        if out_len == 0 || out_len > MAX_SAMPLES { return Err(Error::Capacity("resampled audio exceeds sample limits".into())); }
        let ratio = f64::from(self.sample_rate) / f64::from(target_rate);
        let cutoff = (1.0 / ratio).min(1.0) * 0.95;
        let radius = (32.0 / cutoff).ceil() as i64;
        let mut out = Vec::with_capacity(out_len);
        for i in 0..out_len {
            if i % 1024 == 0 { control.check()?; }
            let pos = i as f64 * ratio;
            let center = pos.floor() as i64;
            let mut value = 0.0f64;
            let mut total = 0.0f64;
            for index in center - radius..=center + radius {
                if index < 0 || index as usize >= self.samples.len() { continue; }
                let x = pos - index as f64;
                let window_pos = x / radius as f64;
                if window_pos.abs() > 1.0 { continue; }
                let arg = std::f64::consts::PI * x * cutoff;
                let sinc = if arg.abs() < 1e-12 { 1.0 } else { arg.sin() / arg };
                let window = 0.42 + 0.5 * (std::f64::consts::PI * window_pos).cos() + 0.08 * (2.0 * std::f64::consts::PI * window_pos).cos();
                let weight = cutoff * sinc * window;
                value += f64::from(self.samples[index as usize]) * weight;
                total += weight;
            }
            out.push(if total.abs() > 1e-12 { (value / total) as f32 } else { 0.0 });
        }
        Self::new(target_rate, out)
    }
}

/// Clips are loaded one at a time; there is no hidden time-stretch or hard clipping.
pub fn render<F>(rate: u32, clips: &[Clip], peak: Option<f32>, control: &Control, mut load: F) -> Result<Audio>
where F: FnMut(&crate::types::AssetId) -> Result<Audio> {
    check_rate(rate)?; check_peak(peak)?;
    if clips.is_empty() || clips.len() > 256 { return Err(Error::Invalid("render requires 1..=256 clips".into())); }
    let mut mix = Vec::<f32>::new();
    for clip in clips {
        control.check()?;
        let audio = load(&clip.asset)?.trim(clip.trim_start_ms, clip.trim_end_ms)?;
        let mut audio = audio.resample(rate, control)?;
        audio.fade(clip.fade_in_ms, clip.fade_out_ms)?;
        let offset = sample_at(clip.at_ms, rate)?;
        let end = offset.checked_add(audio.samples.len()).ok_or_else(|| Error::Capacity("timeline overflow".into()))?;
        if end > MAX_SAMPLES { return Err(Error::Capacity("render exceeds sample budget".into())); }
        mix.resize(mix.len().max(end), 0.0);
        if !clip.gain_db.is_finite() || !(-60.0..=24.0).contains(&clip.gain_db) { return Err(Error::Invalid("invalid clip gain".into())); }
        let gain = 10f32.powf(clip.gain_db / 20.0);
        for (i, &sample) in audio.samples.iter().enumerate() {
            if i % 16384 == 0 { control.check()?; }
            mix[offset + i] += sample * gain;
        }
    }
    let mut audio = Audio::new(rate, mix)?;
    if let Some(db) = peak { audio.normalize_peak(db)?; }
    Ok(audio)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn rejects_empty_nan_and_bad_rate() {
        assert!(Audio::new(16000, vec![]).is_err());
        assert!(Audio::new(16000, vec![f32::NAN]).is_err());
        assert!(Audio::new(0, vec![0.0]).is_err());
    }
    #[test] fn float_wav_roundtrip() {
        let a = Audio::new(24000, vec![-1.0, -0.125, 0.0, 0.125, 1.0]).unwrap();
        assert_eq!(Audio::from_wav(&a.to_wav().unwrap()).unwrap().samples, a.samples);
    }
    #[test] fn stereo_downmix() {
        let mut bytes = Cursor::new(Vec::new());
        { let mut w = hound::WavWriter::new(&mut bytes, hound::WavSpec { channels: 2, sample_rate: 16000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int }).unwrap();
          w.write_sample(1000i16).unwrap(); w.write_sample(-1000i16).unwrap(); w.finalize().unwrap(); }
        assert_eq!(Audio::from_wav(bytes.get_ref()).unwrap().samples, vec![0.0]);
    }
    #[test] fn silence_stats_are_json_safe() {
        let a = Audio::new(16000, vec![0.0; 16]).unwrap();
        assert!(a.stats().peak_dbfs.is_none());
        assert!(a.stats().rms_dbfs.is_none());
        assert!(serde_json::to_string(&a.stats()).is_ok());
    }
    #[test] fn exact_trim_and_out_of_bounds() {
        let a = Audio::new(16000, vec![0.5; 16000]).unwrap();
        assert_eq!(a.trim(100, Some(300)).unwrap().samples.len(), 3200);
        assert!(a.trim(0, Some(1001)).is_err());
        assert!(a.trim(500, Some(500)).is_err());
    }
    #[test] fn normalization_preserves_silence() { let mut a = Audio::new(16000, vec![0.0; 16]).unwrap(); a.normalize_peak(-3.0).unwrap(); assert!(a.samples.iter().all(|s| *s == 0.0)); }
    #[test] fn normalization_sets_peak() { let mut a = Audio::new(16000, vec![0.0, -0.5, 0.2]).unwrap(); a.normalize_peak(-6.0).unwrap(); assert!((a.stats().peak_dbfs.unwrap() + 6.0).abs() < 0.001); }
    #[test] fn fade_reaches_zero() { let mut a = Audio::new(16000, vec![1.0; 160]).unwrap(); a.fade(2, 2).unwrap(); assert_eq!(a.samples[0], 0.0); assert_eq!(a.samples[159], 0.0); assert_eq!(a.samples[80], 1.0); }
    #[test] fn resample_preserves_dc_and_length() {
        let a = Audio::new(48000, vec![0.25; 480]).unwrap().resample(16000, &Control::unbounded()).unwrap();
        assert_eq!(a.samples.len(), 160); assert!(a.samples.iter().all(|s| (*s - 0.25).abs() < 1e-5));
    }
    #[test] fn downsample_rejects_out_of_band_tone() {
        let data = (0..4800).map(|i| (2.0 * std::f64::consts::PI * 12000.0 * i as f64 / 48000.0).sin() as f32).collect();
        let a = Audio::new(48000, data).unwrap().resample(16000, &Control::unbounded()).unwrap();
        let rms = (a.samples[100..1500].iter().map(|s| s * s).sum::<f32>() / 1400.0).sqrt();
        assert!(rms < 0.01, "alias RMS: {rms}");
    }
    #[test] fn cancellation_stops_resampling() { let c = Control::unbounded(); c.cancel(); assert!(matches!(Audio::new(48000, vec![1.0; 480]).unwrap().resample(16000, &c), Err(Error::Cancelled))); }
    #[test] fn sample_clock_does_not_accumulate_rounding() { assert_eq!(sample_at(1000, 44100).unwrap(), 44100); assert_eq!(sample_at(1234567, 48000).unwrap(), 59259216); }
    #[test] fn overlay_sums_instead_of_concatenating() {
        let clip = Clip { asset: crate::types::AssetId::try_from("0".repeat(64)).unwrap(), at_ms: 0, trim_start_ms: 0, trim_end_ms: None, gain_db: 0.0, fade_in_ms: 0, fade_out_ms: 0 };
        let a = render(16000, &[clip.clone(), clip], None, &Control::unbounded(), |_| Audio::new(16000, vec![0.25; 160])).unwrap();
        assert_eq!(a.samples.len(), 160); assert_eq!(a.samples[0], 0.5);
    }
}
