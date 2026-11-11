//! Real composition via the resolved FFmpeg binary: assemble image cards and
//! video clips, lay narration + optional music bed, burn or mux subtitles,
//! loudness-normalize, then verify by full decode + ffprobe + EBU R128
//! measurement. Every reported field is measured from the produced file.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use rightkit_audio::filter::render_chain;
use rightkit_audio::Filter;
use rightkit_ffmpeg::h264_rule as h264;
use rightkit_ffmpeg::{FfmpegArg, FfmpegBinary, FfmpegCommand};

use super::media;
use crate::{GenError, GenResult};

const DEFAULT_W: u32 = 1280;
const DEFAULT_H: u32 = 720;
const DEFAULT_FPS: u32 = 25;
const TARGET_I: f64 = -16.0;
const TARGET_TP: f64 = -1.5;
const TARGET_LRA: f64 = 11.0;

fn s(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn f(v: &Value, key: &str, dflt: f64) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(dflt)
}

pub(super) fn is_image(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif")
    )
}

fn esc_filter_path(p: &Path) -> String {
    // subtitles= needs ffmpeg filter escaping: \ ' : and filter separators.
    p.to_string_lossy()
        .replace('\\', "\\\\")
        .replace(':', "\\:")
        .replace('\'', "\\'")
        .replace(',', "\\,")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

struct FfmpegBuild {
    args: Vec<String>,
    /// Seconds the video track covers (for the report). 0 = unknown (clip-only
    /// inputs whose durations could not be probed).
    video_duration_s: f64,
    has_audio_out: bool,
    /// "burned" | "soft" | "none" | "unavailable".
    subtitle_mode: &'static str,
    encoder: &'static str,
}

/// Which loudnorm pass the audio chain carries (two-pass EBU R128 normalization).
#[derive(Clone, Copy)]
enum Loudnorm<'a> {
    /// Pass 1: analysis only. The graph ends in `loudnorm ... print_format=json`, nothing is
    /// encoded, and the output is the null muxer. Its stderr carries the measurement JSON.
    Measure,
    /// Pass 2: `linear=true` with the values measured in pass 1.
    Linear(&'a Measured),
    /// Fallback when pass 1 could not be measured: dynamic single-pass loudnorm.
    Single,
}

/// Measured values from pass 1 of loudnorm, as ffmpeg prints them with `print_format=json`.
#[derive(Debug, Clone, PartialEq)]
struct Measured {
    /// `input_i` (measured_I): integrated loudness in LUFS.
    i: f64,
    /// `input_tp` (measured_TP): true peak in dBTP.
    tp: f64,
    /// `input_lra` (measured_LRA): loudness range in LU.
    lra: f64,
    /// `input_thresh` (measured_thresh): gating threshold in LUFS.
    thresh: f64,
    /// `target_offset` (offset): gain offset that pass 2 applies before the limiter.
    offset: f64,
}

/// Single-pass target chain, the same text the golden tests pin.
fn loudnorm_target() -> String {
    format!("loudnorm=I={TARGET_I}:TP={TARGET_TP}:LRA={TARGET_LRA}")
}

/// The loudnorm filter text for one pass.
fn loudnorm_text(norm: Loudnorm) -> String {
    match norm {
        Loudnorm::Measure => format!("{}:print_format=json", loudnorm_target()),
        Loudnorm::Single => loudnorm_target(),
        Loudnorm::Linear(m) => format!(
            "{}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true",
            loudnorm_target(),
            m.i,
            m.tp,
            m.lra,
            m.thresh,
            m.offset
        ),
    }
}

/// Parse the pass-1 measurement from ffmpeg's stderr. The JSON object is the last `{...}` block
/// the loudnorm filter prints; every value is a quoted number and must be finite (silence gives
/// `-inf`, which cannot drive a linear pass 2).
fn parse_loudnorm_json(stderr: &str) -> Result<Measured, String> {
    let end = stderr
        .rfind('}')
        .ok_or("pass 1 printed no loudnorm JSON object")?;
    let start = stderr[..end]
        .rfind('{')
        .ok_or("pass 1 printed no loudnorm JSON object")?;
    let v: Value = serde_json::from_str(&stderr[start..=end])
        .map_err(|e| format!("pass 1 JSON is invalid: {e}"))?;
    let num = |key: &str| -> Result<f64, String> {
        let raw = match v.get(key) {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Number(n)) => n.to_string(),
            _ => return Err(format!("pass 1 JSON has no {key}")),
        };
        let x: f64 = raw
            .parse()
            .map_err(|_| format!("pass 1 {key} is not a number: {raw}"))?;
        if x.is_finite() {
            Ok(x)
        } else {
            Err(format!("pass 1 {key} is not finite: {raw}"))
        }
    };
    Ok(Measured {
        i: num("input_i")?,
        tp: num("input_tp")?,
        lra: num("input_lra")?,
        thresh: num("input_thresh")?,
        offset: num("target_offset")?,
    })
}

/// Build the ffmpeg argument vector for a compose spec.
fn build_args(
    input: &Value,
    out: &Path,
    caps: &media::FfCaps,
    norm: Loudnorm,
) -> GenResult<FfmpegBuild> {
    let items = input
        .get("inputs")
        .and_then(Value::as_array)
        .ok_or_else(|| GenError::new("invalid_input", "compose needs inputs[]"))?
        .clone();
    if items.is_empty() {
        return Err(GenError::new("invalid_input", "compose inputs[] is empty"));
    }
    let w = f(input, "width", DEFAULT_W as f64) as u32;
    let h = f(input, "height", DEFAULT_H as f64) as u32;
    let fps = f(input, "fps", DEFAULT_FPS as f64) as u32;
    let audio = s(input, "audio_path");
    let music = s(input, "music_path");
    let subs = s(input, "subtitles_path");
    let burn_requested = input
        .get("burn_subtitles")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // Burn needs libass (`subtitles` filter); absent → soft mov_text track if
    // the muxer supports it, else subtitles are reported unavailable.
    let (burn, subtitle_mode) = if subs.is_empty() {
        (false, "none")
    } else if burn_requested && caps.can_burn_subtitles() {
        (true, "burned")
    } else if caps.can_mux_subtitles() {
        (false, "soft")
    } else {
        (false, "unavailable")
    };
    let normalize = input
        .get("normalize_loudness")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let image_duration = f(input, "image_duration_s", 4.0).max(0.4);
    // Ken Burns on stills needs the zoompan filter (bundled LGPL build has it).
    let ken_burns_requested = input
        .get("ken_burns")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ken_burns = ken_burns_requested && caps.has_filter("zoompan");
    if input["require_ken_burns"] == true && !ken_burns {
        return Err(GenError::new(
            "filter_missing",
            "still_clip requires ffmpeg zoompan",
        ));
    }

    for it in &items {
        let p = PathBuf::from(s(it, "path"));
        if !p.is_file() {
            return Err(GenError::new(
                "invalid_input",
                format!("input not found: {}", p.display()),
            ));
        }
        // Honest pre-flight: the LGPL bundled build lacks a PNG decoder —
        // say so instead of a cryptic filtergraph failure.
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let dec = match ext.as_str() {
            "png" => Some("png"),
            "jpg" | "jpeg" => Some("mjpeg"),
            "webp" => Some("webp"),
            "bmp" => Some("bmp"),
            "tif" | "tiff" => Some("tiff"),
            "mp4" | "mov" | "m4v" => Some("h264"), // best-effort; mov may hold hevc too
            _ => None,
        };
        if let Some(d) = dec {
            if !caps.has_decoder(d) && !(d == "h264" && caps.has_decoder("hevc")) {
                return Err(GenError::new(
                    "decode_unsupported",
                    format!("input {}: resolved ffmpeg lacks a '{d}' decoder (bundled build has jpeg/webp/bmp/tiff + h264/hevc/mpeg4)", p.display()),
                ));
            }
        }
    }

    // `info` matches FFmpeg's default level: the loudnorm measure pass parses its JSON from stderr.
    let mut cmd = FfmpegCommand::base("info");
    let mut filters: Vec<String> = Vec::new();
    let mut video_labels: Vec<String> = Vec::new();
    let mut video_duration = 0.0f64;

    for (i, it) in items.iter().enumerate() {
        let p = PathBuf::from(s(it, "path"));
        if is_image(&p) {
            let dur = f(it, "duration_s", image_duration).max(0.4);
            video_duration += dur;
            if !ken_burns {
                cmd.texts([
                    "-loop".to_string(),
                    "1".into(),
                    "-framerate".into(),
                    fps.to_string(),
                    "-t".into(),
                    format!("{dur}"),
                ]);
            }
        } else if let Ok(pr) = media::probe(&p) {
            video_duration += pr["duration_s"].as_f64().unwrap_or(0.0);
        }
        cmd.input(&p);
        let lbl = format!("v{i}");
        if is_image(&p) && ken_burns {
            // Ken Burns: zoompan animates a single still frame — slow zoom into
            // the centre over `dur` seconds, supersampled 2x to avoid jitter.
            let frames = (f(it, "duration_s", image_duration).max(0.4) * fps as f64)
                .round()
                .max(1.0) as u32;
            filters.push(format!(
                "[{i}:v]scale={sw}:{sh}:force_original_aspect_ratio=increase,crop={sw}:{sh},zoompan=z='min(1.0+on*0.0006,1.14)':x='(iw-iw/zoom)/2':y='(ih-ih/zoom)/2':d={frames}:s={w}x{h}:fps={fps},setsar=1,format=yuv420p[{lbl}]",
                sw = w * 2,
                sh = h * 2,
            ));
        } else {
            // Normalize every visual input to the frame; clips keep their own length.
            filters.push(format!(
                "[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={fps},format=yuv420p[{lbl}]"
            ));
        }
        video_labels.push(format!("[{lbl}]"));
    }

    let n = items.len();
    let mut next_audio_in = n;
    if !audio.is_empty() {
        let p = PathBuf::from(&audio);
        if !p.is_file() {
            return Err(GenError::new(
                "invalid_input",
                format!("audio not found: {audio}"),
            ));
        }
        cmd.input(&p);
        next_audio_in += 1;
    }
    let music_in = if !music.is_empty() {
        let p = PathBuf::from(&music);
        if !p.is_file() {
            return Err(GenError::new(
                "invalid_input",
                format!("music not found: {music}"),
            ));
        }
        cmd.input(&p);
        let idx = next_audio_in;
        next_audio_in += 1;
        Some(idx)
    } else {
        None
    };
    // Soft-subtitle input must be grouped with the other -i inputs (a `-map`
    // after `-i` is parsed as applying to that input file).
    let subs_in = if subtitle_mode == "soft" {
        let p = PathBuf::from(&subs);
        if !p.is_file() {
            return Err(GenError::new(
                "invalid_input",
                format!("subtitles not found: {subs}"),
            ));
        }
        cmd.input(&p);
        Some(next_audio_in)
    } else {
        None
    };

    let mut filter = filters.join(";");
    let concat_out = if burn {
        filter.push_str(&format!(
            ";{}concat=n={n}:v=1:a=0[cv];[cv]subtitles='{}'[vout]",
            video_labels.concat(),
            esc_filter_path(Path::new(&subs))
        ));
        "vout"
    } else {
        filter.push_str(&format!(
            ";{}concat=n={n}:v=1:a=0[vout]",
            video_labels.concat()
        ));
        "vout"
    };

    let mut maps: Vec<String> = vec!["-map".into(), format!("[{concat_out}]")];
    let has_audio_in = !audio.is_empty() || music_in.is_some();
    // Trim/pad narration and music to the measured video duration when known;
    // otherwise `-shortest` bounds the output.
    let timed = video_duration > 0.0;
    // Audio graph legs are RightKit `Filter` chains; the golden tests below pin the exact text.
    let resample = || Filter::new("aresample").pos(44100);
    let trim = || Filter::new("atrim").pos(0).pos(video_duration);
    let pad = || Filter::new("apad").opt("whole_dur", video_duration);
    let music_gain = || Filter::new("volume").pos(0.22);
    if let (false, Some(mi)) = (audio.is_empty(), music_in) {
        let ai = n;
        let amix = Filter::new("amix")
            .opt("inputs", 2)
            .opt("duration", "first")
            .opt("dropout_transition", 2)
            .render();
        if timed {
            let nar = render_chain(&[resample(), trim()]);
            let mus = render_chain(&[resample(), music_gain(), trim(), pad()]);
            filter.push_str(&format!(
                ";[{ai}:a]{nar}[nar];[{mi}:a]{mus}[mus];[nar][mus]{amix}[aout]"
            ));
        } else {
            let nar = resample().render();
            let mus = render_chain(&[resample(), music_gain()]);
            filter.push_str(&format!(
                ";[{ai}:a]{nar}[nar];[{mi}:a]{mus}[mus];[nar][mus]{amix}[aout]"
            ));
        }
        maps.extend(["-map".into(), "[aout]".into()]);
    } else if !audio.is_empty() {
        let ai = n;
        filter.push_str(&format!(";[{ai}:a]{}[aout]", resample().render()));
        maps.extend(["-map".into(), "[aout]".into()]);
    } else if let Some(mi) = music_in {
        if timed {
            // Pad short music with silence so the track runs the full video length.
            filter.push_str(&format!(
                ";[{mi}:a]{}[aout]",
                render_chain(&[resample(), trim(), pad()])
            ));
        } else {
            filter.push_str(&format!(";[{mi}:a]{}[aout]", resample().render()));
        }
        maps.extend(["-map".into(), "[aout]".into()]);
    }
    if normalize && has_audio_in {
        filter.push_str(&format!(";[aout]{}[aoutn]", loudnorm_text(norm)));
        maps.pop();
        maps.pop();
        maps.extend(["-map".into(), "[aoutn]".into()]);
    }

    cmd.text("-filter_complex").text(filter);
    cmd.texts(maps);
    if matches!(norm, Loudnorm::Measure) {
        // Analysis run: no encode, no subtitles, output discarded to the null muxer.
        // `-nostats` is already in the base.
        cmd.texts(["-vn", "-f", "null", "-"]);
        return Ok(FfmpegBuild {
            args: argv(&cmd),
            video_duration_s: video_duration,
            has_audio_out: has_audio_in,
            subtitle_mode,
            encoder: "measure",
        });
    }
    if let Some(si) = subs_in {
        cmd.text("-map")
            .text(si.to_string())
            .texts(["-c:s", "mov_text"]);
    }

    // One H.264 rule, shared with Motion and lip-sync (rightkit_ffmpeg::h264_rule): the platform
    // hardware encoder, else a typed error. MPEG-4 only when the caller asked for it, and never
    // when require_h264 is set.
    let require_h264 = input["require_h264"] == true;
    let allow_mpeg4 = input["allow_mpeg4_fallback"] == true && !require_h264;
    let (encoder, is_h264) = match h264::decide(&caps.encoders, None, allow_mpeg4) {
        h264::Pick::H264(name) => (name, true),
        h264::Pick::Mpeg4Fallback => ("mpeg4", false),
        h264::Pick::Unavailable => {
            let prefix = if require_h264 {
                "still_clip requires an available H.264 encoder: "
            } else {
                ""
            };
            return Err(GenError::new(
                h264::CODE,
                format!("{prefix}{}", h264::unavailable_message()),
            )
            .with_details(h264::details(None, &caps.encoders, None)));
        }
    };
    cmd.text("-c:v").text(encoder);
    match encoder {
        "mpeg4" => {
            cmd.texts(["-q:v", "4", "-pix_fmt", "yuv420p"]);
        }
        "libx264" => {
            cmd.texts(["-preset", "veryfast", "-crf", "20", "-pix_fmt", "yuv420p"]);
        }
        "h264_videotoolbox" => {
            let bps = (w as u64 * h as u64 * fps as u64 * 12 / 100).max(400_000);
            cmd.text("-b:v")
                .text(bps.to_string())
                .texts(["-allow_sw", "1", "-pix_fmt", "yuv420p"]);
        }
        other => {
            let bps = (w as u64 * h as u64 * fps as u64 * 12 / 100).max(400_000);
            let _ = is_h264;
            cmd.text("-b:v")
                .text(bps.to_string())
                .texts(["-pix_fmt", "yuv420p"]);
            let _ = other;
        }
    }
    if has_audio_in {
        cmd.texts(["-c:a", "aac", "-b:a", "160k"]);
        if timed && audio.is_empty() {
            // Music-only with a known video length: bound the output to the video.
            // Same Display formatting as the filter graph (12.5 -> 12.5, 30 -> 30).
            cmd.text("-t").text(format!("{video_duration}"));
        } else if subs_in.is_some() && timed {
            // `-shortest` counts the SRT stream and cuts at the last caption.
            cmd.text("-t").text(format!("{video_duration:.3}"));
        } else if !audio.is_empty() || !timed {
            cmd.text("-shortest");
        }
    }
    cmd.texts(["-movflags", "+faststart"]);
    cmd.path(out);
    Ok(FfmpegBuild {
        args: argv(&cmd),
        video_duration_s: video_duration,
        has_audio_out: has_audio_in,
        subtitle_mode,
        encoder,
    })
}

/// The builder's argv as the strings `media::run` executes (paths are UTF-8 here, so the
/// lossy conversion is exact).
fn argv(cmd: &FfmpegCommand) -> Vec<String> {
    cmd.args()
        .iter()
        .map(|a| match a {
            FfmpegArg::Text(t) => t.clone(),
            FfmpegArg::Path(p) => p.to_string_lossy().into_owned(),
        })
        .collect()
}

/// Adds the located ffmpeg path, the listing it offered and its own message to an
/// `h264_encoder_unavailable` error from `build_args`. Other errors pass through unchanged.
fn with_ffmpeg(
    mut e: GenError,
    ffmpeg: &impl FfmpegBinary,
    listing: &str,
    ffmpeg_message: Option<&str>,
) -> GenError {
    if e.code == h264::CODE {
        e.details = Some(h264::details(
            Some(ffmpeg.ffmpeg_path()),
            listing,
            ffmpeg_message,
        ));
    }
    e
}

/// Pass 1 of two-pass loudnorm: analyse the audio chain exactly as the encode will build it.
/// `Ok(None)` means the spec has no audio, so there is nothing to normalize.
fn measure_loudnorm(
    input: &Value,
    out: &Path,
    caps: &media::FfCaps,
    ffmpeg: &impl FfmpegBinary,
) -> Result<Option<Measured>, String> {
    let spec = build_args(input, out, caps, Loudnorm::Measure).map_err(|e| e.message)?;
    if !spec.has_audio_out {
        return Ok(None);
    }
    let (ok, _, stderr) =
        media::run(ffmpeg.ffmpeg_path(), &spec.args, "ffmpeg").map_err(|e| e.message)?;
    if !ok {
        return Err("pass 1 ffmpeg run failed".into());
    }
    parse_loudnorm_json(&stderr).map(Some)
}

/// Two-pass loudnorm (EBU R128) per the FFmpeg loudnorm filter docs: pass 1 measures the audio
/// with `print_format=json`, pass 2 applies the measured values with `linear=true`. When pass 1
/// cannot be measured or parsed, the encode falls back to single-pass loudnorm and records a
/// warning. The loudness report at the end is still measured from the produced file.
pub fn compose(ctx: &crate::Context, input: &Value) -> GenResult<Value> {
    let t = media::tools();
    let ffmpeg = t.ffmpeg.clone().ok_or_else(|| {
        GenError::new(
            "tool_missing",
            format!(
                "no verified ffmpeg: {}",
                t.ffmpeg_error.as_deref().unwrap_or("unresolved")
            ),
        )
    })?;
    let out_dir = PathBuf::from(if s(input, "out_dir").is_empty() {
        ctx.data_dir
            .join("pipelines")
            .join("media")
            .to_string_lossy()
            .into_owned()
    } else {
        s(input, "out_dir")
    });
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| GenError::new("io", format!("cannot create {}: {e}", out_dir.display())))?;
    let out_path = PathBuf::from(if s(input, "out_path").is_empty() {
        out_dir
            .join(format!(
                "compose-{}.mp4",
                crate::pipeline_workflows::stamp()
            ))
            .to_string_lossy()
            .into_owned()
    } else {
        s(input, "out_path")
    });

    // A failed -encoders probe is reported as itself, never as "no encoder".
    let listing = h264::probe_listing(&ffmpeg).map_err(|e| GenError::new("ffmpeg_failed", e))?;
    let mut caps = media::ffmpeg_caps(&ffmpeg);
    caps.encoders = listing;
    let mut resolved = input.clone();
    struct PreparedStills(Vec<PathBuf>);
    impl Drop for PreparedStills {
        fn drop(&mut self) {
            for p in &self.0 {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let mut converted = PreparedStills(Vec::new());
    if !caps.has_decoder("png") && caps.has_decoder("mjpeg") {
        for item in resolved["inputs"].as_array_mut().into_iter().flatten() {
            let path = PathBuf::from(s(item, "path"));
            if path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("png"))
            {
                let jpeg = out_dir.join(format!("still-{}.jpg", super::stamp()));
                image::open(&path)
                    .and_then(|img| img.to_rgb8().save(&jpeg))
                    .map_err(|e| {
                        GenError::new(
                            "decode_unsupported",
                            format!("cannot prepare {}: {e}", path.display()),
                        )
                    })?;
                item["path"] = json!(jpeg.to_string_lossy());
                converted.0.push(jpeg);
            }
        }
    }
    let input = &resolved;
    let ken_burns_requested = input
        .get("ken_burns")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // An H.264 encoder that refuses to start (an initialisation error in ffmpeg's own output)
    // is removed from the listing and the shared rule decides again: the next H.264 candidate,
    // MPEG-4 if requested, or h264_encoder_unavailable. Any other failure is reported unchanged:
    // it is never retried and never called an encoder problem.
    let mut listing = caps.encoders.clone();
    let mut refused: Vec<&'static str> = Vec::new();
    let mut refused_message: Option<String> = None;
    let mut loudnorm_warnings: Vec<String> = Vec::new();
    let normalize = input
        .get("normalize_loudness")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let measured = if normalize {
        match measure_loudnorm(input, &out_path, &caps, &ffmpeg) {
            Ok(m) => m,
            Err(e) => {
                loudnorm_warnings.push(format!(
                    "loudnorm pass 1 could not be measured ({e}); used single-pass loudnorm"
                ));
                None
            }
        }
    } else {
        None
    };
    let norm = match measured.as_ref() {
        Some(m) => Loudnorm::Linear(m),
        None => Loudnorm::Single,
    };
    let (spec, ok, stderr) = loop {
        let attempt = media::FfCaps {
            encoders: listing.clone(),
            decoders: caps.decoders.clone(),
            filters: caps.filters.clone(),
        };
        let spec = build_args(input, &out_path, &attempt, norm)
            .map_err(|e| with_ffmpeg(e, &ffmpeg, &listing, refused_message.as_deref()))?;
        let (ok, _, stderr) = media::run(ffmpeg.ffmpeg_path(), &spec.args, "ffmpeg")?;
        let failed = !ok || !out_path.is_file();
        if !failed || spec.encoder == "mpeg4" || !h264::is_init_failure(&stderr) {
            break (spec, ok, stderr);
        }
        refused.push(spec.encoder);
        listing = listing
            .lines()
            .filter(|l| l.split_whitespace().nth(1) != Some(spec.encoder))
            .collect::<Vec<_>>()
            .join("\n");
        refused_message = Some(stderr);
        let _ = std::fs::remove_file(&out_path);
    };
    if !ok || !out_path.is_file() {
        return Err(GenError::new(
            "compose_failed",
            format!(
                "ffmpeg exited {}: {}",
                ok,
                stderr
                    .trim()
                    .chars()
                    .rev()
                    .take(900)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            ),
        ));
    }

    // Verification: full decode, probe, loudness measure.
    let decode_ok = media::decode_check(&ffmpeg, &out_path).unwrap_or(false);
    let probe = media::probe(&out_path).ok();
    let measured = media::loudness(&ffmpeg, &out_path).ok();
    let bytes = std::fs::read(&out_path).map_err(|e| GenError::new("io", e.to_string()))?;
    let mut warnings: Vec<String> = loudnorm_warnings;
    if !decode_ok {
        warnings.push("decode check reported errors".into());
    }
    if ken_burns_requested && !caps.has_filter("zoompan") {
        warnings.push("ken_burns requested but resolved ffmpeg lacks the zoompan filter; stills rendered static".into());
    }
    if spec.subtitle_mode == "unavailable" {
        warnings.push("subtitles requested but resolved ffmpeg lacks both the subtitles filter (burn) and mov_text muxing".into());
    }
    if spec.subtitle_mode == "soft"
        && input
            .get("burn_subtitles")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    {
        warnings.push("burn requested but resolved ffmpeg lacks the subtitles filter; delivered a mov_text soft track instead".into());
    }
    for failed in &refused {
        warnings.push(format!(
            "{failed} failed to start; re-encoded with {}",
            spec.encoder
        ));
    }
    let fallback = spec.encoder == "mpeg4";
    let fallback_note = fallback.then_some(h264::MPEG4_NOTE);
    if let Some(note) = fallback_note {
        warnings.push(note.into());
    }
    if spec.has_audio_out {
        if let Some(m) = &measured {
            let i = m["integrated_lufs"].as_f64().unwrap_or(0.0);
            if (i - TARGET_I).abs() > 3.0 {
                warnings.push(format!(
                    "integrated loudness {i:.1} LUFS is >3 LU from target {TARGET_I}"
                ));
            }
        }
    }

    Ok(json!({
        "ok": decode_ok,
        "out_path": out_path.to_string_lossy(),
        "sha256": rightkit_media_gen::library::sha256_hex(&bytes),
        "bytes": bytes.len(),
        "mime": "video/mp4",
        "video_duration_s": spec.video_duration_s,
        "encoder": spec.encoder,
        "video_codec": spec.encoder,
        "fallback": fallback,
        "fallback_note": fallback_note,
        "subtitle_mode": spec.subtitle_mode,
        "probe": probe,
        "loudness": measured,
        "decode_ok": decode_ok,
        "warnings": warnings,
        "command_tail": spec.args.iter().skip(spec.args.len().saturating_sub(6)).cloned().collect::<Vec<_>>(),
    }))
}
