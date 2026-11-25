//! The phone alert sender seam and the retry loop around it (moved from Pulse's `engine/src/push.rs`).
//!
//! [`PhoneAlertSender`] is the only thing that talks to Apple. Until a host installs one in a
//! [`SenderSlot`], a [`NotConfiguredSender`] answers `NotConfigured` and tells the host once.
//! [`deliver`] looks up the stored registration, hands the alert to the sender up to
//! [`ATTEMPTS`] times while the answer is `Retry` (waiting one second, then two), and tells the
//! token store when Apple says the token is dead.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::file_store::{FileTokenStore, Registration};
use super::{PhoneAlert, PushError};

/// Sends tried for one alert in one pass; a `Retry` waits a second and then two.
pub const ATTEMPTS: u32 = 3;

/// What one send came to (`PushOutcome`, plus `NotConfigured`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    /// Apple says the token is dead; the stored one is removed.
    TokenGone,
    /// Try again shortly (a throttle, a server error, no answer).
    Retry,
    /// Refused for another reason; not retried. The text is Apple's short reason, never the token.
    Rejected(String),
    /// No sender is installed.
    NotConfigured,
}

/// Sends one alert to one phone. The implementation must not log the registration or the alert.
pub trait PhoneAlertSender: Send + Sync {
    fn send(&self, device_id: &str, registration: &Registration, alert: &PhoneAlert) -> Outcome;
}

/// The sender until a real one is installed: sends nothing and calls `on_first` once.
pub struct NotConfiguredSender {
    warned: AtomicBool,
    on_first: Box<dyn Fn() + Send + Sync>,
}

impl NotConfiguredSender {
    pub fn new(on_first: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            warned: AtomicBool::new(false),
            on_first: Box::new(on_first),
        }
    }
}

impl PhoneAlertSender for NotConfiguredSender {
    fn send(&self, _device_id: &str, _registration: &Registration, _alert: &PhoneAlert) -> Outcome {
        if !self.warned.swap(true, Ordering::SeqCst) {
            (self.on_first)();
        }
        Outcome::NotConfigured
    }
}

/// The installed sender: a [`NotConfiguredSender`] until the host calls [`SenderSlot::set`].
pub struct SenderSlot {
    slot: Mutex<Arc<dyn PhoneAlertSender>>,
}

impl SenderSlot {
    pub fn new(on_not_configured: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            slot: Mutex::new(Arc::new(NotConfiguredSender::new(on_not_configured))),
        }
    }

    /// Installs the sender that talks to Apple. Called once at startup by the host that has the key.
    pub fn set(&self, sender: Arc<dyn PhoneAlertSender>) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = sender;
    }

    pub fn get(&self) -> Arc<dyn PhoneAlertSender> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Apple's reason, cut to what is safe to log: ASCII letters and digits, 64 at most.
pub fn loggable_reason(reason: &str) -> String {
    reason
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(64)
        .collect()
}

/// Sends `alert` to `device_id`, up to [`ATTEMPTS`] times while the answer is `Retry`. A phone with
/// no stored token answers `TokenGone`. The store is told when Apple says the token is dead
/// (removed only while it is still the one sent to). `on_rejected` gets the loggable reason of a
/// `Rejected` answer.
pub fn deliver(
    store: &FileTokenStore,
    sender: &dyn PhoneAlertSender,
    device_id: &str,
    alert: &PhoneAlert,
    on_rejected: &dyn Fn(&str),
) -> Outcome {
    let Some(registration) = store.get(device_id) else {
        return Outcome::TokenGone;
    };
    let mut outcome = Outcome::Retry;
    for attempt in 0..ATTEMPTS {
        outcome = sender.send(device_id, &registration, alert);
        if outcome != Outcome::Retry {
            break;
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(Duration::from_secs(1 << attempt));
        }
    }
    match &outcome {
        Outcome::TokenGone => {
            store.remove(device_id, Some(&registration.token));
        }
        Outcome::Rejected(reason) => on_rejected(&loggable_reason(reason)),
        Outcome::Delivered | Outcome::Retry | Outcome::NotConfigured => {}
    }
    outcome
}

/// The fixed word an item alert's outcome is reported as: `Ok` means Apple accepted it.
pub fn item_alert_result(outcome: Outcome) -> Result<(), String> {
    match outcome {
        Outcome::Delivered => Ok(()),
        Outcome::TokenGone => Err("push_token_gone".to_string()),
        Outcome::Retry => Err("push_retry".to_string()),
        Outcome::Rejected(_) => Err("push_rejected".to_string()),
        Outcome::NotConfigured => Err("push_not_configured".to_string()),
    }
}

/// The fixed word for an alert that could not be built: `bad_title` or `bad_item_id`.
pub fn alert_error_code(error: &PushError) -> &'static str {
    match error {
        PushError::BadItemId => "bad_item_id",
        _ => "bad_title",
    }
}
