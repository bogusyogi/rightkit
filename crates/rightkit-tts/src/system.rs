//! OS speech backend. It renders to a temporary file, never opens speakers.

use crate::wav;
use rightkit_transcript::{TimedWord, Transcript};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentenceChunk {
    pub text: String,
    pub char_start: usize,
    pub char_end: usize,
}

/// Abbreviation-aware sentence splitter shared by file-backed system TTS.
/// Offsets are character offsets into original UTF-8 text, matching the
/// CodeRight timing-manifest contract.
pub fn chunk_sentences(text: &str) -> Vec<SentenceChunk> {
    const ABBREVIATIONS: &[&str] = &["mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "vs", "etc", "e.g", "i.e", "inc", "ltd", "co", "capt", "gen", "col", "lt", "sgt", "rev", "hon", "no"];
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if matches!(c, '.' | '!' | '?') {
            let prev_digit = i > 0 && chars[i - 1].is_ascii_digit();
            let next_digit = chars.get(i + 1).is_some_and(|c| c.is_ascii_digit());
            if c == '.' && prev_digit && next_digit { i += 1; continue; }
            if c == '.' {
                let mut w = i;
                while w > start && !chars[w - 1].is_whitespace() { w -= 1; }
                let word: String = chars[w..i].iter().collect::<String>().to_ascii_lowercase();
                if ABBREVIATIONS.contains(&word.as_str()) || (word.chars().count() == 1 && word.chars().next().is_some_and(char::is_alphabetic)) { i += 1; continue; }
            }
            if i + 1 < chars.len() && !chars[i + 1].is_whitespace() { i += 1; continue; }
            let mut end = i + 1;
            while end < chars.len() && matches!(chars[end], '.' | '!' | '?') { end += 1; }
            let (mut s, mut e) = (start, end);
            while s < e && chars[s].is_whitespace() { s += 1; }
            while e > s && chars[e - 1].is_whitespace() { e -= 1; }
            if s < e { out.push(SentenceChunk { text: chars[s..e].iter().collect(), char_start: s, char_end: e }); }
            start = end;
            while start < chars.len() && chars[start].is_whitespace() { start += 1; }
            i = start;
            continue;
        }
        i += 1;
    }
    let (mut s, mut e) = (start, chars.len());
    while s < e && chars[s].is_whitespace() { s += 1; }
    while e > s && chars[e - 1].is_whitespace() { e -= 1; }
    if s < e { out.push(SentenceChunk { text: chars[s..e].iter().collect(), char_start: s, char_end: e }); }
    out
}

#[derive(Debug, Clone)]
pub struct SystemTtsOptions {
    pub voice: Option<String>,
    pub rate: Option<i32>,
    pub language: String,
}

impl Default for SystemTtsOptions {
    fn default() -> Self {
        Self { voice: None, rate: None, language: "en".into() }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SystemTtsError {
    #[error("text is empty")]
    EmptyText,
    #[error("system TTS is unavailable: {0}")]
    Unavailable(String),
    #[error("system TTS output is invalid: {0}")]
    InvalidOutput(String),
    #[error("system TTS command failed: {0}")]
    Command(String),
}

#[derive(Debug, Clone)]
pub struct SystemTtsResult {
    pub wav: Vec<u8>,
    pub transcript: Transcript,
    pub backend: String,
    pub manifest: SystemTtsManifest,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemTtsManifest {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    pub total_duration_ms: u64,
    pub segments: Vec<SystemTtsSegment>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SystemTtsSegment {
    pub sentence_index: usize,
    pub text: String,
    pub source_char_start: usize,
    pub source_char_end: usize,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct SystemTts;

impl SystemTts {
    /// Render speech to a file using OS APIs. This command shape writes audio
    /// only; it never invokes playback or touches a speaker device.
    pub fn synthesize(&self, text: &str, options: &SystemTtsOptions) -> Result<SystemTtsResult, SystemTtsError> {
        if text.trim().is_empty() {
            return Err(SystemTtsError::EmptyText);
        }
        let chunks = chunk_sentences(text);
        if chunks.is_empty() { return Err(SystemTtsError::EmptyText); }
        let dir = std::env::temp_dir().join(format!("rightkit-tts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| SystemTtsError::Unavailable(e.to_string()))?;
        let result = self.render_segments(&dir, &chunks, options);
        let _ = std::fs::remove_dir(&dir);
        result
    }

    fn render_segments(&self, dir: &Path, chunks: &[SentenceChunk], options: &SystemTtsOptions) -> Result<SystemTtsResult, SystemTtsError> {
        let mut decoded = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.iter().enumerate() {
            let path = dir.join(format!("segment-{index}.wav"));
            self.render_to(&path, &chunk.text, options)?;
            let bytes = std::fs::read(&path).map_err(|e| SystemTtsError::InvalidOutput(e.to_string()))?;
            let audio = wav::decode_pcm16(&bytes).ok_or_else(|| SystemTtsError::InvalidOutput("expected mono PCM16 WAV".into()))?;
            let _ = std::fs::remove_file(&path);
            if audio.1.is_empty() || audio.0 == 0 { return Err(SystemTtsError::InvalidOutput("empty PCM output".into())); }
            decoded.push(audio);
        }
        let rate = decoded[0].0;
        if decoded.iter().any(|(r, _)| *r != rate) { return Err(SystemTtsError::InvalidOutput("system backend returned mixed sample rates".into())); }
        let rms_values: Vec<f64> = decoded.iter().map(|(_, s)| rms(s)).collect();
        let voiced: Vec<f64> = rms_values.iter().copied().filter(|r| *r > 1.0).collect();
        let target = voiced.iter().sum::<f64>() / voiced.len().max(1) as f64;
        let mut output = Vec::new();
        let mut words = Vec::new();
        let mut manifest = Vec::with_capacity(chunks.len());
        for (index, ((_, mut samples), chunk)) in decoded.into_iter().zip(chunks).enumerate() {
            let segment_start = output.len() as u64 * 1000 / rate as u64;
            if target > 0.0 { match_loudness(&mut samples, rms_values[index], target); }
            let segment_ms = samples.len() as u64 * 1000 / rate as u64;
            words.extend(interpolate_words(&chunk.text, segment_ms).into_iter().map(|mut w| { w.start_ms += segment_start; w.end_ms += segment_start; w }));
            output.extend(samples);
            let segment_end = output.len() as u64 * 1000 / rate as u64;
            manifest.push(SystemTtsSegment { sentence_index: index, text: chunk.text.clone(), source_char_start: chunk.char_start, source_char_end: chunk.char_end, start_ms: segment_start, end_ms: segment_end });
            if index + 1 < chunks.len() { output.extend(std::iter::repeat_n(0, rate as usize * 12 / 1000)); }
        }
        let duration_ms = output.len() as u64 * 1000 / rate as u64;
        let transcript = Transcript::from_words(&words, &options.language, Some(duration_ms), Default::default(), rightkit_transcript::EndSource::Interpolated);
        Ok(SystemTtsResult { wav: wav::encode_pcm16(&output, rate), transcript, backend: if cfg!(target_os = "macos") { "macos-say".into() } else if cfg!(target_os = "windows") { "windows-sapi".into() } else { "unsupported".into() }, manifest: SystemTtsManifest { sample_rate: rate, channels: 1, bits_per_sample: 16, total_duration_ms: duration_ms, segments: manifest } })
    }

    fn render_to(&self, out: &Path, text: &str, options: &SystemTtsOptions) -> Result<(), SystemTtsError> {
        #[cfg(target_os = "macos")]
        {
            // `say` selects its container from output extension; keep its AIFF
            // intermediate separate so final path is always PCM WAV.
            let aiff_path = out.with_extension("aiff");
            let mut cmd = Command::new("say");
            cmd.arg("-o").arg(&aiff_path);
            if let Some(voice) = &options.voice { cmd.arg("-v").arg(voice); }
            if let Some(rate) = options.rate { cmd.arg("-r").arg(rate.to_string()); }
            cmd.arg(text);
            let status = cmd.status().map_err(|e| SystemTtsError::Unavailable(e.to_string()))?;
            if !status.success() { return Err(SystemTtsError::Command(format!("say exited {status}"))); }
            let status = Command::new("afconvert").args(["-f", "WAVE", "-d", "LEI16@24000"]).arg(&aiff_path).arg(out).status().map_err(|e| SystemTtsError::Unavailable(e.to_string()))?;
            if !status.success() { return Err(SystemTtsError::Command(format!("afconvert exited {status}"))); }
            let _ = std::fs::remove_file(&aiff_path);
            Ok(())
        }
        #[cfg(target_os = "windows")]
        {
            let script = "$s=New-Object System.Speech.Synthesis.SpeechSynthesizer; if ($env:RK_TTS_VOICE) { $v=$s.GetInstalledVoices() | ? { $_.VoiceInfo.Name -eq $env:RK_TTS_VOICE } | select -First 1; if ($v) { $s.SelectVoice($v.VoiceInfo.Name) } }; if ($env:RK_TTS_RATE) { $s.Rate=[int]$env:RK_TTS_RATE }; $s.SetOutputToWaveFile($env:RK_TTS_OUT); $s.Speak($env:RK_TTS_TEXT); $s.Dispose()";
            let mut cmd = Command::new("powershell.exe");
            cmd.args(["-NoProfile", "-NonInteractive", "-Command", script]);
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }
            cmd.env("RK_TTS_OUT", out).env("RK_TTS_TEXT", text);
            if let Some(voice) = &options.voice { cmd.env("RK_TTS_VOICE", voice); }
            if let Some(rate) = options.rate { cmd.env("RK_TTS_RATE", rate.to_string()); }
            let status = cmd.status().map_err(|e| SystemTtsError::Unavailable(e.to_string()))?;
            if status.success() { Ok(()) } else { Err(SystemTtsError::Command(format!("SAPI exited {status}"))) }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        { let _ = (out, text, options); Err(SystemTtsError::Unavailable("no OS speech backend".into())) }
    }
}

fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() { return 0.0; }
    (samples.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / samples.len() as f64).sqrt()
}

fn match_loudness(samples: &mut [i16], current: f64, target: f64) {
    if current <= 1.0 { return; }
    let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f64;
    let gain = (target / current).min(if peak > 0.0 { i16::MAX as f64 / peak } else { 1.0 });
    for sample in samples { *sample = ((*sample as f64) * gain).clamp(i16::MIN as f64, i16::MAX as f64) as i16; }
}

fn interpolate_words(text: &str, duration_ms: u64) -> Vec<TimedWord> {
    let toks: Vec<&str> = text.split_whitespace().collect();
    let total: usize = toks.iter().map(|t| t.chars().count()).sum();
    if total == 0 { return Vec::new(); }
    let mut acc = 0;
    toks.into_iter().filter_map(|t| { let s = duration_ms * acc as u64 / total as u64; acc += t.chars().count(); let e = duration_ms * acc as u64 / total as u64; (e > s).then(|| TimedWord::new(t, s, e)) }).collect()
}
