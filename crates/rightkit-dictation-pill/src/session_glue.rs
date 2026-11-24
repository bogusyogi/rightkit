//! Platform-free glue a host would otherwise re-type around the pill's single-flight retry and
//! the visibility guardian: the "reveal again once the splash is gone" schedule and one guardian
//! batch (sample fullscreen, hop to the main thread, apply, run the invariant).
//!
//! Every fact (what counts as a splash, how to reach the main thread, what a reveal does, the
//! log sink and the event names) is passed in; nothing here touches a window or the OS.

use std::time::Duration;

use crate::guardian::Due;
use crate::reveal_retry::RevealRetry;

/// Log sink: `(event, detail)`.
pub type LogSink = fn(&str, &str);

/// Names and timing for [`schedule_reveal_after_splash`].
#[derive(Debug, Clone, Copy)]
pub struct RevealAfterSplash<'a> {
    /// Name of the waiting thread.
    pub thread_name: &'a str,
    /// Poll interval while the splash is up.
    pub poll: Duration,
    /// Log event written when the reveal fires; the detail is `reason=<reason>`.
    pub retry_event: &'a str,
    /// Log event written when the OS refuses the thread; the detail is `reason=<reason> error=<err>`.
    pub error_event: &'a str,
}

/// Schedules one reveal for after the splash is gone, on `retry`'s single-flight latch.
///
/// While `splash_present()` is true the pill is kept hidden (`hide_while_splash`) and the thread
/// polls again. When it clears, `log` records `retry_event`, then `reveal_with_intro` runs if
/// `with_intro` else `reveal_preserving_active`. A refused thread is logged under `error_event`.
/// Returns whether a thread was started (`false`: one was already waiting or the spawn failed).
#[allow(clippy::too_many_arguments)]
pub fn schedule_reveal_after_splash(
    retry: &'static RevealRetry,
    config: &RevealAfterSplash<'_>,
    reason: &'static str,
    with_intro: bool,
    log: LogSink,
    splash_present: impl FnMut() -> bool + Send + 'static,
    hide_while_splash: impl FnMut() + Send + 'static,
    reveal_with_intro: impl FnOnce() + Send + 'static,
    reveal_preserving_active: impl FnOnce() + Send + 'static,
) -> bool {
    let retry_event = config.retry_event.to_string();
    let result = retry.schedule(
        config.thread_name,
        config.poll,
        splash_present,
        hide_while_splash,
        move || {
            log(&retry_event, &format!("reason={reason}"));
            if with_intro {
                reveal_with_intro();
            } else {
                reveal_preserving_active();
            }
        },
    );
    match result {
        Ok(started) => started,
        Err(error) => {
            log(
                config.error_event,
                &format!("reason={reason} error={error}"),
            );
            false
        }
    }
}

/// Where fullscreen is sampled for [`apply_guardian_batch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenSampling {
    /// On the guardian worker, before the main-thread hop (for probes that block on another
    /// process, such as an accessibility request that must not stall the UI loop).
    OnWorker,
    /// On the main thread, inside the hop.
    OnMain,
}

/// One guardian pass. Samples fullscreen via `sample_fullscreen` (on the worker or on the main
/// thread per `sampling`), hands the rest to `run_on_main` (the host's main-thread dispatcher),
/// applies the fullscreen value with `apply_fullscreen`, and, when the batch carries a
/// focus-class event (`due.invariant`), runs `enforce_invariant`. A pass whose `due.evaluate` and
/// `due.invariant` are both false still applies the fullscreen value, as a host's batch does.
pub fn apply_guardian_batch(
    due: Due,
    sampling: FullscreenSampling,
    sample_fullscreen: impl FnOnce() -> bool + Send + 'static,
    run_on_main: impl FnOnce(Box<dyn FnOnce() + Send + 'static>),
    apply_fullscreen: impl FnOnce(bool) + Send + 'static,
    enforce_invariant: impl FnOnce() + Send + 'static,
) {
    let run_invariant = due.invariant;
    match sampling {
        FullscreenSampling::OnWorker => {
            let fullscreen = sample_fullscreen();
            run_on_main(Box::new(move || {
                apply_fullscreen(fullscreen);
                if run_invariant {
                    enforce_invariant();
                }
            }));
        }
        FullscreenSampling::OnMain => run_on_main(Box::new(move || {
            apply_fullscreen(sample_fullscreen());
            if run_invariant {
                enforce_invariant();
            }
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn batch_runs_invariant_only_when_due() {
        for (invariant, expected) in [(true, 1), (false, 0)] {
            let ran = Arc::new(AtomicUsize::new(0));
            let r = ran.clone();
            apply_guardian_batch(
                Due {
                    evaluate: true,
                    invariant,
                },
                FullscreenSampling::OnWorker,
                || true,
                |f| f(),
                |fs| assert!(fs),
                move || {
                    r.fetch_add(1, Ordering::SeqCst);
                },
            );
            assert_eq!(ran.load(Ordering::SeqCst), expected);
        }
    }
}
