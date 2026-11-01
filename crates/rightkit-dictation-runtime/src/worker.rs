#[cfg(test)]
mod replay_contract;
#[cfg(test)]
mod replay_driver;
#[cfg(test)]
mod replay_effect_sink;
#[cfg(test)]
mod wav_replay_source;

mod capture_device;
mod capture_recovery;
mod probe_decisions;
mod recording_session;
mod streaming_commit;
mod transcript_text;
mod unified_probe_lane;
mod wake_listener;
mod windows_lifecycle;
mod worker_api;
mod worker_clock;
mod worker_main;
#[cfg(test)]
mod worker_tests;

pub use unified_probe_lane::FileProgress;

pub use worker_api::{
    spawn_dummy_worker, spawn_worker, ControlCutStatus, FinalTranscript, StopEvidence, WorkerCmd,
    WorkerEvent, WorkerHandle,
};

/// Small policy surface for deterministic replay tools. These wrappers keep
/// replay scheduling tied to the same production predicates & constants.
#[cfg(target_os = "windows")]
pub mod replay_policy {
    use super::{probe_decisions, worker_api};

    pub fn tail_probe_due(total_samples: usize, audio_arrived: bool) -> bool {
        probe_decisions::tail_probe_due(total_samples, audio_arrived)
    }

    pub fn tail_prefix_expired_samples(elapsed_samples: usize) -> bool {
        probe_decisions::tail_prefix_expired(std::time::Duration::from_secs_f64(
            elapsed_samples as f64 / worker_api::SAMPLE_RATE as f64,
        ))
    }

    pub fn full_tail_fallback_due(candidate_armed: bool, new_audio_samples: usize) -> bool {
        probe_decisions::should_run_full_tail_fallback("", candidate_armed, new_audio_samples)
    }

    pub fn candidate_armed_after_result(text: &str) -> bool {
        let control_found = matches!(text, "zephyr stop" | "zephyr send" | "zephyr cancel");
        let saw_candidate = rightkit_text_polish::text_pipeline::has_trailing_control_wake(text)
            || probe_decisions::tail_text_ends_with_control_verb(text);
        probe_decisions::should_keep_control_candidate_armed(control_found, saw_candidate)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(crate) use rightkit_dictation_inference::{resolve_trigger_audio_cut, TriggerAudioCut};
