//! FFmpeg-driven audio measurement and offline renders (loudness/true-peak/clipping,
//! astats, band energy, room-tone step, chain/bus/ducking renders).
//!
//! Donor: CutRight `cutright-audio/src/tools.rs`. Reconciled onto the shared
//! `rightkit-media` runner (owned child tree, cancellation, timeout, bounded stderr)
//! instead of CutRight's private `Command` runner; FFmpeg is any
//! [`rightkit_media::FfmpegBinary`] (catalog-verified in production).

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use rightkit_media::{runner::run_checked, CancellationToken, FfmpegBinary};

use crate::chain::DialogueChain;
use crate::ducking::Ducking;
use crate::error::AudioError;
use crate::filter::render_chain;
use crate::room_tone::{
    find_room_tone, room_tone_fill_fragment, GapSpan, RoomToneParams, RoomToneSample,
};

pub struct AudioTools<'a> {
    pub ffmpeg: &'a dyn FfmpegBinary,
    pub timeout: Duration,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelMeasurement {
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub clipped_samples: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AStats {
    pub rms_db: f64,
    pub peak_db: f64,
    pub noise_floor_db: Option<f64>,
    pub rms_trough_db: Option<f64>,
}

/// Samples at or beyond this magnitude count as clipped (just under full scale so a
/// 16-bit full-scale sample still counts).
pub const CLIP_THRESHOLD: f32 = 0.999;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoomToneStep {
    pub before_mean_db: Option<f64>,
    pub after_mean_db: Option<f64>,
    pub step_db: Option<f64>,
}

fn path_arg(p: &Path) -> OsString {
    p.as_os_str().to_os_string()
}

pub fn rms_db(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return f64::NEG_INFINITY;
    }
    let p = samples.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / samples.len() as f64;
    10.0 * (p + 1e-12).log10()
}

pub fn peak_db(samples: &[f32]) -> f64 {
    let p = samples.iter().fold(0f32, |m, s| m.max(s.abs()));
    20.0 * (f64::from(p) + 1e-12).log10()
}

fn last_labeled(text: &str, label: &str) -> Option<f64> {
    text.lines().rev().find_map(|line| {
        let i = line.find(label)?;
        line[i + label.len()..]
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

impl<'a> AudioTools<'a> {
    pub fn new(ffmpeg: &'a dyn FfmpegBinary) -> Self {
        Self {
            ffmpeg,
            timeout: Duration::from_secs(600),
            cancel: CancellationToken::new(),
        }
    }

    /// Run FFmpeg with `args` through the shared runner. Arguments are never joined
    /// into a shell string.
    fn run(&self, args: &[OsString]) -> Result<rightkit_media::Captured, AudioError> {
        let mut cmd = rightkit_media::FfmpegCommand::new();
        for a in args {
            cmd.path(a);
        }
        Ok(run_checked(self.ffmpeg, &cmd, self.timeout, &self.cancel)?)
    }

    fn base(&self, loglevel: &str) -> Vec<OsString> {
        ["-hide_banner", "-nostats", "-loglevel", loglevel, "-y"]
            .iter()
            .map(OsString::from)
            .collect()
    }

    fn need(path: &Path) -> Result<(), AudioError> {
        if path.is_file() {
            Ok(())
        } else {
            Err(AudioError::MissingInput(path.into()))
        }
    }

    /// Decode (optionally a window, optionally through `af`) to mono f32.
    pub fn decode_mono_f32(
        &self,
        input: &Path,
        rate: u32,
        window_ms: Option<(i64, i64)>,
        af: Option<&str>,
    ) -> Result<Vec<f32>, AudioError> {
        Self::need(input)?;
        let mut a = self.base("error");
        if let Some((start, dur)) = window_ms {
            a.extend([
                "-ss".into(),
                format!("{:.3}", start.max(0) as f64 / 1000.0).into(),
            ]);
            a.extend([
                "-t".into(),
                format!("{:.3}", dur.max(0) as f64 / 1000.0).into(),
            ]);
        }
        a.extend(["-i".into(), path_arg(input), "-vn".into()]);
        if let Some(af) = af {
            a.extend(["-af".into(), af.into()]);
        }
        a.extend(["-ac", "1", "-ar"].map(OsString::from));
        a.push(rate.to_string().into());
        a.extend(["-f", "f32le", "-"].map(OsString::from));
        let out = self.run(&a)?;
        Ok(out
            .stdout
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect())
    }

    /// `astats` over a window, optionally after a pre-filter chain.
    pub fn astats(
        &self,
        input: &Path,
        window_ms: Option<(i64, i64)>,
        pre_filter: Option<&str>,
    ) -> Result<AStats, AudioError> {
        Self::need(input)?;
        let mut a = self.base("info");
        if let Some((start, dur)) = window_ms {
            a.extend([
                "-ss".into(),
                format!("{:.3}", start.max(0) as f64 / 1000.0).into(),
            ]);
            a.extend([
                "-t".into(),
                format!("{:.3}", dur.max(0) as f64 / 1000.0).into(),
            ]);
        }
        a.extend(["-i".into(), path_arg(input), "-vn".into(), "-af".into()]);
        let stats = "astats=metadata=0:reset=0";
        a.push(match pre_filter {
            Some(p) => format!("{p},{stats}").into(),
            None => stats.into(),
        });
        a.extend(["-f", "null", "-"].map(OsString::from));
        let e = self.run(&a)?.stderr;
        Ok(AStats {
            rms_db: last_labeled(&e, "RMS level dB:")
                .ok_or_else(|| AudioError::Measurement("astats gave no RMS level".into()))?,
            peak_db: last_labeled(&e, "Peak level dB:")
                .ok_or_else(|| AudioError::Measurement("astats gave no peak level".into()))?,
            noise_floor_db: last_labeled(&e, "Noise floor dB:"),
            // This ffmpeg build spells it "through"; upstream docs say "trough".
            rms_trough_db: last_labeled(&e, "RMS trough dB:")
                .or_else(|| last_labeled(&e, "RMS through dB:")),
        })
    }

    /// RMS (dBFS) of the band `[lo_hz, hi_hz]` over a window: sibilance energy etc.
    pub fn band_rms_db(
        &self,
        input: &Path,
        lo_hz: f64,
        hi_hz: f64,
        window_ms: Option<(i64, i64)>,
    ) -> Result<f64, AudioError> {
        let af = format!(
            "highpass=f={}:poles=2,lowpass=f={}:poles=2",
            crate::filter::num(lo_hz),
            crate::filter::num(hi_hz)
        );
        let s = self.decode_mono_f32(input, 48_000, window_ms, Some(&af))?;
        Ok(rms_db(&s))
    }

    /// Integrated loudness, true peak (one `ebur128` pass) and a clipped-sample count.
    pub fn measure_loudness(&self, input: &Path) -> Result<LevelMeasurement, AudioError> {
        Self::need(input)?;
        let mut a = self.base("info");
        a.extend(["-i".into(), path_arg(input), "-vn".into()]);
        a.extend(["-af", "ebur128=peak=true", "-f", "null", "-"].map(OsString::from));
        let e = self.run(&a)?.stderr;
        // Summary lines are bare ("    I:  -23.0 LUFS"); take the last of each label.
        let labeled = |label: &str| {
            e.lines().rev().find_map(|l| {
                l.trim()
                    .strip_prefix(label)
                    .and_then(|r| r.split_whitespace().next())
                    .and_then(|t| t.parse::<f64>().ok())
            })
        };
        let samples = self.decode_mono_f32(input, 48_000, None, None)?;
        Ok(LevelMeasurement {
            integrated_lufs: labeled("I:"),
            true_peak_dbtp: labeled("Peak:"),
            clipped_samples: samples.iter().filter(|s| s.abs() >= CLIP_THRESHOLD).count() as u64,
        })
    }

    /// Mean-volume step across a join (ported `measure_room_tone_step`).
    pub fn room_tone_step(
        &self,
        input: &Path,
        join_ms: i64,
        window_ms: i64,
    ) -> Result<RoomToneStep, AudioError> {
        let mean = |start: i64| -> Result<Option<f64>, AudioError> {
            let s = self.decode_mono_f32(input, 48_000, Some((start.max(0), window_ms)), None)?;
            Ok((!s.is_empty()).then(|| rms_db(&s)))
        };
        let before = mean(join_ms - window_ms)?;
        let after = mean(join_ms)?;
        Ok(RoomToneStep {
            before_mean_db: before,
            after_mean_db: after,
            step_db: before.zip(after).map(|(b, a)| (a - b).abs()),
        })
    }

    /// Capture a room-tone sample from `input` (source-time VAD spans optional).
    pub fn capture_room_tone(
        &self,
        input: &Path,
        speech_spans_ms: &[(i64, i64)],
        params: &RoomToneParams,
    ) -> Result<RoomToneSample, AudioError> {
        let samples = self.decode_mono_f32(input, 48_000, None, None)?;
        find_room_tone(&samples, 48_000, speech_spans_ms, params)
    }

    /// Offline: run the whole dialogue chain over `input` -> mono 48 kHz WAV.
    pub fn apply_chain(
        &self,
        input: &Path,
        output: &Path,
        chain: &DialogueChain,
    ) -> Result<(), AudioError> {
        Self::need(input)?;
        chain.validate()?;
        if input == output {
            return Err(AudioError::OutputIsInput);
        }
        let mut a = self.base("error");
        a.extend(["-i".into(), path_arg(input), "-vn".into(), "-af".into()]);
        a.push(chain.af().into());
        a.extend(["-ar", "48000", "-ac", "1", "-c:a", "pcm_f32le"].map(OsString::from));
        a.push(path_arg(output));
        self.run(&a).map(|_| ())
    }

    /// Offline: render the room-tone layer the timeline render mixes. The sample window is
    /// cut from `source` AFTER the chain's high-pass/denoise so it carries the same
    /// residual floor as the processed dialogue. Output: mono 48 kHz f32 WAV, `total_ms`
    /// long, room tone only under `gaps` (+ overlap), silence elsewhere.
    #[allow(clippy::too_many_arguments)]
    pub fn render_room_tone_layer(
        &self,
        source: &Path,
        sample: &RoomToneSample,
        chain: &DialogueChain,
        gaps: &[GapSpan],
        gain_db: f64,
        overlap_ms: i64,
        total_ms: i64,
        output: &Path,
    ) -> Result<(), AudioError> {
        Self::need(source)?;
        if source == output {
            return Err(AudioError::OutputIsInput);
        }
        let mut cond = chain.conditioning_filters();
        // The expander lives in the dialogue dynamics stage and would erase the layer
        // stream, so the tone gets the equivalent static pad instead: the injected
        // floor then matches the level the gate leaves behind in pauses.
        if let Some(g) = &chain.expander {
            cond.push(crate::filter::Filter::new("volume").opt(
                "volume",
                format!("{}dB", crate::filter::num(20.0 * g.range.log10())),
            ));
        }
        let cond = if cond.is_empty() {
            "anull".to_string()
        } else {
            render_chain(&cond)
        };
        let s = |ms: i64| format!("{:.3}", ms as f64 / 1000.0);
        let fill = room_tone_fill_fragment(
            "tone",
            sample.duration_ms,
            48_000,
            gaps,
            gain_db,
            overlap_ms,
            total_ms,
            "rt",
            "bed",
        )?;
        let graph = format!(
            "[0:a]{cond},atrim=start={}:end={},asetpts=PTS-STARTPTS[tone];{fill}",
            s(sample.start_ms),
            s(sample.start_ms + sample.duration_ms)
        );
        let mut a = self.base("error");
        a.extend(["-i".into(), path_arg(source), "-vn".into()]);
        a.extend([
            "-filter_complex".into(),
            graph.into(),
            "-map".into(),
            "[bed]".into(),
        ]);
        a.extend(["-ar", "48000", "-ac", "1", "-c:a", "pcm_f32le"].map(OsString::from));
        a.push(path_arg(output));
        self.run(&a).map(|_| ())
    }

    /// Offline: dialogue bus (chain + optional room-tone layer file) -> WAV.
    pub fn render_dialogue_bus(
        &self,
        dialogue: &Path,
        room_tone_layer: Option<&Path>,
        chain: &DialogueChain,
        output: &Path,
    ) -> Result<(), AudioError> {
        Self::need(dialogue)?;
        if dialogue == output || room_tone_layer == Some(output) {
            return Err(AudioError::OutputIsInput);
        }
        let frag = crate::bus::dialogue_bus_fragment(
            chain,
            "0:a",
            room_tone_layer.map(|_| "1:a"),
            "dbus",
            "db",
        )?;
        let mut a = self.base("error");
        a.extend(["-i".into(), path_arg(dialogue)]);
        if let Some(l) = room_tone_layer {
            Self::need(l)?;
            a.extend(["-i".into(), path_arg(l)]);
        }
        a.extend([
            "-filter_complex".into(),
            frag.into(),
            "-map".into(),
            "[dbus]".into(),
        ]);
        a.extend(["-ar", "48000", "-ac", "1", "-c:a", "pcm_f32le"].map(OsString::from));
        a.push(path_arg(output));
        self.run(&a).map(|_| ())
    }

    /// Offline: duck `music` under dialogue. `Sidechain` keys on `dialogue` (used only as key); `Spans` uses
    /// `speech_spans_ms`.
    pub fn duck_music(
        &self,
        music: &Path,
        dialogue: &Path,
        ducking: &Ducking,
        speech_spans_ms: &[(i64, i64)],
        output: &Path,
    ) -> Result<(), AudioError> {
        Self::need(music)?;
        Self::need(dialogue)?;
        if music == output || dialogue == output {
            return Err(AudioError::OutputIsInput);
        }
        let frag = match ducking {
            Ducking::Sidechain { .. } => {
                ducking.fragment("0:a", Some("1:a"), speech_spans_ms, "ducked")?
            }
            Ducking::Spans { .. } => ducking.fragment("0:a", None, speech_spans_ms, "ducked")?,
        };
        let mut a = self.base("error");
        a.extend([
            "-i".into(),
            path_arg(music),
            "-i".into(),
            path_arg(dialogue),
        ]);
        a.extend([
            "-filter_complex".into(),
            frag.into(),
            "-map".into(),
            "[ducked]".into(),
        ]);
        a.extend(["-ar", "48000", "-ac", "1", "-c:a", "pcm_f32le"].map(OsString::from));
        a.push(path_arg(output));
        self.run(&a).map(|_| ())
    }
}
