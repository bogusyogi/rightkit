//! Effect owner for gated paste & streaming input.
//!
//! Each write function consumes a [`WritePermit`] issued by `evidence_gate::authorize_with_input`,
//! redeems it and then issues the write. A refused redemption returns the "not written" value. The
//! write functions never probe: they cannot decide, they can only carry out a decision the gate
//! already made. The only reads here are the two modifier-key functions at the end of the file
//! (`settle_modifiers` before the gate, [`modifiers_quiet`] as the gate's last sample); neither
//! is called between a permit and its send.
//!
//! # Authorization is the last thing before the effect
//!
//! Every write, streaming included, is split in two:
//!
//! 1. BUILD (before authorization): `prepare_*` turns the input into an opaque [`Prepared`] batch.
//!    Building does all the lookups and allocation (virtual-key and scan codes, UTF-16 encoding,
//!    event objects). It touches nothing outside this process. Streaming (`streaming.rs`) builds
//!    its typed chunk, its Backspace batches and its Return here before it samples anything.
//! 2. SEND (after authorization): validate the permit, run the optional `before_send` hook,
//!    redeem it again against its original deadline, then call
//!    `Prepared::send_now`. The crate's effect-only primitive issues the whole batch in one call
//!    and does not wait, sleep, retry, poll the modifier keys, look up key codes or read any
//!    state. This module has no function that builds a batch from a permit: every send function
//!    takes a batch that already exists.
//!
//! The physical-modifier wait is its own pre-authorization step (`settle_modifiers`); the final
//! modifier sample, a tri-state ([`modifiers_quiet`]), is part of the authorization
//! (`evidence_gate::authorize_with_input`), which has no variant without it.
//!
//! The effect-only primitives are `rightkit_automation::desktop::{windows, macos}::PreparedInput`
//! with `prepare_*` and `send_now(&self)` from `rightkit-automation` 0.7.1. A
//! batch is single-use (a second `send_now`, or any attempt, returns false), is not `Send`, holds
//! at most `rightkit_automation::desktop::MAX_PREPARED_INPUT_EVENTS` (1,024) native events, and a
//! `prepare_*` call returns `None` when the batch is oversized, a key code cannot be resolved, an
//! event source cannot be created or memory cannot be reserved. Every `None` here is a refusal:
//! nothing is sent, and the caller takes its safe fallback (a paste is refused before the clipboard
//! is touched and the host offers Copy; a streamed write that cannot be prepared is never
//! authorized and streaming stops with nothing written by it). Text longer than one batch is never sent as one: the caller splits it at
//! [`MAX_UNITS_PER_BATCH`] and gives every batch its own authorization.
//!
//! The test `raw_write_primitives_appear_only_in_gated_writes` in `evidence_gate` fails when any
//! other source file names one of the raw primitives.

use crate::evidence_gate::{WriteKind, WritePermit};

#[cfg(target_os = "windows")]
use rightkit_automation::desktop::windows as platform;

#[cfg(target_os = "macos")]
use rightkit_automation::desktop::macos as platform;

/// The most UTF-16 units of typed text, and the most Backspace keys, that one prepared batch may
/// hold: the crate's cap on native events per batch divided by the two events (down and up) each
/// needs. A caller splits longer work and authorizes every batch separately. The compile-time
/// check below keeps this within the crate's cap, so lowering the cap there fails the build here.
pub const MAX_UNITS_PER_BATCH: usize = 512;

#[cfg(any(target_os = "windows", target_os = "macos"))]
const _: () =
    assert!(MAX_UNITS_PER_BATCH * 2 <= rightkit_automation::desktop::MAX_PREPARED_INPUT_EVENTS);

/// Byte length of the longest prefix of `text`, cut on a character boundary, that holds at most
/// [`MAX_UNITS_PER_BATCH`] UTF-16 units. Surrogate pairs are never split. It is `text.len()` for
/// text that fits one batch, and `0` only for empty text.
pub fn typed_prefix_len(text: &str) -> usize {
    let mut units = 0usize;
    for (index, ch) in text.char_indices() {
        units += ch.len_utf16();
        if units > MAX_UNITS_PER_BATCH {
            return index;
        }
    }
    text.len()
}

/// An input batch built before authorization. Opaque; sent only through the functions below.
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub struct Prepared(platform::PreparedInput);

/// Unsupported platform: nothing can be prepared, so nothing can be sent.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub struct Prepared(());

// ---- BUILD (before authorization) ----

/// Streaming: the batch that types `text` into the focused control. `None` when `text` does not
/// fit one batch (more than [`MAX_UNITS_PER_BATCH`] UTF-16 units) or the platform cannot build it.
pub fn prepare_type_text(text: &str) -> Option<Prepared> {
    #[cfg(target_os = "windows")]
    {
        platform::prepare_unicode_text(text).map(Prepared)
    }
    #[cfg(target_os = "macos")]
    {
        platform::prepare_unicode_chunked(text).map(Prepared)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = text;
        None
    }
}

/// Streaming: the batch that deletes `count` characters before the caret. `None` when `count`
/// exceeds [`MAX_UNITS_PER_BATCH`] or the platform cannot build it.
pub fn prepare_backspaces(count: usize) -> Option<Prepared> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        platform::prepare_backspaces(count).map(Prepared)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = count;
        None
    }
}

/// The batch that presses and releases Return.
pub fn prepare_enter() -> Option<Prepared> {
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        platform::prepare_return().map(Prepared)
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// Windows: the Ctrl+V batch (modifiers are assumed up; see [`settle_modifiers`]).
#[cfg(target_os = "windows")]
pub fn prepare_paste() -> Option<Prepared> {
    platform::prepare_paste_chord().map(Prepared)
}

/// macOS: the Cmd+V batch tagged with the paste-probe token.
#[cfg(target_os = "macos")]
pub fn prepare_paste(token: i64) -> Option<Prepared> {
    platform::prepare_paste_tagged(token).map(Prepared)
}

// ---- SEND (after authorization) ----

/// What a send did. The difference between the last two is what streaming needs to know whether
/// it can still count what is in the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    /// The crate's `send_now` reported that every event was issued.
    Sent,
    /// The permit was refused at redemption (expired or for another kind). Nothing was sent: the
    /// field is exactly as it was.
    NotSent,
    /// `send_now` reported failure. The batch was handed to the OS and may have been inserted in
    /// part (a failed or partial `SendInput` consumes the batch), so what was written is unknown.
    Failed,
}

/// Validate, run the hook, redeem, send. Redemption checks expiry after the hook, which is for the one
/// in-process step that has to sit at the send (the paste receipt boundary, see [`paste`]).
fn issue_outcome(
    permit: WritePermit,
    allowed: &[WriteKind],
    prepared: &Prepared,
    before_send: impl FnOnce(),
) -> SendOutcome {
    if permit.validate(allowed).is_err() {
        return SendOutcome::NotSent;
    }
    before_send();
    // Receipt marking can wait for an in-process lock. Its delay must never
    // extend authorization: consume the permit only immediately before send.
    if permit.redeem(allowed).is_err() {
        return SendOutcome::NotSent;
    }
    if send(prepared) {
        SendOutcome::Sent
    } else {
        SendOutcome::Failed
    }
}

/// [`issue_outcome`] for the callers that only need "was it sent".
fn issue(
    permit: WritePermit,
    allowed: &[WriteKind],
    prepared: &Prepared,
    before_send: impl FnOnce(),
) -> bool {
    issue_outcome(permit, allowed, prepared, before_send) == SendOutcome::Sent
}

#[cfg(target_os = "windows")]
fn send(prepared: &Prepared) -> bool {
    prepared.0.send_now()
}

#[cfg(target_os = "macos")]
fn send(prepared: &Prepared) -> bool {
    prepared.0.send_now()
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn send(_prepared: &Prepared) -> bool {
    false
}

/// Streaming: type the batch built by [`prepare_type_text`]. Redeems the permit and sends; builds
/// nothing, reads nothing.
pub fn type_prepared(permit: WritePermit, prepared: &Prepared) -> SendOutcome {
    issue_outcome(permit, &[WriteKind::StreamChunk], prepared, || {})
}

/// Streaming: send the Backspace batch built by [`prepare_backspaces`] (reconcile or
/// cancel-erase). Redeems the permit and sends; builds nothing, reads nothing.
pub fn backspaces_prepared(permit: WritePermit, prepared: &Prepared) -> SendOutcome {
    issue_outcome(
        permit,
        &[WriteKind::StreamBackspace, WriteKind::CancelErase],
        prepared,
        || {},
    )
}

/// Press Return with a batch built by [`prepare_enter`] before authorization (the paste route's
/// Return and streaming's Return).
pub fn enter_prepared(permit: WritePermit, prepared: &Prepared) -> bool {
    issue(permit, &[WriteKind::Enter], prepared, || {})
}

/// Send the paste chord built by `prepare_paste`. `before_send` runs after the permit is
/// initially validated; expiry is checked again after the hook, immediately before the batch
/// is sent. The caller marks the promised clipboard
/// as posted there (an in-process lock and notify, no I/O). It has to sit at the send because a
/// receipt that arrives before the mark is ignored as an eager third-party read, so marking it
/// earlier would count unrelated reads and marking it later could drop the target's real one.
pub fn paste(permit: WritePermit, prepared: &Prepared, before_send: impl FnOnce()) -> bool {
    issue(permit, &[WriteKind::PasteKeystroke], prepared, before_send)
}

// ---- Modifier keys ----

/// Result of waiting for the stop hotkey's modifiers to lift.
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModifierSettle {
    /// Time spent waiting.
    pub waited_ms: u64,
    /// A physical Ctrl, Shift, Alt or Win key was still down at the cap.
    pub still_held: bool,
}

/// Windows: wait until the stop hotkey's physical modifiers are up, at most `cap_ms`. Runs BEFORE
/// the evidence is sampled. It reads input state only and writes nothing; if a key is still held
/// at the cap the caller refuses the write (the chord no longer releases and re-presses the
/// user's keys).
#[cfg(target_os = "windows")]
pub fn settle_modifiers(cap_ms: u64) -> ModifierSettle {
    let started = std::time::Instant::now();
    let mut still_held = platform::physical_modifiers_held();
    while still_held {
        if started.elapsed().as_millis() as u64 >= cap_ms {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        still_held = platform::physical_modifiers_held();
    }
    ModifierSettle {
        waited_ms: started.elapsed().as_millis() as u64,
        still_held,
    }
}

/// The final modifier sample, handed to `evidence_gate::authorize_with_input`, as a tri-state:
/// `Some(true)` when the user's physical modifier keys are all up, `Some(false)` when one is down,
/// `None` when the platform could not observe them. It is read once, with nothing before or after
/// it.
///
/// * Windows: `rightkit_automation::desktop::windows::physical_modifiers_held` (Ctrl, Shift, Alt
///   and Win).
/// * macOS: `rightkit_automation::desktop::macos::physical_modifier_state()` (both sides of Shift,
///   Control, Option and Command, plus Fn), mapped with `.ok().map(|state| !state.any_held())`.
///   Missing Input Monitoring permission, Secure Input, an unavailable console session or HID
///   source, and an inconsistent sample are `Err` there and become `None`. The sample is an
///   instantaneous read of the Quartz HID state table; it proves neither a quiet period nor that
///   a complete press and release between its reads did not happen.
/// * Any other platform: `None`.
///
/// `None` is refused by the gate as `modifiers_unknown`: unknown is never "up".
pub fn modifiers_quiet() -> Option<bool> {
    #[cfg(target_os = "windows")]
    {
        Some(!platform::physical_modifiers_held())
    }
    #[cfg(target_os = "macos")]
    {
        platform::physical_modifier_state()
            .ok()
            .map(|state| !state.any_held())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        None
    }
}
