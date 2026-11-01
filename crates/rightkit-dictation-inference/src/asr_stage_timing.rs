//! Debug-only stage timings for the Windows Parakeet benchmark runner.
//!
//! `decoder_joint_ms` and `predictor_ms` are measured strictly around ORT
//! `Session::run`. The corresponding `*_wrapper_ms` values also include input
//! tensor setup and output extraction; `*_wrapper_overhead_ms` is their
//! difference. `search_bookkeeping_ms` is decoder-search wall time minus full
//! synchronous wrapper calls, capturing remaining beam/search/control-flow.

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct WindowStageTiming {
    /// Absolute source-audio position at which this disjoint cursor window starts.
    pub window_start_sample: usize,
    /// Source-audio samples consumed by this cursor window, excluding any
    /// terminal-silence flush and encoder static-shape padding.
    pub source_sample_count: usize,
    /// Samples passed to the mel model after terminal-silence flush handling.
    pub model_input_sample_count: usize,
    /// Extra samples added by final-tail silence handling before mel inference.
    pub terminal_silence_samples: usize,
    /// Log-mel frames produced before static encoder padding.
    pub mel_valid_frames: usize,
    /// Static encoder input width, when fixed; otherwise equals mel frames.
    pub encoder_input_frames: usize,
    /// Zero-filled mel frames added to meet the encoder's declared static width.
    pub encoder_input_padded_frames: usize,
    /// Valid output steps reported by the encoder length tensor.
    pub encoder_output_valid_steps: usize,
    /// Encoder output steps beyond its reported valid length.
    pub encoder_output_padded_steps: usize,
    pub mel_ms: f64,
    pub encode_ms: f64,
    pub decode_ms: f64,
    pub finalize_ms: f64,
    pub decoder_joint_ms: f64,
    pub decoder_joint_wrapper_ms: f64,
    /// Wrapper-call wall minus measured `.run()` wall; includes tensor setup,
    /// output extraction/copies, and small wrapper control overhead.
    pub decoder_joint_wrapper_overhead_ms: f64,
    pub predictor_ms: f64,
    pub predictor_wrapper_ms: f64,
    pub predictor_wrapper_overhead_ms: f64,
    /// Count of synchronous decoder/joint and split-predictor wrapper calls.
    pub decoder_calls: usize,
    pub beam_width_effective: usize,
    /// Seconds from start of input to start of this model window.
    pub window_start_sec: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordingStageTiming {
    pub mel_ms: f64,
    pub encode_ms: f64,
    pub decode_ms: f64,
    /// Directly measured detokenization/window assembly time.
    pub finalize_ms: f64,
    pub n_windows: usize,
    pub decoder_calls: usize,
    pub beam_width_effective: usize,
    /// Sum of decoder/joint ORT `.run()` wall time only.
    pub decoder_joint_ms: f64,
    /// Sum of full synchronous decoder/joint wrapper-call wall time.
    pub decoder_joint_wrapper_ms: f64,
    /// Wrapper-call wall minus `.run()` wall: tensor setup, output copies,
    /// and small wrapper control cost.
    pub decoder_joint_wrapper_overhead_ms: f64,
    /// Sum of split predictor ORT `.run()` wall time only.
    pub predictor_ms: f64,
    /// Sum of full synchronous split predictor wrapper-call wall time.
    pub predictor_wrapper_ms: f64,
    /// Wrapper-call wall minus `.run()` wall for split predictor.
    pub predictor_wrapper_overhead_ms: f64,
    /// Decoder-search wall outside measured joint/predictor wrapper calls.
    pub search_bookkeeping_ms: f64,
    pub windows: Vec<WindowStageTiming>,
}

impl RecordingStageTiming {
    pub fn from_windows(windows: Vec<WindowStageTiming>, finalize_ms: f64) -> Self {
        let mut result = Self {
            finalize_ms: finalize_ms.max(0.0),
            n_windows: windows.len(),
            windows,
            ..Self::default()
        };
        for window in &result.windows {
            result.mel_ms += window.mel_ms;
            result.encode_ms += window.encode_ms;
            result.decode_ms += window.decode_ms;
            result.finalize_ms += window.finalize_ms;
            result.decoder_joint_ms += window.decoder_joint_ms;
            result.decoder_joint_wrapper_ms += window.decoder_joint_wrapper_ms;
            result.decoder_joint_wrapper_overhead_ms +=
                (window.decoder_joint_wrapper_ms - window.decoder_joint_ms).max(0.0);
            result.predictor_ms += window.predictor_ms;
            result.predictor_wrapper_ms += window.predictor_wrapper_ms;
            result.predictor_wrapper_overhead_ms +=
                (window.predictor_wrapper_ms - window.predictor_ms).max(0.0);
            result.decoder_calls += window.decoder_calls;
            result.beam_width_effective = window.beam_width_effective;
        }
        result.search_bookkeeping_ms =
            (result.decode_ms - result.decoder_joint_wrapper_ms - result.predictor_wrapper_ms)
                .max(0.0);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(decode_ms: f64, joint_ms: f64, predictor_ms: f64, calls: usize) -> WindowStageTiming {
        WindowStageTiming {
            window_start_sample: 0,
            source_sample_count: 239_000,
            model_input_sample_count: 240_000,
            terminal_silence_samples: 1_000,
            mel_valid_frames: 1_490,
            encoder_input_frames: 1_500,
            encoder_input_padded_frames: 10,
            encoder_output_valid_steps: 187,
            encoder_output_padded_steps: 0,
            mel_ms: 2.0,
            encode_ms: 3.0,
            decode_ms,
            finalize_ms: 0.5,
            decoder_joint_ms: joint_ms,
            decoder_joint_wrapper_ms: joint_ms + 1.0,
            decoder_joint_wrapper_overhead_ms: 1.0,
            predictor_ms,
            predictor_wrapper_ms: predictor_ms + 2.0,
            predictor_wrapper_overhead_ms: 2.0,
            decoder_calls: calls,
            beam_width_effective: 3,
            window_start_sec: 0.0,
        }
    }

    #[test]
    fn aggregation_sums_windows_and_preserves_actual_call_counts() {
        let mut second = window(30.0, 13.0, 4.0, 17);
        second.window_start_sample = 240_000;
        let result =
            RecordingStageTiming::from_windows(vec![window(20.0, 9.0, 2.0, 11), second], 1.5);
        assert_eq!(result.n_windows, 2);
        assert_eq!(result.mel_ms, 4.0);
        assert_eq!(result.encode_ms, 6.0);
        assert_eq!(result.decode_ms, 50.0);
        assert_eq!(result.finalize_ms, 2.5);
        assert_eq!(result.decoder_calls, 28);
        assert_eq!(result.beam_width_effective, 3);
        assert_eq!(result.decoder_joint_wrapper_ms, 24.0);
        assert_eq!(result.decoder_joint_wrapper_overhead_ms, 2.0);
        assert_eq!(result.predictor_wrapper_ms, 10.0);
        assert_eq!(result.predictor_wrapper_overhead_ms, 4.0);
        assert_eq!(result.search_bookkeeping_ms, 16.0);
        assert_eq!(result.windows[1].window_start_sample, 240_000);
        assert_eq!(result.windows[0].source_sample_count, 239_000);
        assert_eq!(result.windows[0].terminal_silence_samples, 1_000);
        assert_eq!(result.windows[0].encoder_input_padded_frames, 10);
        assert_eq!(result.windows[0].encoder_output_valid_steps, 187);
    }

    #[test]
    fn empty_recording_has_empty_metrics_without_reusing_prior_values() {
        let result = RecordingStageTiming::from_windows(Vec::new(), 0.0);
        assert_eq!(result, RecordingStageTiming::default());
    }
}
