// Onboarding failure probe: compares mel & encoder stage outputs across compute
// profiles on identical input without retaining PCM or uploading tensors.
use std::path::Path;

use objc2::rc::autoreleasepool;

use crate::coreml::{ml_f16, ml_i32, read_f16, Stage};

use super::model::{stage_path, PipelineMeta};

/// Compare CoreML mel & encoder stages on identical input without retaining
/// PCM or uploading tensors. Runs only when onboarding's preferred route fails.
pub fn compute_differential_probe(
    dir: &Path,
    conditioned: &[f32],
) -> Result<std::collections::BTreeMap<String, f64>, String> {
    let meta = PipelineMeta::from_dir(dir)?;
    let win_samples = (meta.window_sec * 16_000.0) as usize;
    let mut padded = vec![0.0f32; win_samples];
    let real_samples = conditioned.len().min(win_samples);
    padded[..real_samples].copy_from_slice(&conditioned[..real_samples]);
    let mel_path = stage_path(dir, "MelSpectrogram");
    let encoder_path = stage_path(dir, "AudioEncoder");
    let run_mel = |stage: &Stage, output: &str| -> Result<Vec<f32>, String> {
        autoreleasepool(|_| {
            let audio = ml_f16(&[1, win_samples], &padded)?;
            let len = ml_i32(&[1], &[real_samples as i32])?;
            stage
                .predict(&[("audio", &audio), ("audio_len", &len)], output)
                .map(|out| read_f16(&out))
        })
    };
    let (mel_ane_first, mel_ane_repeat) = {
        let stage = Stage::load_with_compute_profile(&mel_path, "neural_engine")?;
        let output = stage
            .output_names()
            .into_iter()
            .next()
            .ok_or("MelSpectrogram has no output")?;
        (run_mel(&stage, &output)?, run_mel(&stage, &output)?)
    };
    let mel_gpu_output = {
        let stage = Stage::load_with_compute_profile(&mel_path, "cpu_gpu")?;
        let output = stage
            .output_names()
            .into_iter()
            .next()
            .ok_or("MelSpectrogram has no output")?;
        run_mel(&stage, &output)?
    };
    let slice_mel = |full: &[f32]| {
        let full_frames = full.len() / meta.n_mel;
        let mut sliced = vec![0.0f32; meta.n_mel * meta.mel_frames];
        for channel in 0..meta.n_mel {
            for frame in 0..meta.mel_frames {
                sliced[channel * meta.mel_frames + frame] = full[channel * full_frames + frame];
            }
        }
        sliced
    };
    let mel_from_ane = slice_mel(&mel_ane_first);
    let mel_from_gpu = slice_mel(&mel_gpu_output);
    let run_encoder = |stage: &Stage, output: &str, mel: &[f32]| -> Result<Vec<f32>, String> {
        autoreleasepool(|_| {
            let input = ml_f16(&[1, meta.n_mel, meta.mel_frames], mel)?;
            let len = ml_i32(&[1], &[meta.mel_frames as i32])?;
            stage
                .predict(
                    &[("melspectrogram_features", &input), ("mel_len", &len)],
                    output,
                )
                .map(|out| read_f16(&out))
        })
    };
    let (encoder_ane_first, encoder_ane_repeat) = {
        let stage = Stage::load_with_compute_profile(&encoder_path, "neural_engine")?;
        let output = stage
            .output_names()
            .into_iter()
            .next()
            .ok_or("AudioEncoder has no output")?;
        (
            run_encoder(&stage, &output, &mel_from_gpu)?,
            run_encoder(&stage, &output, &mel_from_gpu)?,
        )
    };
    let (encoder_gpu_same_mel, encoder_gpu_ane_mel) = {
        let stage = Stage::load_with_compute_profile(&encoder_path, "cpu_gpu")?;
        let output = stage
            .output_names()
            .into_iter()
            .next()
            .ok_or("AudioEncoder has no output")?;
        (
            run_encoder(&stage, &output, &mel_from_gpu)?,
            run_encoder(&stage, &output, &mel_from_ane)?,
        )
    };
    let mut metrics = std::collections::BTreeMap::new();
    append_probe_comparison(
        &mut metrics,
        "probe_mel_ane_repeat",
        &mel_ane_first,
        &mel_ane_repeat,
        None,
    );
    append_probe_comparison(
        &mut metrics,
        "probe_mel_ane_gpu",
        &mel_ane_first,
        &mel_gpu_output,
        None,
    );
    append_probe_comparison(
        &mut metrics,
        "probe_encoder_ane_repeat",
        &encoder_ane_first,
        &encoder_ane_repeat,
        None,
    );
    append_probe_comparison(
        &mut metrics,
        "probe_encoder_ane_gpu_same_mel",
        &encoder_ane_first,
        &encoder_gpu_same_mel,
        Some(meta.enc_frames),
    );
    append_probe_comparison(
        &mut metrics,
        "probe_encoder_gpu_ane_gpu_mel",
        &encoder_gpu_ane_mel,
        &encoder_gpu_same_mel,
        None,
    );
    metrics.insert("probe_complete".into(), 1.0);
    Ok(metrics)
}

fn append_probe_comparison(
    metrics: &mut std::collections::BTreeMap<String, f64>,
    prefix: &str,
    left: &[f32],
    right: &[f32],
    frames: Option<usize>,
) {
    if left.len() != right.len() || left.is_empty() {
        metrics.insert(format!("{prefix}_cosine"), -1.0);
        metrics.insert(format!("{prefix}_max_abs"), -1.0);
        return;
    }
    if left.iter().chain(right).any(|value| !value.is_finite()) {
        metrics.insert(format!("{prefix}_cosine"), -2.0);
        metrics.insert(format!("{prefix}_max_abs"), -2.0);
        return;
    }
    let (mut dot, mut left_sq, mut right_sq, mut max_abs) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (&a, &b) in left.iter().zip(right) {
        let (a, b) = (f64::from(a), f64::from(b));
        dot += a * b;
        left_sq += a * a;
        right_sq += b * b;
        max_abs = max_abs.max((a - b).abs());
    }
    metrics.insert(
        format!("{prefix}_cosine"),
        dot / (left_sq.sqrt() * right_sq.sqrt()).max(f64::EPSILON),
    );
    metrics.insert(format!("{prefix}_max_abs"), max_abs);
    if let Some(frame_count) =
        frames.filter(|count| *count > 0 && left.len().is_multiple_of(*count))
    {
        let width = left.len() / frame_count;
        let mut worst = (0usize, 1.0f64);
        for frame in 0..frame_count {
            let (mut frame_dot, mut frame_left_sq, mut frame_right_sq) = (0.0f64, 0.0f64, 0.0f64);
            for channel in 0..width {
                let index = channel * frame_count + frame;
                let (a, b) = (f64::from(left[index]), f64::from(right[index]));
                frame_dot += a * b;
                frame_left_sq += a * a;
                frame_right_sq += b * b;
            }
            let cosine =
                frame_dot / (frame_left_sq.sqrt() * frame_right_sq.sqrt()).max(f64::EPSILON);
            if cosine < worst.1 {
                worst = (frame, cosine);
            }
        }
        metrics.insert(format!("{prefix}_worst_frame"), worst.0 as f64);
        metrics.insert(format!("{prefix}_worst_frame_cosine"), worst.1);
    }
}
