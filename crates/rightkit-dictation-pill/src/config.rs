//! Everything the host supplies that is not a look: window names, status words, timings,
//! limits, placement and stacking policy, payload policy, the intro sound, the capture policy,
//! an event sink and the callbacks. The look itself (painters, fonts, sizes, motion) is
//! [`Painters`], the first input of [`Config::new`].
//!
//! The host builds one [`Config`] and calls [`install`] once, before the first call into the
//! crate. Until it is installed every driver call does nothing: there is no default look to fall
//! back to. Nothing in this crate names a product or carries a value that a product chose:
//! [`HostInputs`] and [`Callbacks`] have no `Default`, so a host cannot ship by accident with a
//! number, a word, a policy or a callback that this crate picked for it.
//!
//! # Validation
//!
//! [`install`] checks every host number, string and list before anything is stored: finite
//! numbers inside the documented range, durations that fit the clock, strings with a byte cap and
//! no control characters, byte blobs with a cap. The first violation is reported as
//! [`InstallError::Invalid`] carrying the name of the field, and nothing is installed. The three
//! `Painters` reads (`fonts`, `metrics`, `motion`) run once, guarded against a panic, before the
//! configuration becomes visible, so a half-installed configuration cannot exist.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::anim::Mode;
use crate::look::{Metrics, Motion, Painters};
use crate::regions::Activation;
use crate::text::Fonts;

/// Structural bounds the crate applies to what a painter returns and to what the host sends. They
/// are not product choices: they cap work and memory, and a host cannot raise them.
pub mod bound {
    /// Largest window or surface edge, in points.
    pub const MAX_SURFACE_PX: f32 = 4096.0;
    /// Interaction regions taken from one frame; later ones are dropped.
    pub const MAX_REGIONS: usize = 64;
    /// Display-list operations replayed from one frame; later ones are dropped.
    pub const MAX_DRAW_OPS: usize = 4096;
    /// Points of one polygon; a longer polygon is not drawn.
    pub const MAX_POLYGON_POINTS: usize = 256;
    /// Bytes of one display-list text run; a longer run is cut.
    pub const MAX_TEXT_RUN_BYTES: usize = 4096;
    /// Bytes of an item id or a hint kind.
    pub const MAX_ID_BYTES: usize = 256;
    /// Longest duration a host timing may ask for (one hour), milliseconds.
    pub const MAX_DURATION_MS: u64 = 3_600_000;
    /// Bytes of one font blob.
    pub const MAX_FONT_BYTES: usize = 16 * 1024 * 1024;
    /// Bytes of an in-memory intro sound.
    pub const MAX_SOUND_BYTES: usize = 8 * 1024 * 1024;
}

// ---------------------------------------------------------------------------------------------
// Status words, timings, placement, limits
// ---------------------------------------------------------------------------------------------

/// The words the controller puts into the capsule for events that carry none. The look draws the
/// label it is handed; every other string (button names, hint and clip copy) is the painter's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusLabels {
    /// `Success` shows `done`; a `Status` event with an empty label shows `saved`; an `Error`
    /// event with an empty label shows `error`; a `Status` whose trimmed label equals one of
    /// `cancelled_aliases` (ASCII case-insensitive) shows `cancelled`. Each is non-empty and at
    /// most `Limits::max_label_bytes` bytes.
    pub done: &'static str,
    pub saved: &'static str,
    pub cancelled: &'static str,
    pub error: &'static str,
    /// The incoming labels that mean "cancelled" (at most 8, each at most 64 bytes). The host
    /// decides which spellings its state machine emits; the crate knows none.
    pub cancelled_aliases: &'static [&'static str],
}

/// How long transient surfaces stay up and how long host work may take, in milliseconds. Every
/// field is validated by [`install`]: `1..=bound::MAX_DURATION_MS` unless stated otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timings {
    /// A status capsule (done, saved, cancelled) holds this long before collapsing to idle.
    pub status_hold_ms: u64,
    /// An error capsule holds this long, then the recovery callback runs.
    pub error_hold_ms: u64,
    /// A single (just-captured) clip card closes itself after this long unless touched.
    pub single_clip_autodismiss_ms: u64,
    /// A clip copy (`Callbacks::copy_text`) that has not answered after this long counts as
    /// failed: the card shows the failure feedback and stays open. A late answer is ignored.
    pub copy_deadline_ms: u64,
    /// A host action whose outcome the surface reads (`Action::HostFlash`, or `Action::Host` that
    /// closes a card) that has not answered after this long counts as failed. A late answer is
    /// ignored.
    pub host_action_deadline_ms: u64,
    /// How long the clip card shows its copy-failure feedback before clearing it.
    pub copy_failed_hold_ms: u64,
    /// How long `ensure_installed` waits for the owner thread to create the overlays. When it
    /// gives up, the owner destroys whatever it made and exits. `50..=600_000`.
    pub startup_timeout_ms: u64,
    /// How long [`crate::shutdown`] waits for an owner thread to close the overlays and exit and
    /// for every retained owner and window thread to finish and every forwarder and worker to
    /// retire, and how long a restarting `ensure_installed` waits for the same two things in total
    /// (one bound for both; when it runs out the answer is `Install::RetirementPending`).
    /// `5_000..=600_000`: the floor is kept from the earlier teardown (three windows joined for up
    /// to 1.5 s each), so a healthy teardown never reports `false` because the bound was too
    /// short. A shorter wait is refused at install, not honoured.
    pub shutdown_timeout_ms: u64,
    /// While a clip card is open the crate watches for a press outside it. The first look comes
    /// this long after the card opens, so the click that opened it is never taken for an outside
    /// click. `0..=bound::MAX_DURATION_MS`.
    pub click_watch_first_ms: u64,
    /// Poll period of the primary-button watch (clip card open) and of the right-button watch
    /// (pointer on a region with a secondary action). `5..=1000`.
    pub pointer_poll_ms: u64,
    /// Re-layout period while a hovered recording dock's timer digits change width. `10..=5000`.
    pub region_refresh_ms: u64,
}

/// Which card sits nearest the capsule when the hint and the clip card are both up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardOrder {
    /// The clip card is stacked against the capsule, the hint above it.
    ClipNearest,
    /// The hint is stacked against the capsule, the clip card above it.
    HintNearest,
}

/// Where the overlays sit when the monitor cannot be resolved, and how the cards stack. The pill
/// otherwise follows the monitor under the cursor, bottom-centred above its work area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacementPolicy {
    /// Physical px; finite, magnitude at most 1e6.
    pub fallback_center_x: f64,
    pub fallback_capsule_bottom: f64,
    pub card_order: CardOrder,
    /// Opening the clip card closes a visible hint at once (no exit animation).
    pub clip_replaces_hint: bool,
}

/// Bounds on what the host sends, validated by [`install`]. Each has the range stated; a send
/// over its bound is cut (labels, hint text) or refused with a diagnostic (clip content).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes of a capsule label (`Status` / `Error` event text). `16..=4096`. Longer labels are
    /// cut at a character boundary.
    pub max_label_bytes: usize,
    /// Bytes of each of a hint's `kind`, `title` and `body`. `16..=65_536`. Longer text is cut.
    pub max_hint_text_bytes: usize,
    /// Items of one clip card. `1..=64`. More items are cut.
    pub max_clip_items: usize,
    /// Bytes of one clip payload (`primary` or `alternate`). `64..=1_048_576`. A card with an
    /// item over the bound is **not shown** (and `show_clipboard` reports it): cutting would let
    /// a copy put a truncated transcript on the clipboard.
    pub max_clip_payload_bytes: usize,
    /// Bytes of all payloads of one card together (every kept item's `primary` plus `alternate`).
    /// `max_clip_payload_bytes..=33_554_432`. A card over the bound is refused, not cut
    /// (`ClipRefusal::TotalTooLarge`).
    pub max_clip_total_bytes: usize,
    /// Short-lived worker threads running host callbacks at once, counted process-wide: a worker
    /// that outlives its generation still holds its slot, and the next generation sees it.
    /// `1..=64`. A click that would start one more is refused with a diagnostic.
    pub max_worker_threads: usize,
}

// ---------------------------------------------------------------------------------------------
// Payload policy
// ---------------------------------------------------------------------------------------------

/// Which of a clip item's two payloads a single card shows first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadSide {
    Primary,
    Alternate,
}

/// The crate knows a clip item as a "primary" payload and an optional "alternate" payload and
/// attaches no meaning to the pair (a host may mean polished and raw text, or anything else).
/// This is the host's rule for them.
#[derive(Clone, Copy, Debug)]
pub struct PayloadPolicy {
    /// The payload a single card shows when it opens (and copies, until the user toggles).
    pub initial: PayloadSide,
    /// Whether `alternate` counts as a different payload from `primary`. When it does not, the
    /// card offers no toggle and `ToggleAlternate` does nothing. Called with payloads of at most
    /// `Limits::max_clip_payload_bytes` bytes; a panic counts as "not different".
    pub distinct: fn(primary: &str, alternate: &str) -> bool,
}

// ---------------------------------------------------------------------------------------------
// Sound
// ---------------------------------------------------------------------------------------------

/// The sound played with the launch bloom (`reveal(intro = true)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntroSound {
    /// A complete WAV file image played from memory, at most `bound::MAX_SOUND_BYTES`. The bytes
    /// stay alive for the process, so an asynchronous play is safe.
    Wav(Arc<[u8]>),
    /// A WAV file played through the default output (path at most 1024 bytes).
    File(PathBuf),
    /// No sound: the host's choice, stated.
    Off,
}

// ---------------------------------------------------------------------------------------------
// Host inputs
// ---------------------------------------------------------------------------------------------

/// Everything the host must decide before the pill can exist. No `Default`.
#[derive(Clone, Debug)]
pub struct HostInputs {
    pub status: StatusLabels,
    pub timings: Timings,
    pub placement: PlacementPolicy,
    pub limits: Limits,
    /// OS window titles (`OverlayConfig::name`) of the pill, hint and clip overlays: distinct,
    /// non-empty, at most 96 bytes (the overlay crate refuses a longer name), no control
    /// characters. They are visible to other software
    /// (no-activate sweeps, QA journeys), so a host keeps them stable across releases.
    pub names: [&'static str; 3],
    /// Prefix of the crate's thread names (the owner thread and `<prefix>-events-N` forwarders
    /// and `<prefix>-<job>` workers), visible in profilers and crash reports. Non-empty, at most
    /// 48 bytes, no control characters.
    pub thread_name: &'static str,
    /// Prefix of every diagnostic event name the crate reports: `<prefix>_<code>`. Lower-case
    /// ASCII letters, digits and `_`, at most 32 bytes.
    pub event_prefix: &'static str,
    pub payloads: PayloadPolicy,
    pub intro_sound: IntroSound,
    /// Keep the overlays out of screen capture (Teams, Zoom, screenshots). A host turns this off
    /// for builds that must capture the pill.
    pub exclude_from_capture: bool,
    /// What a `PillEvent::Starting` shows: `Idle`, `Recording` or `Processing`. The crate has no
    /// default; `Status` and `Error` need a label and are refused at install.
    pub starting_mode: Mode,
    /// Where diagnostics go: `(event, detail)`. Event names are `<event_prefix>_<code>` and the
    /// detail is `key=value` text made only of fixed codes, booleans, counts, the closed surface
    /// names (`Pill`, `Hint`, `Clip`) and numbers (generations, deadlines, region ids). It never
    /// carries a host string (a window name, a hint kind, a region name), a transcript, a label,
    /// a path or error text. A panic in the sink is caught.
    pub event_sink: fn(&str, &str),
}

// ---------------------------------------------------------------------------------------------
// Callbacks
// ---------------------------------------------------------------------------------------------

/// What an action callback reports back. `Action::HostFlash` reads it to drive the control's
/// feedback (`Done` flashes a check mark, `Empty` and `Failed` dim it and it ignores clicks while
/// dimmed); an `Action::Host` that closes a card closes it only on `Done`. The host logs its own
/// failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionOutcome {
    Done,
    /// There was nothing to act on (the control shows its disabled state).
    Empty,
    Failed,
}

/// What the surfaces ask the host to do. No `Default`: every callback is required and does what
/// its name says.
///
/// Threading: `activate` and `copy_text` run on a short-lived worker thread (never on the driver
/// thread) and may block; the surface reads their result, or treats no answer within the
/// matching `Timings` deadline as a failure and ignores the late answer. A callback that never
/// returns costs its worker thread until process exit, and at most `Limits::max_worker_threads`
/// workers run at once. `activate_secondary`, `error_generation`, `clip_dismissed` and the two
/// `publish_*` callbacks are cheap and non-blocking and run on the driver thread (they must not
/// call [`crate::shutdown`]: it refuses on that thread); `reveal_pill` and `recover_error` run on
/// a short-lived worker. A panic in any callback is caught and reported
/// (`<prefix>_host_panic`): it never takes a window or the owner thread with it, and counts as a
/// failure where the surface reads an outcome.
#[derive(Clone, Copy)]
pub struct Callbacks {
    /// A region with `Action::Host` or `Action::HostFlash` was clicked. The host maps
    /// `Activation::tag` to its own command and handles errors (logging, telemetry).
    pub activate: fn(Activation) -> ActionOutcome,
    /// A right-button press on a region that has a `secondary` tag. Must return quickly (spawn
    /// any async work).
    pub activate_secondary: fn(Activation),
    /// Copy this text to the clipboard (a clip row or the single clip's copy button). `true`
    /// means the clipboard holds the text now: the card shows "copied" and (single card) closes
    /// only after it returns `true`. The reason for a failure is the host's to log.
    pub copy_text: fn(String) -> bool,
    /// The clip card was dismissed: release the host's holds (override transcript, "pill stays
    /// expanded").
    pub clip_dismissed: fn(),
    /// A status capsule expired: collapse to idle through the host's reveal path so its
    /// visibility policy still applies.
    pub reveal_pill: fn(),
    /// An error capsule expired: terminal-only recovery bound to the generation armed with it.
    /// `false` means the host rejected the recovery (reported, nothing else changes).
    pub recover_error: fn(generation: Option<u64>) -> bool,
    /// The generation to bind an error capsule's expiry to (read when an `Error` event arrives).
    pub error_generation: fn() -> Option<u64>,
    /// The capsule's rectangle inside the pill window, logical px (`x, y, width, height`), each
    /// time it changes. Hosts anchor their window transitions to it.
    pub publish_capsule_hitbox: fn(x: f64, y: f64, width: f64, height: f64),
    /// The pill window's physical top-left and the monitor scale, on every layout.
    pub publish_window_frame: fn(x: f64, y: f64, scale: f64),
}

// ---------------------------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
pub struct Config {
    /// The look. Required: there is no default.
    pub painters: Arc<dyn Painters>,
    pub status: StatusLabels,
    pub timings: Timings,
    pub placement: PlacementPolicy,
    pub limits: Limits,
    pub names: [&'static str; 3],
    pub thread_name: &'static str,
    pub event_prefix: &'static str,
    pub payloads: PayloadPolicy,
    pub intro_sound: IntroSound,
    pub exclude_from_capture: bool,
    pub starting_mode: Mode,
    pub event_sink: fn(&str, &str),
    pub callbacks: Callbacks,
}

impl Config {
    /// A config from the look, the host's decisions and the host's callbacks. Nothing is
    /// defaulted; [`install`] validates it.
    pub fn new(painters: Arc<dyn Painters>, host: HostInputs, callbacks: Callbacks) -> Config {
        Config {
            painters,
            status: host.status,
            timings: host.timings,
            placement: host.placement,
            limits: host.limits,
            names: host.names,
            thread_name: host.thread_name,
            event_prefix: host.event_prefix,
            payloads: host.payloads,
            intro_sound: host.intro_sound,
            exclude_from_capture: host.exclude_from_capture,
            starting_mode: host.starting_mode,
            event_sink: host.event_sink,
            callbacks,
        }
    }
}

/// Why [`install`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallError {
    /// A configuration is already in place (the first one stays).
    AlreadyInstalled,
    /// A host input is outside its documented range. Carries the field's name.
    Invalid(&'static str),
    /// A `Painters` read (`fonts`, `metrics`, `motion`) or a host curve probed at install
    /// panicked.
    PainterPanicked,
}

impl InstallError {
    /// A fixed code for diagnostics (never a `{:?}` of the value: `Invalid` carries a field name).
    pub fn code(self) -> &'static str {
        match self {
            InstallError::AlreadyInstalled => "already_installed",
            InstallError::Invalid(_) => "invalid_input",
            InstallError::PainterPanicked => "painter_panicked",
        }
    }

    /// The name of the field that was refused, for the host's own error handling (it is a
    /// `&'static str` chosen by this crate, never host text).
    pub fn field(self) -> Option<&'static str> {
        match self {
            InstallError::Invalid(field) => Some(field),
            _ => None,
        }
    }
}

static CONFIG: OnceLock<Config> = OnceLock::new();
static METRICS: OnceLock<Metrics> = OnceLock::new();
static MOTION: OnceLock<Motion> = OnceLock::new();
/// Serialises [`install`] so two racing calls cannot mix their metrics and motion.
static INSTALL: Mutex<()> = Mutex::new(());

fn f32_in(value: f32, low: f32, high: f32) -> bool {
    value.is_finite() && value >= low && value <= high
}

fn f64_in(value: f64, low: f64, high: f64) -> bool {
    value.is_finite() && value >= low && value <= high
}

fn ms_in(value: u64, low: u64, high: u64) -> bool {
    value >= low && value <= high
}

/// Non-empty (unless `allow_empty`), within `max` bytes, no control characters.
fn text_ok(value: &str, max: usize, allow_empty: bool) -> bool {
    (allow_empty || !value.is_empty()) && value.len() <= max && !value.chars().any(char::is_control)
}

fn event_prefix_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

macro_rules! check {
    ($ok:expr, $field:expr) => {
        if !($ok) {
            return Err(InstallError::Invalid($field));
        }
    };
}

fn check_spring(
    params: &crate::anim::SpringParams,
    name: &'static str,
) -> Result<(), InstallError> {
    check!(f32_in(params.response, 0.02, 10.0), name);
    check!(f32_in(params.damping, 0.0, 10.0), name);
    Ok(())
}

/// A host curve must map progress 0 to 0 and 1 to 1 (within 0.01) and answer finite numbers at a
/// quarter, a half and three quarters; it is called here once, under the install guard.
fn check_curve(curve: crate::anim::Curve, name: &'static str) -> Result<(), InstallError> {
    for probe in [0.25f32, 0.5, 0.75] {
        check!(curve(probe).is_finite(), name);
    }
    check!(f32_in(curve(0.0), -0.01, 0.01), name);
    check!(f32_in(curve(1.0), 0.99, 1.01), name);
    Ok(())
}

fn check_metrics(m: &Metrics) -> Result<(), InstallError> {
    let edge = bound::MAX_SURFACE_PX;
    check!(f32_in(m.pill_window_w, 1.0, edge), "metrics.pill_window_w");
    check!(f32_in(m.pill_window_h, 1.0, edge), "metrics.pill_window_h");
    check!(
        f32_in(m.pill_window_bottom_pad, 0.0, edge),
        "metrics.pill_window_bottom_pad"
    );
    check!(f32_in(m.bottom_margin, 0.0, edge), "metrics.bottom_margin");
    check!(f32_in(m.aux_spacing, 0.0, edge), "metrics.aux_spacing");
    check!(f32_in(m.extent_slack, 0.0, 256.0), "metrics.extent_slack");
    Ok(())
}

fn check_motion(m: &Motion) -> Result<(), InstallError> {
    let day = bound::MAX_DURATION_MS;
    check_spring(&m.capsule_spring, "motion.capsule_spring")?;
    check_spring(&m.reduced_spring, "motion.reduced_spring")?;
    check_spring(&m.card_spring, "motion.card_spring")?;
    check!(
        f32_in(m.capsule_spring_eps, 0.0, 1000.0),
        "motion.capsule_spring_eps"
    );
    check!(
        f32_in(m.card_spring_eps, 0.0, 1000.0),
        "motion.card_spring_eps"
    );
    check!(
        f32_in(m.card_scale_base, 0.0, 4.0),
        "motion.card_scale_base"
    );
    check!(
        f32_in(m.card_scale_gain, -4.0, 4.0),
        "motion.card_scale_gain"
    );
    check!(
        f32_in(m.card_rows_window_ms, 0.0, 60_000.0),
        "motion.card_rows_window_ms"
    );
    check!(f32_in(m.crossfade_ms, 0.0, 60_000.0), "motion.crossfade_ms");
    check!(
        f32_in(m.crossfade_reduced_ms, 0.0, 60_000.0),
        "motion.crossfade_reduced_ms"
    );
    check_curve(m.fade_curve, "motion.fade_curve")?;
    check_curve(m.crossfade_curve, "motion.crossfade_curve")?;
    check!(
        f32_in(m.controls_scale_base, 0.0, 4.0),
        "motion.controls_scale_base"
    );
    check!(
        f32_in(m.controls_scale_gain, -4.0, 4.0),
        "motion.controls_scale_gain"
    );
    check!(
        f32_in(m.processing_delay_ms, 0.0, 60_000.0),
        "motion.processing_delay_ms"
    );
    check!(
        ms_in(m.hover_exit_delay_ms, 0, day),
        "motion.hover_exit_delay_ms"
    );
    check!(f32_in(m.hide_fade_ms, 0.0, 60_000.0), "motion.hide_fade_ms");
    check!(
        ms_in(m.hide_order_out_ms, 0, day),
        "motion.hide_order_out_ms"
    );
    check!(
        f32_in(m.button_hot_ms, 0.0, 60_000.0),
        "motion.button_hot_ms"
    );
    check!(f32_in(m.aux_hot_ms, 0.0, 60_000.0), "motion.aux_hot_ms");
    check!(
        f32_in(m.reveal_offset, -4096.0, 4096.0),
        "motion.reveal_offset"
    );
    check!(f32_in(m.reveal_ms, 0.0, 60_000.0), "motion.reveal_ms");
    check!(
        f32_in(m.intro_seconds, 0.001, 600.0),
        "motion.intro_seconds"
    );
    check!(
        f64_in(m.copy_check_ms, 0.0, 600_000.0),
        "motion.copy_check_ms"
    );
    check!(
        f64_in(m.copy_empty_ms, 0.0, 600_000.0),
        "motion.copy_empty_ms"
    );
    check!(
        f64_in(m.copy_check_fade_in_ms, 0.0, 600_000.0),
        "motion.copy_check_fade_in_ms"
    );
    check!(
        f64_in(m.copy_empty_fade_in_ms, 0.0, 600_000.0),
        "motion.copy_empty_fade_in_ms"
    );
    check!(
        f64_in(m.copy_fade_out_ms, 0.0, 600_000.0),
        "motion.copy_fade_out_ms"
    );
    check!(
        ms_in(m.close_after_copy_ms, 0, day),
        "motion.close_after_copy_ms"
    );
    check!(ms_in(m.copied_reset_ms, 0, day), "motion.copied_reset_ms");
    Ok(())
}

fn check_fonts(fonts: &Fonts) -> Result<(), InstallError> {
    let mut total = 0usize;
    for face in &fonts.faces {
        check!(face.bytes.len() <= bound::MAX_FONT_BYTES, "fonts.bytes");
        total = total.saturating_add(face.bytes.len());
        check!(text_ok(face.gdi_family, 128, false), "fonts.gdi_family");
        check!(
            text_ok(face.typographic_family, 128, false),
            "fonts.typographic_family"
        );
        check!(face.gdi_weight <= 1000, "fonts.gdi_weight");
        check!(face.typographic_weight <= 1000, "fonts.typographic_weight");
        check!(f32_in(face.ascent_em, 0.0, 4.0), "fonts.ascent_em");
        check!(f32_in(face.descent_em, 0.0, 4.0), "fonts.descent_em");
        check!(f32_in(face.gap_em, 0.0, 4.0), "fonts.gap_em");
    }
    check!(total <= 3 * bound::MAX_FONT_BYTES, "fonts.total_bytes");
    Ok(())
}

fn check_config(config: &Config) -> Result<(), InstallError> {
    let day = bound::MAX_DURATION_MS;
    let l = &config.limits;
    check!(
        l.max_label_bytes >= 16 && l.max_label_bytes <= 4096,
        "limits.max_label_bytes"
    );
    check!(
        l.max_hint_text_bytes >= 16 && l.max_hint_text_bytes <= 65_536,
        "limits.max_hint_text_bytes"
    );
    check!(
        l.max_clip_items >= 1 && l.max_clip_items <= 64,
        "limits.max_clip_items"
    );
    check!(
        l.max_clip_payload_bytes >= 64 && l.max_clip_payload_bytes <= 1_048_576,
        "limits.max_clip_payload_bytes"
    );
    check!(
        l.max_clip_total_bytes >= l.max_clip_payload_bytes && l.max_clip_total_bytes <= 33_554_432,
        "limits.max_clip_total_bytes"
    );
    check!(
        l.max_worker_threads >= 1 && l.max_worker_threads <= 64,
        "limits.max_worker_threads"
    );

    let s = &config.status;
    for (word, field) in [
        (s.done, "status.done"),
        (s.saved, "status.saved"),
        (s.cancelled, "status.cancelled"),
        (s.error, "status.error"),
    ] {
        check!(text_ok(word, l.max_label_bytes, false), field);
    }
    check!(s.cancelled_aliases.len() <= 8, "status.cancelled_aliases");
    for alias in s.cancelled_aliases {
        check!(text_ok(alias, 64, false), "status.cancelled_aliases");
    }

    let t = &config.timings;
    for (value, field) in [
        (t.status_hold_ms, "timings.status_hold_ms"),
        (t.error_hold_ms, "timings.error_hold_ms"),
        (
            t.single_clip_autodismiss_ms,
            "timings.single_clip_autodismiss_ms",
        ),
        (t.copy_deadline_ms, "timings.copy_deadline_ms"),
        (t.host_action_deadline_ms, "timings.host_action_deadline_ms"),
        (t.copy_failed_hold_ms, "timings.copy_failed_hold_ms"),
    ] {
        check!(ms_in(value, 1, day), field);
    }
    check!(
        ms_in(t.startup_timeout_ms, 50, 600_000),
        "timings.startup_timeout_ms"
    );
    check!(
        ms_in(t.shutdown_timeout_ms, 5_000, 600_000),
        "timings.shutdown_timeout_ms"
    );
    check!(
        ms_in(t.click_watch_first_ms, 0, day),
        "timings.click_watch_first_ms"
    );
    check!(ms_in(t.pointer_poll_ms, 5, 1000), "timings.pointer_poll_ms");
    check!(
        ms_in(t.region_refresh_ms, 10, 5000),
        "timings.region_refresh_ms"
    );

    let p = &config.placement;
    check!(
        f64_in(p.fallback_center_x, -1e6, 1e6),
        "placement.fallback_center_x"
    );
    check!(
        f64_in(p.fallback_capsule_bottom, -1e6, 1e6),
        "placement.fallback_capsule_bottom"
    );

    for name in config.names {
        check!(text_ok(name, 96, false), "names");
    }
    check!(
        config.names[0] != config.names[1]
            && config.names[0] != config.names[2]
            && config.names[1] != config.names[2],
        "names"
    );
    check!(
        matches!(
            config.starting_mode,
            Mode::Idle | Mode::Recording | Mode::Processing
        ),
        "starting_mode"
    );
    check!(text_ok(config.thread_name, 48, false), "thread_name");
    check!(event_prefix_ok(config.event_prefix), "event_prefix");

    match &config.intro_sound {
        IntroSound::Wav(bytes) => check!(bytes.len() <= bound::MAX_SOUND_BYTES, "intro_sound"),
        IntroSound::File(path) => check!(path.as_os_str().len() <= 1024, "intro_sound"),
        IntroSound::Off => {}
    }
    Ok(())
}

/// Install the host configuration. Succeeds once; call it before any other function of this
/// crate. Validates every host input, reads the painters' fonts, metrics and motion once
/// (guarded against a panic) and only then makes the configuration visible: either everything is
/// installed or nothing is.
pub fn install(config: Config) -> Result<(), InstallError> {
    let _serial = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
    if CONFIG.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }
    check_config(&config)?;
    let painters = Arc::clone(&config.painters);
    let read = catch_unwind(AssertUnwindSafe(|| {
        (painters.fonts(), painters.metrics(), painters.motion())
    }));
    let Ok((fonts, metrics, motion)) = read else {
        return Err(InstallError::PainterPanicked);
    };
    check_fonts(&fonts)?;
    check_metrics(&metrics)?;
    // `check_motion` calls the host's curves, so it runs under the same panic guard as the reads.
    match catch_unwind(AssertUnwindSafe(|| check_motion(&motion))) {
        Ok(checked) => checked?,
        Err(_) => return Err(InstallError::PainterPanicked),
    }
    crate::text::install_fonts(fonts);
    let _ = METRICS.set(metrics);
    let _ = MOTION.set(motion);
    // Last: `try_get()` returning `Some` implies metrics, motion and fonts are in place.
    CONFIG
        .set(config)
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// The installed configuration, if any.
pub fn try_get() -> Option<&'static Config> {
    CONFIG.get()
}

/// The installed configuration. Only the driver and the models call this, and the driver does not
/// start before `install`.
pub fn get() -> &'static Config {
    CONFIG
        .get()
        .expect("rightkit-dictation-pill: config::install must run before the pill is used")
}

pub fn metrics() -> &'static Metrics {
    METRICS
        .get()
        .expect("rightkit-dictation-pill: config::install must run before the pill is used")
}

pub fn motion() -> &'static Motion {
    MOTION
        .get()
        .expect("rightkit-dictation-pill: config::install must run before the pill is used")
}

/// What a `Starting` event shows (`HostInputs::starting_mode`).
pub fn starting_mode() -> Mode {
    get().starting_mode
}

pub fn status() -> &'static StatusLabels {
    &get().status
}

pub fn timings() -> &'static Timings {
    &get().timings
}

pub fn payloads() -> &'static PayloadPolicy {
    &get().payloads
}

pub fn limits() -> &'static Limits {
    &get().limits
}

/// The capsule size the look asks for, sanitised: each edge finite and within `1..=` the pill
/// window (`Metrics::pill_window_w` / `_h`). A panic in the look is not caught here; it ends the
/// generation through the owner thread's guard.
pub fn capsule_size(
    mode: crate::anim::Mode,
    hovered: bool,
    clipboard_visible: bool,
    label: &str,
) -> (f32, f32) {
    let m = metrics();
    let (w, h) = get()
        .painters
        .capsule_size(mode, hovered, clipboard_visible, label);
    (
        if w.is_finite() {
            w.max(1.0).min(m.pill_window_w)
        } else {
            1.0
        },
        if h.is_finite() {
            h.max(1.0).min(m.pill_window_h)
        } else {
            1.0
        },
    )
}

/// Report a diagnostic to the host's sink as `<event_prefix>_<code>`. `code` and `detail` are
/// written by this crate from fixed codes, booleans, counts, the closed surface names and numbers
/// (see `HostInputs::event_sink`); neither ever carries host text. A no-op before install; a panic
/// in the sink is caught.
pub fn report(code: &str, detail: &str) {
    let Some(config) = try_get() else {
        return;
    };
    let name = format!("{}_{}", config.event_prefix, code);
    let sink = config.event_sink;
    let _ = catch_unwind(AssertUnwindSafe(|| sink(&name, detail)));
}
