//! PCM decode and mix for native playback.
//!
//! Audio is decoded once per layer through the bundle ffmpeg into interleaved
//! stereo f32 at the plan's mix rate, then mixed per [`AudioMixDescriptor`]:
//! silent base, per-layer gain, and sample-exact duck/mute windows — the same
//! semantics `audio_graph` compiles into the export filter chain.

use std::path::{Path, PathBuf};

use rightkit_ffmpeg::FfmpegCommand;
use rightkit_timeline::{MilliDb, SourceId, Ticks};
use rightkit_video_project::worker::{spawn_ffmpeg, wait_with_output, StdioMode, StdioPolicy};
use thiserror::Error;

use crate::native::audio::{AudioMixDescriptor, AudioSegment};

/// Interleaved stereo f32 PCM at the mix rate.
#[derive(Debug, Clone)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub channels: usize,
    pub sample_rate: i64,
}

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("cannot run ffmpeg at {0}: {1}")]
    Spawn(PathBuf, String),
    #[error("ffmpeg audio decode exited with {status}: {stderr}")]
    Failed { status: String, stderr: String },
    #[error("audio decode produced {got} bytes (not a whole frame)")]
    Truncated { got: usize },
}

fn secs(t: Ticks) -> String {
    let micros = t.to_units(1_000_000);
    format!("{}.{:06}", micros / 1_000_000, micros.rem_euclid(1_000_000))
}

/// Decodes `duration` of `path` starting at `source_in` to stereo f32 at
/// `sample_rate`. Errors are typed; a source with no audio yields silence via
/// [`Pcm::silence`].
pub fn decode_pcm(
    ffmpeg: &Path,
    path: &Path,
    source_in: Ticks,
    duration: Ticks,
    sample_rate: i64,
) -> Result<Pcm, AudioError> {
    let mut typed = FfmpegCommand::new();
    typed
        .texts(["-v", "error", "-ss"])
        .text(secs(source_in))
        .input(path)
        .text("-t")
        .text(secs(duration))
        .texts(["-map", "0:a:0", "-vn", "-f", "f32le", "-ac", "2", "-ar"])
        .text(sample_rate.to_string())
        .text("-");
    let out = spawn_ffmpeg(
        ffmpeg,
        &typed,
        StdioPolicy {
            stdin: StdioMode::Null,
            stdout: StdioMode::Piped,
            stderr: StdioMode::Piped,
        },
    )
    .and_then(wait_with_output)
    .map_err(|e| AudioError::Spawn(ffmpeg.to_path_buf(), e.to_string()))?;
    if !out.status.success() {
        return Err(AudioError::Failed {
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    if out.stdout.len() % 4 != 0 {
        return Err(AudioError::Truncated {
            got: out.stdout.len(),
        });
    }
    let samples = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    Ok(Pcm {
        samples,
        channels: 2,
        sample_rate,
    })
}

impl Pcm {
    pub fn silence(sample_rate: i64) -> Self {
        Self {
            samples: Vec::new(),
            channels: 2,
            sample_rate,
        }
    }
}

/// Millidecibels attenuation -> linear multiplier.
fn duck_gain(amount: MilliDb) -> f32 {
    10f32.powf(-(amount.0 as f32) / 20_000.0)
}

/// Gain applied to `layer` at output sample `s` (frame index = s / channels).
fn layer_gain(seg: &AudioSegment, frame_index: i64) -> f32 {
    let mut g = seg.gain_linear;
    for w in &seg.windows {
        if frame_index >= w.start_sample && frame_index < w.end_sample {
            g *= match w.duck {
                Some(amount) => duck_gain(amount),
                None => 0.0,
            };
        }
    }
    g
}

/// Mixes every layer's decoded PCM into one interleaved stereo buffer of
/// exactly `descriptor.total_samples` frames. `layer_pcm` is keyed by
/// [`AudioSegment::id`]; missing layers contribute silence. Output size is
/// bounded by the descriptor — callers cap `total_samples` before mixing.
pub fn mix(
    descriptor: &AudioMixDescriptor,
    layer_pcm: &std::collections::HashMap<String, Pcm>,
    channels: usize,
) -> Vec<f32> {
    let frames = descriptor.total_samples.max(0) as usize;
    let mut out = vec![0f32; frames * channels];
    for seg in &descriptor.layers {
        let Some(pcm) = layer_pcm.get(&seg.id) else {
            continue;
        };
        let src_frames = pcm.samples.len() / pcm.channels.max(1);
        let avail = (seg.duration_samples.max(0) as usize).min(src_frames);
        let start = seg.start_sample.max(0) as usize;
        for f in 0..avail.min(frames.saturating_sub(start)) {
            let g = layer_gain(seg, (start + f) as i64);
            if g == 0.0 {
                continue;
            }
            for ch in 0..channels {
                let s = pcm.samples[f * pcm.channels + ch.min(pcm.channels - 1)];
                out[(start + f) * channels + ch] += s * g;
            }
        }
    }
    out
}

/// Decodes every layer and mixes. `max_frames` bounds the output buffer
/// (bytes = frames * channels * 4); overshoot returns `Err` rather than
/// silently truncating, so the caller can degrade to video-only playback.
pub fn decode_and_mix(
    ffmpeg: &Path,
    descriptor: &AudioMixDescriptor,
    sources: &dyn Fn(&SourceId) -> Option<PathBuf>,
    channels: usize,
    max_frames: usize,
) -> Result<Vec<f32>, AudioError> {
    if descriptor.total_samples < 0 || descriptor.total_samples as usize > max_frames {
        return Err(AudioError::Failed {
            status: "bound".into(),
            stderr: format!(
                "mix of {} samples exceeds the {}-sample playback bound",
                descriptor.total_samples, max_frames
            ),
        });
    }
    let mut decoded: Vec<(String, Pcm)> = Vec::new();
    for seg in &descriptor.layers {
        let Some(path) = sources(&seg.source) else {
            decoded.push((seg.id.clone(), Pcm::silence(descriptor.sample_rate)));
            continue;
        };
        let pcm = decode_pcm(
            ffmpeg,
            &path,
            seg.source_in,
            Ticks(rightkit_timeline::time::round_div_half_down(
                seg.duration_samples as i128 * rightkit_timeline::TICKS_PER_SECOND as i128,
                descriptor.sample_rate as i128,
            ) as i64),
            descriptor.sample_rate,
        )?;
        decoded.push((seg.id.clone(), pcm));
    }
    let map: std::collections::HashMap<String, Pcm> = decoded.into_iter().collect();
    Ok(mix(descriptor, &map, channels))
}

/// Streaming mixer for playback: decodes only the output window a caller asks
/// for, so memory and open latency stay bounded regardless of timeline length.
/// One ffmpeg pass decodes each overlapping layer per window; the caller owns
/// the buffering policy (ring size, look-ahead, chunk size).
pub struct AudioStreamer {
    ffmpeg: PathBuf,
    descriptor: AudioMixDescriptor,
    sources: std::collections::HashMap<SourceId, PathBuf>,
    channels: usize,
}

impl AudioStreamer {
    pub fn new(
        ffmpeg: PathBuf,
        descriptor: AudioMixDescriptor,
        sources: std::collections::HashMap<SourceId, PathBuf>,
        channels: usize,
    ) -> Self {
        Self {
            ffmpeg,
            descriptor,
            sources,
            channels: channels.max(1),
        }
    }

    /// Total output frames in the mix.
    pub fn total_frames(&self) -> i64 {
        self.descriptor.total_samples.max(0)
    }

    /// Decodes and mixes output frames `[start_frame, start_frame + frames)`,
    /// clamped to the mix. Returns exactly `frames * channels` samples (with
    /// trailing silence past the mix end) so ring-buffer slots stay full.
    /// Layers that do not overlap the window are never decoded.
    pub fn decode_range(&self, start_frame: i64, frames: usize) -> Result<Vec<f32>, AudioError> {
        let total = self.total_frames();
        let start = start_frame.clamp(0, total);
        let end = (start.saturating_add(frames as i64)).min(total);
        let mut out = vec![0f32; frames * self.channels];
        let rate = self.descriptor.sample_rate as i128;
        let tps = rightkit_timeline::TICKS_PER_SECOND as i128;
        // Decode pad: request a little extra so accurate-seek rounding never
        // leaves the window's tail silent.
        let pad_ticks = (2 * tps / rate.max(1)) as i64;
        for seg in &self.descriptor.layers {
            let seg_start = seg.start_sample.max(0);
            let seg_end = seg_start + seg.duration_samples.max(0);
            let lo = start.max(seg_start);
            let hi = end.min(seg_end);
            if lo >= hi {
                continue;
            }
            let Some(path) = self.sources.get(&seg.source) else {
                continue;
            };
            let skip_samples = lo - seg_start;
            let skip_ticks =
                rightkit_timeline::time::round_div_half_down(skip_samples as i128 * tps, rate)
                    as i64;
            let dur_ticks =
                rightkit_timeline::time::round_div_half_down((hi - lo) as i128 * tps, rate) as i64
                    + pad_ticks;
            let pcm = decode_pcm(
                &self.ffmpeg,
                path,
                Ticks(seg.source_in.0 + skip_ticks),
                Ticks(dur_ticks.max(0)),
                self.descriptor.sample_rate,
            )?;
            let src_frames = pcm.samples.len() / pcm.channels.max(1);
            let out_off = (lo - start) as usize;
            for f in 0..(hi - lo) as usize {
                if f >= src_frames {
                    break;
                }
                let g = layer_gain(seg, lo + f as i64);
                if g == 0.0 {
                    continue;
                }
                for ch in 0..self.channels {
                    let s = pcm.samples[f * pcm.channels + ch.min(pcm.channels - 1)];
                    out[(out_off + f) * self.channels + ch] += s * g;
                }
            }
        }
        Ok(out)
    }
}
