//! Public crate-facing API: thin request builders over the worker channel,
//! plus the one-shot SetValue path guarded by the circuit breaker.

use std::sync::atomic::Ordering;
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant};

use rightkit_transcript_library::delivery::TargetSnapshot;

use super::protocol::{
    FocusedElementIdentity, Request, StopTarget, TextProof, REQUEST_TIMEOUT,
    SET_VALUE_CIRCUIT_OPEN, SET_VALUE_IN_FLIGHT,
};
use super::worker::{worker, worker_loop};

fn request<T>(
    timeout: Duration,
    build: impl FnOnce(Instant, mpsc::SyncSender<T>) -> Request,
) -> Option<T> {
    if timeout.is_zero() {
        return None;
    }
    let deadline = Instant::now() + timeout;
    let (reply, receive) = mpsc::sync_channel(1);
    let request = build(deadline, reply);
    if matches!(&request, Request::SetValue { .. }) {
        if SET_VALUE_CIRCUIT_OPEN.load(Ordering::Acquire)
            || SET_VALUE_IN_FLIGHT
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        if std::thread::Builder::new()
            .name("windows-uia-set-value".to_string())
            .spawn(move || worker(request, true))
            .is_err()
        {
            SET_VALUE_IN_FLIGHT.store(false, Ordering::Release);
            return None;
        }
        return match receive.recv_timeout(timeout) {
            Ok(value) => Some(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The detached setter may still mutate later. Permanently
                // open circuit prevents another native insertion attempt.
                SET_VALUE_CIRCUIT_OPEN.store(true, Ordering::Release);
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => None,
        };
    }

    read_only_sender()?.try_send(request).ok()?;
    receive.recv_timeout(timeout).ok()
}

fn read_only_sender() -> Option<&'static mpsc::SyncSender<Request>> {
    static SENDER: OnceLock<Option<mpsc::SyncSender<Request>>> = OnceLock::new();
    SENDER
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("windows-uia-mta".to_string())
                .spawn(move || worker_loop(rx))
                .ok()
                .map(|_| tx)
        })
        .as_ref()
}

pub(crate) fn focused_text_input() -> Option<bool> {
    request(REQUEST_TIMEOUT, |deadline, reply| Request::Editable {
        deadline,
        reply,
    })
    .flatten()
}

pub(crate) fn focused_field_is_password() -> Option<bool> {
    request(REQUEST_TIMEOUT, |deadline, reply| Request::Password {
        deadline,
        reply,
    })
    .flatten()
}

// Dormant by design: native insertion is gated off (docs/dispatch/evidence/
// native-insertion-capability-gated.md) but the contract tests pin this
// one-shot circuit-breaker path, so the entry point stays until the gate opens.
#[allow(dead_code)]
pub(crate) fn native_insertion_capability() -> bool {
    request(REQUEST_TIMEOUT, |deadline, reply| {
        Request::NativeCapability { deadline, reply }
    })
    .flatten()
    .unwrap_or(false)
}

pub(crate) fn focused_element_identity(target: &TargetSnapshot) -> Option<FocusedElementIdentity> {
    let process_id = target.process_id? as i32;
    let window_handle = target.window_handle?;
    request(REQUEST_TIMEOUT, |deadline, reply| Request::Identity {
        deadline,
        process_id,
        control_handle: target.focused_control_handle.unwrap_or_default(),
        window_handle,
        reply,
    })
    .flatten()
}

pub(crate) fn focused_text_with_identity(
    target: &TargetSnapshot,
    expected: &FocusedElementIdentity,
    timeout: Duration,
) -> Option<TextProof> {
    request(timeout, |deadline, reply| Request::ReadText {
        deadline,
        target: target.clone(),
        expected: expected.clone(),
        reply,
    })
    .flatten()
}

pub(crate) fn stop_target_text(stop: &StopTarget, timeout: Duration) -> Option<TextProof> {
    request(timeout, |deadline, reply| Request::ReadStopText {
        deadline,
        stop: stop.clone(),
        reply,
    })
    .flatten()
}

// See `native_insertion_capability`: dormant until native insertion is enabled.
#[allow(dead_code)]
pub(crate) fn set_value(
    text: &str,
    target: &TargetSnapshot,
    expected: &FocusedElementIdentity,
) -> bool {
    request(REQUEST_TIMEOUT, |deadline, reply| Request::SetValue {
        deadline,
        text: text.to_owned(),
        target: target.clone(),
        expected: expected.clone(),
        reply,
    })
    .flatten()
    .unwrap_or(false)
}

pub(crate) fn capture_stop_target(target: TargetSnapshot) -> Option<StopTarget> {
    request(REQUEST_TIMEOUT, |deadline, reply| {
        Request::CaptureStopTarget {
            deadline,
            target,
            reply,
        }
    })
    .flatten()
}

pub(crate) fn validate_stop_target(stop: &StopTarget) -> bool {
    request(REQUEST_TIMEOUT, |deadline, reply| {
        Request::ValidateStopTarget {
            deadline,
            stop: stop.clone(),
            reply,
        }
    })
    .unwrap_or(false)
}

pub(crate) fn stop_target_is_gone(stop: &StopTarget) -> Option<bool> {
    request(REQUEST_TIMEOUT, |deadline, reply| {
        Request::StopTargetIsGone {
            deadline,
            stop: stop.clone(),
            reply,
        }
    })
    .flatten()
}

pub(crate) fn restore_stop_target(stop: &StopTarget) -> bool {
    request(REQUEST_TIMEOUT, |deadline, reply| {
        Request::RestoreStopTarget {
            deadline,
            stop: stop.clone(),
            reply,
        }
    })
    .unwrap_or(false)
}

/// UIA counterpart of macOS web-area binding: true only when the retained,
/// non-secure, editable stop element sits under a Document ancestor whose
/// native window is the control named by the hotkey proof.
pub(crate) fn stop_target_names_web_container(
    stop: &StopTarget,
    hotkey_control: isize,
    window_handle: isize,
) -> bool {
    request(REQUEST_TIMEOUT, |deadline, reply| {
        Request::WebContainerNamed {
            deadline,
            stop: stop.clone(),
            hotkey_control,
            window_handle,
            reply,
        }
    })
    .unwrap_or(false)
}

pub(crate) fn release_stop_target(token: u64) {
    if token == 0 {
        return;
    }
    let Some(sender) = read_only_sender() else {
        return;
    };
    let (reply, _receive) = mpsc::sync_channel(1);
    let _ = sender.try_send(Request::ReleaseStopTarget { token, reply });
}
