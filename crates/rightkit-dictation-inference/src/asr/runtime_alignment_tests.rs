use super::{
    onnx_alignment_accepted, onnx_alignment_bounds, onnx_best_exact_candidate,
    onnx_exact_suffix_score,
};

#[test]
fn exact_lattice_allows_same_frame_zero_duration_tokens_then_terminal_blank() {
    // Two token emissions share frame zero; final blank closes path on same frame.
    let blank = vec![vec![-0.4], vec![-0.4], vec![-0.2]];
    let tokens = vec![vec![-1.0], vec![-1.5]];
    let (total, mean) = onnx_exact_suffix_score(0.0, &blank, &tokens, 2).unwrap();
    assert!((total + 2.7).abs() < 1e-6);
    assert!((mean + 1.25).abs() < 1e-6);
}

#[test]
fn exact_candidate_selection_uses_total_path_score_not_token_mean() {
    let candidates = [(3, -20.0, -2.0), (7, -21.0, -1.0)];
    assert_eq!(onnx_best_exact_candidate(&candidates), Some((3, -2.0)));
    assert_eq!(
        onnx_best_exact_candidate(&[(3, -20.0, -2.0), (7, -20.0, -1.0)]),
        Some((3, -2.0))
    );
}

#[test]
fn bounds_are_floored_exclusive_and_clamped_to_available_frames() {
    assert_eq!(onnx_alignment_bounds(0.159, 20), (1, 19));
    assert_eq!(onnx_alignment_bounds(-2.0, 20), (0, 17));
    assert_eq!(onnx_alignment_bounds(99.0, 20), (19, 20));
}

#[test]
fn rejected_mean_survives_as_uncertain_alignment() {
    assert!(!onnx_alignment_accepted(-6.8001));
    assert!(onnx_alignment_accepted(-6.8));
    assert!(!onnx_alignment_accepted(f32::NAN));
}
