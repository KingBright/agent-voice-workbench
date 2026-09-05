use crate::{Error, Result, types::{AssetId, SubtitleFormat}};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TimingSource {
    /// The model returned text only. Subtitle export must not invent timing.
    Unavailable,
    /// Audio chunks are known, but word positions inside them are not.
    AudioChunks,
    /// Actual forced-aligner output, not uniform text splitting.
    ForcedAligner,
    /// Caller-supplied timestamps; not independently verified by this workbench.
    Imported,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub speaker: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transcript {
    pub schema_version: u32,
    pub source_audio: Option<AssetId>,
    pub language: Option<String>,
    pub text: String,
    pub timing_source: TimingSource,
    pub segments: Vec<Segment>,
}
impl Transcript {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 { return Err(Error::Invalid("unsupported transcript schema version".into())); }
        if self.text.contains('\0') || self.language.as_ref().is_some_and(|v| v.len() > 64 || v.chars().any(char::is_control)) { return Err(Error::Invalid("invalid transcript text/language".into())); }
        if self.text.len() > 4 * 1024 * 1024 || self.segments.len() > 100_000 { return Err(Error::Capacity("transcript is too large".into())); }
        if self.timing_source == TimingSource::Unavailable && !self.segments.is_empty() { return Err(Error::Invalid("unavailable timing must not contain timestamped segments".into())); }
        let mut previous_start = 0;
        let mut segment_bytes = 0usize;
        for (i, s) in self.segments.iter().enumerate() {
            segment_bytes = segment_bytes.checked_add(s.text.len()).ok_or_else(|| Error::Capacity("transcript text budget overflow".into()))?;
            if segment_bytes > 4 * 1024 * 1024 { return Err(Error::Capacity("combined segment text exceeds 4 MiB".into())); }
            if s.end_ms <= s.start_ms || s.end_ms > 24 * 3_600_000 || (i > 0 && s.start_ms < previous_start) {
                return Err(Error::Invalid("timestamps must have positive durations and nondecreasing starts within 24h".into()));
            }
            if s.text.trim().is_empty() || s.text.contains('\0') || s.text.len() > 64 * 1024 { return Err(Error::Invalid("invalid segment text".into())); }
            if s.speaker.as_ref().is_some_and(|v| v.len() > 128 || v.chars().any(char::is_control)) { return Err(Error::Invalid("invalid speaker label".into())); }
            previous_start = s.start_ms;
        }
        Ok(())
    }
    pub fn subtitles(&self, format: SubtitleFormat) -> Result<String> {
        self.validate()?;
        if self.segments.is_empty() || self.timing_source == TimingSource::Unavailable {
            return Err(Error::Unsupported("no timestamps: run real forced alignment before subtitle export".into()));
        }
        let mut out = match format { SubtitleFormat::Srt => String::new(), SubtitleFormat::Vtt => "WEBVTT\n\n".into() };
        for (i, s) in self.segments.iter().enumerate() {
            let label = s.speaker.as_ref().map(|v| format!("[{v}] ")).unwrap_or_default();
            let text = clean_cue(&format!("{label}{}", s.text), matches!(format, SubtitleFormat::Vtt));
            out.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, timestamp(s.start_ms, format), timestamp(s.end_ms, format), text));
        }
        Ok(out)
    }
}
fn clean_cue(text: &str, vtt: bool) -> String {
    // Flatten control characters and blank lines to prevent injected cue blocks.
    let filtered: String = text.chars().filter(|c| !c.is_control() || c.is_whitespace()).collect();
    let clean = filtered.split_whitespace().collect::<Vec<_>>().join(" ").replace("-->", "→");
    if vtt { clean.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") }
    else { clean.replace('<', "‹").replace('>', "›") }
}
fn timestamp(ms: u64, format: SubtitleFormat) -> String {
    let separator = match format { SubtitleFormat::Srt => ',', SubtitleFormat::Vtt => '.' };
    format!("{:02}:{:02}:{:02}{}{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, separator, ms % 1000)
}

/// Parses the documented `[start][S01] text [end]` MOSS output format.
/// This is a parser for supplied text, NOT a MOSS neural inference implementation.
pub fn parse_moss(text: &str, source_audio: Option<AssetId>) -> Result<Transcript> {
    if text.len() > 4 * 1024 * 1024 { return Err(Error::Capacity("MOSS transcript input is too large".into())); }
    let mut segments = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let (start, after_start) = bracket(rest)?;
        let start_ms = decimal_seconds_ms(start)?;
        let (speaker, after_speaker) = bracket(after_start.trim_start())?;
        if !speaker.starts_with('S') || speaker.len() < 2 || !speaker[1..].bytes().all(|v| v.is_ascii_digit()) {
            return Err(Error::Invalid("MOSS speaker must be S followed by digits".into()));
        }
        let mut endpoint = None;
        // Bracketed acoustic events may occur in the text; only numeric brackets end a segment.
        for (index, _) in after_speaker.match_indices('[') {
            if let Ok((candidate, tail)) = bracket(&after_speaker[index..]) {
                if let Ok(end_ms) = decimal_seconds_ms(candidate) {
                    endpoint = Some((index, end_ms, tail)); break;
                }
            }
        }
        let (index, end_ms, tail) = endpoint.ok_or_else(|| Error::Invalid("missing MOSS segment end timestamp".into()))?;
        segments.push(Segment { start_ms, end_ms, text: after_speaker[..index].trim().to_owned(), speaker: Some(speaker.to_owned()) });
        rest = tail.trim_start();
    }
    if segments.is_empty() { return Err(Error::Invalid("MOSS transcript contains no segments".into())); }
    let result = Transcript { schema_version: 1, source_audio, language: None, text: segments.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n"), timing_source: TimingSource::Imported, segments };
    result.validate()?;
    Ok(result)
}
fn bracket(text: &str) -> Result<(&str, &str)> {
    if !text.starts_with('[') { return Err(Error::Invalid("expected opening bracket".into())); }
    let end = text.find(']').ok_or_else(|| Error::Invalid("unclosed bracket".into()))?;
    Ok((&text[1..end], &text[end + 1..]))
}
fn decimal_seconds_ms(text: &str) -> Result<u64> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) || frac.len() > 6 {
        return Err(Error::Invalid("invalid decimal timestamp".into()));
    }
    let seconds: u64 = whole.parse().map_err(|_| Error::Invalid("timestamp overflow".into()))?;
    let fraction = format!("{frac:0<3}");
    let millis: u64 = fraction[..3].parse().map_err(|_| Error::Invalid("invalid timestamp fraction".into()))?;
    seconds.checked_mul(1000).and_then(|v| v.checked_add(millis)).ok_or_else(|| Error::Invalid("timestamp overflow".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn parses_moss_multispeaker() {
        let t = parse_moss("[0.11][S01] 你好！ [1.03]\n[1.11][S02] Hello. [2.42]", None).unwrap();
        assert_eq!(t.segments.len(), 2); assert_eq!(t.segments[1].start_ms, 1110); assert_eq!(t.segments[1].speaker.as_deref(), Some("S02"));
    }
    #[test] fn preserves_event_labels() { assert!(parse_moss("[0][S01] [laugh] hello [1]", None).unwrap().text.contains("[laugh]")); }
    #[test] fn rejects_missing_or_reversed_timestamps() {
        for s in ["[1][S01] hello", "[2][S01] hi [1]", "[NaN][S01] hi [1]", ""] { assert!(parse_moss(s, None).is_err()); }
    }
    #[test] fn exports_millisecond_precision() {
        let t = parse_moss("[0.001][S01] 你好 [61.234]", None).unwrap();
        assert!(t.subtitles(SubtitleFormat::Srt).unwrap().contains("00:00:00,001 --> 00:01:01,234"));
        assert!(t.subtitles(SubtitleFormat::Vtt).unwrap().starts_with("WEBVTT\n\n"));
    }
    #[test] fn allows_overlapping_speakers() { assert!(parse_moss("[0][S01] hello [2] [1][S02] yes [3]", None).is_ok()); }
    #[test] fn rejects_out_of_order_segments() { assert!(parse_moss("[2][S01] hello [3] [1][S02] yes [4]", None).is_err()); }
    #[test] fn does_not_invent_timestamps() {
        let t = Transcript { schema_version: 1, source_audio: None, language: None, text: "hello".into(), timing_source: TimingSource::Unavailable, segments: vec![] };
        assert!(t.subtitles(SubtitleFormat::Srt).is_err());
    }
    #[test] fn escapes_vtt_markup() { let t = parse_moss("[0][S01] <b>yes</b> [1]", None).unwrap(); assert!(t.subtitles(SubtitleFormat::Vtt).unwrap().contains("&lt;b&gt;")); }
    #[test] fn decimal_parser_does_not_round_across_a_second() { assert_eq!(decimal_seconds_ms("9.9999").unwrap(), 9999); }
}
