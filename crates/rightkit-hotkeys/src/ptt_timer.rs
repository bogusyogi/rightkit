//! One bounded, cancellable push-to-talk arm timer.
//!
//! The donor spawned a sleeping thread per key press. Releasing the key only invalidated the
//! sleeper logically, so quick taps piled up threads and a dropped tap could still fire a start
//! callback. [`ArmTimer`] owns exactly one worker thread and at most one pending deadline: arming
//! replaces the pending deadline, [`ArmTimer::cancel`] clears it, and dropping the timer stops the
//! thread without ever firing.

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

/// Longest arm delay a caller can request; longer values are clamped so a timer is always bounded.
pub const MAX_ARM_DELAY: Duration = Duration::from_millis(1_000);

enum Msg {
    Arm { generation: u64, deadline: Instant },
    Cancel,
    Shutdown,
}

pub struct ArmTimer {
    tx: Sender<Msg>,
}

impl ArmTimer {
    /// Start the worker. `on_fire(generation)` runs on the worker thread when an armed deadline
    /// passes without being cancelled or replaced. Sending to the timer never blocks.
    pub fn spawn(name: &str, on_fire: impl Fn(u64) + Send + 'static) -> std::io::Result<ArmTimer> {
        let (tx, rx) = mpsc::channel::<Msg>();
        thread::Builder::new().name(name.into()).spawn(move || {
            let mut pending: Option<(u64, Instant)> = None;
            loop {
                let msg = match pending {
                    None => match rx.recv() {
                        Ok(msg) => msg,
                        Err(_) => return,
                    },
                    Some((generation, deadline)) => {
                        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                            Ok(msg) => msg,
                            Err(RecvTimeoutError::Timeout) => {
                                pending = None;
                                on_fire(generation);
                                continue;
                            }
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                };
                match msg {
                    Msg::Arm {
                        generation,
                        deadline,
                    } => pending = Some((generation, deadline)),
                    Msg::Cancel => pending = None,
                    Msg::Shutdown => return,
                }
            }
        })?;
        Ok(ArmTimer { tx })
    }

    /// Arm (or re-arm, replacing any pending deadline) to fire `generation` after `delay`, clamped
    /// to [`MAX_ARM_DELAY`].
    pub fn arm(&self, generation: u64, delay: Duration) {
        let _ = self.tx.send(Msg::Arm {
            generation,
            deadline: Instant::now() + delay.min(MAX_ARM_DELAY),
        });
    }

    /// Drop any pending deadline. A fire already running is not interrupted; callers re-check their
    /// generation inside `on_fire`.
    pub fn cancel(&self) {
        let _ = self.tx.send(Msg::Cancel);
    }
}

impl Drop for ArmTimer {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}
