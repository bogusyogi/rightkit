//! The dictation pill controller: a recording capsule, a hint card and a clip card, driven as
//! three native layered overlays on Windows, plus the platform-neutral session logic around a
//! pill of any kind (the full-screen and foreground guardian, the generation-stamped event queue,
//! bounds and validation helpers for a native pill view's C ABI).
//!
//! What the crate owns, and what the host supplies:
//!
//! * the controller: the capsule state model (`pill`), the hint and clip card state (`hint`,
//!   `clipboard`), the mapping from `PillEvent` to state, timers, topmost handling, placement,
//!   hover tracking, hit-test dispatch and the owner thread (`driver`), with an explicit
//!   generation lifecycle (`lifecycle`) and host work that is never shown as done before the host
//!   reports it (`pending`)
//! * generic primitives a painter needs: display-list types and stroke helpers (`draw`), font
//!   slots and text measurement (`text`), easing, springs and fades (`anim`), the interaction
//!   region contract (`regions`), the overlay seam (`gfx`, Windows)
//! * the session logic that does not draw: the event-driven guardian (`guardian`,
//!   `guardian_worker`, the OS event sources `guardian_win`/`guardian_mac` and the macOS
//!   fullscreen verdict `fullscreen_mac`; the Windows verdict is
//!   `rightkit_overlay::windows::foreground_fullscreen_on`), the generation-stamped event queue
//!   (`dispatch_queue`), the capsule hitbox and frame store (`frame`) and the generic bounds and
//!   validation helpers for a native pill view's C ABI (`bridge`: text cut and log filters, name
//!   and id alphabets, integer-code tables; no wire encoding and no splash logic)
//!
//! What the host supplies, always: the look ([`look::Painters`], required, no default), the
//! interaction regions each painter returns, [`config::HostInputs`] (status words, timings,
//! limits, placement and stacking policy, payload policy, what a `Starting` event shows, window
//! titles, thread and diagnostic prefixes, the intro sound, the capture policy and the event sink),
//! [`look::Motion`] (every spring, fade, easing curve and duration), [`guardian::GuardianTimings`]
//! and [`guardian::EdgeTolerance`] and [`config::Callbacks`]: none of them has a `Default`, so
//! nothing a product would choose is chosen here. Whether a pill shows at all (a splash in front,
//! onboarding, a full-screen application, the "always show the resting pill" setting), which tips
//! exist and when they are offered, what a dictation target counts as, how a splash hand-off is
//! timed and which numbers a native view's wire event uses are the host's policy and live in the
//! host. The crate has no palette, no layout size, no label, no easing curve, no timing, no
//! tolerance, no persisted key, no file name and no tip logic of its own.
//!
//! Usage: build a [`config::Config`] from your `Painters`, `HostInputs` and `Callbacks`, call
//! [`config::install`] once (it validates every host number, string and list), then
//! `ensure_installed` on Windows (the only call that starts the driver, and the only one besides
//! `shutdown` that blocks; it answers an `Install`, and `RetirementPending` means a thread of an
//! earlier generation (owner, window, forwarder or worker) is still running: nothing was created,
//! retry later),
//! then feed events with `apply_event` and `voice_level`.
//! `shutdown` destroys the overlays. Until `install` has run, every driver call does nothing;
//! after `install`, a call made while no generation is running is dropped and reported once.
//!
//! `PillEvent` is `rightkit_dictation_core::pill::PillEvent` (wire form pinned there).
//!
//! macOS: the controller and painters are Windows-only; a macOS pill is a native view reached
//! through the host's own C ABI, using the `bridge` helpers for bounds. The guardian, `frame`,
//! `dispatch_queue` and `bridge` are the same on both.
//!
//! Diagnostics carry fixed codes and `key=value` pairs made of fixed codes, booleans, counts, the
//! closed surface names and numbers. They never carry a panic payload, a host string (window
//! name, hint kind, region name), a host error string, a transcript, a label or a path: a panic
//! is reported by the callback's name and the payload's type.
//!
//! Idle cost: one parked thread per overlay event forwarder plus the driver blocked on its command
//! queue. No timer runs until something animates.

#![allow(dead_code)]

pub mod anim;
pub mod bridge;
pub mod clipboard;
pub(crate) mod clock;
pub(crate) mod cmd_queue;
pub mod config;
pub mod dispatch_queue;
pub mod draw;
pub mod frame;
pub mod guardian;
pub mod guardian_worker;
pub mod hint;
pub(crate) mod lifecycle;
pub mod look;
pub(crate) mod pending;
pub mod pill;
pub mod regions;
pub(crate) mod retire;
pub mod text;
pub mod visibility;

#[cfg(target_os = "windows")]
mod driver;
#[cfg(target_os = "windows")]
mod gfx;
#[cfg(target_os = "windows")]
mod guardian_win;

#[cfg(target_os = "macos")]
pub mod fullscreen_mac;
#[cfg(target_os = "macos")]
mod guardian_mac;

pub use clipboard::ClipRefusal;
pub use config::{
    install, ActionOutcome, Callbacks, CardOrder, Config, HostInputs, InstallError, IntroSound,
    Limits, PayloadPolicy, PayloadSide, PlacementPolicy, StatusLabels, Timings,
};
pub use guardian::{EdgeTolerance, GuardianTimings};
pub use guardian_worker::{GuardianHandle, GuardianNames, GuardianStartError};
pub use look::{
    AuxFrame, ClipView, HintView, Metrics, Motion, Painters, Pass, PillFrame, PillScene,
    PrevContent,
};
pub use regions::{
    cursor_at, Action, ActionTag, Activation, Cursor, HotSet, HoverRole, Region, RegionId, Shape,
    Surface,
};
pub use rightkit_dictation_core::pill::PillEvent;
pub use visibility::Visibility;

#[cfg(target_os = "windows")]
pub use driver::{
    apply_capture_affinity, apply_event, capsule_rect, clipboard_visible, dismiss_clipboard,
    dismiss_hint, ensure_installed, hint_visible, is_running, is_visible, monitor_rect,
    overlay_frames, overlay_hwnds, reassert_on, reassert_topmost, reposition, reveal,
    set_visibility, show_clipboard, show_hint, shutdown, voice_level, Install,
};
#[cfg(target_os = "windows")]
pub(crate) use driver::{Cmd, CmdTx, Shared, Surf};
