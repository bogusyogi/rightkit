//! The glue a host writes around the controller, with every product value an input: installing
//! the configuration once and logging a refusal, turning a command's result into an
//! [`ActionOutcome`], the copy-last outcome, opening a route on the host's main thread and waiting
//! for it (bounded), and showing a hint or a clip card with the refusal logged.
//!
//! Moved from HeardRight `pill_native_win/mod.rs`. Event names, the sink and the wait are the
//! host's; nothing here knows a product, a command or an error string (an error value is never
//! read or logged, only its presence).

use std::sync::Once;
use std::time::{Duration, Instant};

use crate::clipboard::ClipItem;
use crate::config::{install, ActionOutcome, Config};
use crate::hint::HintContent;
use crate::ClipRefusal;

/// The host's diagnostic sink: `(event, detail)`.
pub type Sink = fn(&str, &str);

/// Install the host configuration once, before anything reaches the controller. `build` runs only
/// on the first call (through `once`, which the host declares as a `static`). A refusal is logged
/// as `<prefix>_install_error` with `reason=<code> field=<field or none>`: stable tags only, never
/// a `{:?}` of the error. The controller then stays inert: every call into it does nothing.
pub fn install_once(once: &'static Once, prefix: &str, sink: Sink, build: impl FnOnce() -> Config) {
    once.call_once(|| {
        if let Err(error) = install(build()) {
            sink(
                &format!("{prefix}_install_error"),
                &format!(
                    "reason={} field={}",
                    error.code(),
                    error.field().unwrap_or("none")
                ),
            );
        }
    });
}

/// Report a command's result as an [`ActionOutcome`]. A failure is logged under `event` as
/// `outcome=failed`; the error value is never logged (it can carry user content such as a path or
/// a title), so the event name is the only report of which command failed.
pub fn outcome_of<E>(sink: Sink, event: &str, result: Result<(), E>) -> ActionOutcome {
    match result {
        Ok(()) => ActionOutcome::Done,
        Err(_) => {
            sink(event, "outcome=failed");
            ActionOutcome::Failed
        }
    }
}

/// The two events of the copy-last action.
#[derive(Clone, Copy, Debug)]
pub struct CopyLastEvents<'a> {
    /// Logged as `outcome=failed` when the copy failed.
    pub error: &'a str,
    /// Logged as `outcome=copied|empty|failed` for every attempt.
    pub result: &'a str,
}

/// Report a copy-newest-transcript attempt for the button's check mark or disabled state.
/// `Err(error)` whose text contains `empty_marker` means there was nothing to copy
/// ([`ActionOutcome::Empty`]); any other error is a failure. The text is searched, never logged:
/// the telemetry carries the outcome only.
pub fn copy_last_outcome(
    sink: Sink,
    events: CopyLastEvents<'_>,
    result: Result<(), String>,
    empty_marker: &str,
) -> ActionOutcome {
    let outcome = match result {
        Ok(()) => ActionOutcome::Done,
        Err(error) if error.contains(empty_marker) => ActionOutcome::Empty,
        Err(_) => {
            sink(events.error, "outcome=failed");
            ActionOutcome::Failed
        }
    };
    sink(
        events.result,
        &format!(
            "outcome={}",
            match outcome {
                ActionOutcome::Done => "copied",
                ActionOutcome::Empty => "empty",
                ActionOutcome::Failed => "failed",
            }
        ),
    );
    outcome
}

/// How [`open_route_on_main`] reports.
#[derive(Clone, Copy, Debug)]
pub struct RouteLog<'a> {
    /// Event name of the result line.
    pub event: &'a str,
    /// The `action=` value of the line.
    pub action: &'a str,
}

/// Run `work` on the host's main thread and wait (at most `wait`) for its answer: `Done` only when
/// `work` returned `true`, so a card that asked for it closes on success, not on a request.
/// `schedule` hands the closure to the main thread and returns whether it was queued. The result
/// line is `action=<action> route=<route> phase=result outcome=ok|error elapsed_ms=<n>`.
pub fn open_route_on_main(
    sink: Sink,
    log: RouteLog<'static>,
    route: &'static str,
    wait: Duration,
    schedule: impl FnOnce(Box<dyn FnOnce() + Send + 'static>) -> bool,
    work: impl FnOnce() -> bool + Send + 'static,
) -> ActionOutcome {
    let started = Instant::now();
    let (reply, answer) = std::sync::mpsc::channel::<bool>();
    let queued = schedule(Box::new(move || {
        let ok = work();
        sink(
            log.event,
            &format!(
                "action={} route={route} phase=result outcome={} elapsed_ms={}",
                log.action,
                if ok { "ok" } else { "error" },
                started.elapsed().as_millis()
            ),
        );
        let _ = reply.send(ok);
    }));
    if !queued {
        return ActionOutcome::Failed;
    }
    match answer.recv_timeout(wait) {
        Ok(true) => ActionOutcome::Done,
        _ => ActionOutcome::Failed,
    }
}

/// The two events of [`show_hint_logged`].
#[derive(Clone, Copy, Debug)]
pub struct HintEvents<'a> {
    /// `reason=not_accepted` when the controller refused or dropped the card.
    pub not_shown: &'a str,
    /// `result=unknown_kind` when the host has no content for the kind.
    pub unknown_kind: &'a str,
}

/// Show `content` (the host's mapping from a hint kind, `None` for an unknown kind or one scoped to
/// another OS) through `show` (the controller's `show_hint`). `false`, and logged, when there is
/// nothing to show or `show` did not accept it. The kind itself is never logged.
pub fn show_hint_logged(
    sink: Sink,
    events: HintEvents<'_>,
    content: Option<HintContent>,
    show: impl FnOnce(HintContent) -> bool,
) -> bool {
    match content {
        Some(content) => {
            let accepted = show(content);
            if !accepted {
                sink(events.not_shown, "reason=not_accepted");
            }
            accepted
        }
        None => {
            sink(events.unknown_kind, "result=unknown_kind");
            false
        }
    }
}

/// Show a clip card through `show` (the controller's `show_clipboard`). A refusal is logged under
/// `event` as `reason=<code>` and returned so the caller does not act as if the card showed.
pub fn show_clipboard_logged(
    sink: Sink,
    event: &str,
    items: Vec<ClipItem>,
    single: bool,
    show: impl FnOnce(Vec<ClipItem>, bool) -> Result<(), ClipRefusal>,
) -> Result<(), ClipRefusal> {
    let result = show(items, single);
    if let Err(refusal) = result {
        sink(event, &format!("reason={}", refusal.code()));
    }
    result
}
