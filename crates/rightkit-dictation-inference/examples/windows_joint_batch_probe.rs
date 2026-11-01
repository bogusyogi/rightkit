//! Compare installed fused scalar output with batch-one & distinct batch-two rows.
//! Usage: windows_joint_batch_probe <installed-model-dir>

#[cfg(target_os = "windows")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use ndarray::Array3;
    use rightkit_asr::onnx_tdt::{ExecutionProvider, OnnxAsr, SessionOptions};
    use std::path::PathBuf;

    let model_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: windows_joint_batch_probe <installed-model-dir>")?;
    let mut model = OnnxAsr::load(&model_dir, SessionOptions::new(ExecutionProvider::Cpu))?;
    let frame = |seed: usize| {
        (0..1024)
            .map(|i| ((i * 17 + seed * 31) % 997) as f32 / 997.0 - 0.5)
            .collect::<Vec<_>>()
    };
    let frames = vec![frame(1), frame(7)];
    let tokens = vec![4, 5];
    let mut states = Vec::new();
    for row in 0..2 {
        let mut h = Array3::<f32>::zeros((2, 1, 640));
        let mut c = Array3::<f32>::zeros((2, 1, 640));
        for (i, value) in h.iter_mut().enumerate() {
            *value = ((i + row * 3) % 101) as f32 / 101.0;
        }
        for (i, value) in c.iter_mut().enumerate() {
            *value = ((i + row * 11) % 89) as f32 / 89.0;
        }
        states.push((h, c));
    }
    let state_refs: Vec<(&Array3<f32>, &Array3<f32>)> =
        states.iter().map(|(h, c)| (h, c)).collect();
    let vocab = model.vocab_pieces().len();
    let scalar0 = model.joint_step(&frames[0], tokens[0], &states[0].0, &states[0].1)?;
    let batch1 = model
        .joint_steps_batched(&frames[..1], &tokens[..1], &state_refs[..1])?
        .remove(0);
    let (valid, exact) = report(
        "batch1",
        vocab,
        &scalar0.logits,
        &scalar0.state_h,
        &scalar0.state_c,
        &batch1.0,
        &batch1.1,
        &batch1.2,
    );
    if !valid || !exact {
        return Err("batch1 must exactly match scalar logits & states, with matching argmax and finite outputs".into());
    }
    let scalar1 = model.joint_step(&frames[1], tokens[1], &states[1].0, &states[1].1)?;
    let batch2 = model.joint_steps_batched(&frames, &tokens, &state_refs)?;
    if batch2.len() != 2 {
        return Err(format!("batch2 returned {} rows", batch2.len()).into());
    }
    if !report(
        "batch2-row0",
        vocab,
        &scalar0.logits,
        &scalar0.state_h,
        &scalar0.state_c,
        &batch2[0].0,
        &batch2[0].1,
        &batch2[0].2,
    )
    .0 || !report(
        "batch2-row1",
        vocab,
        &scalar1.logits,
        &scalar1.state_h,
        &scalar1.state_c,
        &batch2[1].0,
        &batch2[1].1,
        &batch2[1].2,
    )
    .0
    {
        return Err("batch2 produced invalid shape or non-finite values".into());
    }
    assert!(model.joint_steps_batched(&[], &[], &[]).is_err());
    assert!(model
        .joint_steps_batched(&frames, &tokens[..1], &state_refs)
        .is_err());
    let invalid_h = Array3::zeros((2, 2, 640));
    let invalid_c = Array3::zeros((2, 2, 640));
    let invalid_states = vec![(&invalid_h, &invalid_c)];
    assert!(model
        .joint_steps_batched(&frames[..1], &tokens[..1], &invalid_states)
        .is_err());
    println!("invalid_empty_count_state_shapes_rejected=true");
    Ok(())
}

#[cfg(target_os = "windows")]
// A one-off probe report: each argument is a column it prints.
#[allow(clippy::too_many_arguments)]
fn report(
    label: &str,
    vocab: usize,
    scalar_logits: &[f32],
    scalar_h: &ndarray::Array3<f32>,
    scalar_c: &ndarray::Array3<f32>,
    batch_logits: &[f32],
    batch_h: &ndarray::Array3<f32>,
    batch_c: &ndarray::Array3<f32>,
) -> (bool, bool) {
    let max_diff = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max)
    };
    let argmax = |v: &[f32]| {
        v.iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
    };
    let shapes = scalar_logits.len() == batch_logits.len()
        && scalar_h.shape() == batch_h.shape()
        && scalar_c.shape() == batch_c.shape()
        && scalar_logits.len() > vocab;
    let finite = scalar_logits
        .iter()
        .chain(batch_logits)
        .chain(scalar_h.iter())
        .chain(batch_h.iter())
        .chain(scalar_c.iter())
        .chain(batch_c.iter())
        .all(|v| v.is_finite());
    let token_equal = shapes && argmax(&scalar_logits[..vocab]) == argmax(&batch_logits[..vocab]);
    let duration_equal =
        shapes && argmax(&scalar_logits[vocab..]) == argmax(&batch_logits[vocab..]);
    let logits_diff = max_diff(scalar_logits, batch_logits);
    let h_diff = max_diff(scalar_h.as_slice().unwrap(), batch_h.as_slice().unwrap());
    let c_diff = max_diff(scalar_c.as_slice().unwrap(), batch_c.as_slice().unwrap());
    println!("{label} finite={finite} shapes_match={shapes} logits_max_abs={logits_diff:.9} state_h_max_abs={h_diff:.9} state_c_max_abs={c_diff:.9} token_argmax_equal={token_equal} duration_argmax_equal={duration_equal}");
    (
        finite && shapes,
        logits_diff == 0.0 && h_diff == 0.0 && c_diff == 0.0 && token_equal && duration_equal,
    )
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("windows_joint_batch_probe is supported on Windows only");
    std::process::exit(2);
}
