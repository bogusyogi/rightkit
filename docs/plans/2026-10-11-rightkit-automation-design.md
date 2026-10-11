Sage design, 2026-10-11, for gap ledger rows 126 (HeardRight focused-element probes, re-homed here from rightkit-control) and 130 (Pulse automation acceptance list). Source reading only; nothing built or run.

Lead decisions: all five rulings accepted. No credential override is added. `El::set_str` is made crate-private before the 0.3.0 tree is committed. The crate stays `publish = false` until an app needs a published version.

# Design: accessibility probes, observers, markers and the UIA worker in rightkit-automation

Nothing was built or run; every statement about current code comes from reading source. Paths are under `/Volumes/D/claude/rightkit/crates/rightkit-automation/` unless stated.

## Rulings

1. **Focused-element probes belong to `rightkit-automation`.** Ledger row 126 was filed against the wrong crate and should be re-homed.
2. **No credential override.** The engine refusal stays unconditional; pasting credential-shaped payloads stays app-side, built on a raw marked key-post primitive that carries no text.
3. **Caller markers use a one-shot nonce ledger.** A static tag is identification only and is forgeable on both platforms.
4. **This ships as 0.4.0 (breaking).** The 0.3.0 surface is still uncommitted in the working tree; land it first with one fix (ruling 5).
5. **`El::set_str` must become `pub(crate)` before 0.3.0 lands.** It is a public string write on a public type (`src/macos/ax.rs:268`, module public at `src/macos/mod.rs:5`) that bypasses "No override flag" (`README.md:62`). `CHANGELOG.md:7` says the helpers stayed internal; they did not.

## Why rightkit-automation owns the probes

- **It already holds the surface.**
  - macOS: public `El`, `settable`, `is_secure`, `focused_editor` (`src/macos/ax.rs:57`, `216`, `333`, `445`).
  - Windows: `GetFocusedElement`, `CurrentIsPassword`, `top_hwnd`, text readback and `SetValue` (`src/win/uia.rs:303`, `170`, `213`, `402`, `367`).
- **Its own docs list these as gaps.** `docs/MOVED_FROM.md:17` names the AXObserver notifications and the single persistent UIA MTA worker; `docs/LOCKED_RULES.md:15-16` repeats both.
- **`rightkit-control` is the wrong shape.**
  - Its `windows` dependency is optional and exists only for WebView2 (`rightkit-control/Cargo.toml:34-38`); the `mac` module is macOS-only (`rightkit-control/src/lib.rs:38-39`).
  - It reads frontmost by spawning `lsappinfo` twice (`rightkit-control/src/mac.rs:161-172`).
  - It hard-codes a 3.0 s AX timeout (`mac.rs:385`) and exposes elements as a raw `usize` pointer (`mac.rs:311`).
  - Its README sends cross-app automation to `rightkit-automation` (`rightkit-control/README.md:25`).
  - It is published, so churn there costs every consumer.
- **A new small crate is rejected.** It would be a fourth private copy of the AX FFI, and it would split `El` from `FocusPin<E>` (`src/delivery.rs:20`), which must hold the same element the probe returned.

## Defects in current code the design must fix

| Defect | Evidence |
|---|---|
| Every AX read error collapses to `None`, so a hung app reads as "no focus" | `src/macos/ax.rs:148-157`, `src/macos/mod.rs:136-139` |
| Derived elements get no timeout of their own | `el_attr` at `src/macos/ax.rs:158-160` |
| The only hit test mutates the process-wide timeout | `src/macos/ax.rs:85-91`, `114-124` |
| `Platform::frontmost` pumps the calling thread's run loop for 20 ms | `src/macos/appkit.rs:68-70`, `src/macos/ffi.rs:337-341` |
| `focused_editor` walks up to 700 children with no deadline | `src/macos/ax.rs:449`, `460` |
| Posted events carry no marker on either platform | `src/macos/events.rs:111-117`, `src/win/sys.rs:332`, `348` |
| `is_password()` turns a failed read into `false` | `src/win/uia.rs:170-172` |
| UIA calls have no deadline and join the MTA on the caller's thread | `src/win/uia.rs:27-35` |
| `LOCKED_RULES.md:15` claims CannotComplete is retried inside the deadline; no retry exists in `src/` | grep of `src/` for `retry` and `25204` |
| The live unit test hit-tests the real menu bar and resets the process default | `src/macos/ax.rs:651-672` |

## 1. Public API

### 1a. macOS `macos::ax` (additions and changes)

```rust
/// Layout change: was `pub struct El(pub Cf)`. Fields become private; the timeout travels with the element.
#[derive(Clone)]
pub struct El { /* cf: Cf, timeout: Duration */ }

/// Budget for one probe made of several AX messages.
#[derive(Clone, Copy, Debug)]
pub struct AxDeadline { /* end: Instant, per_message: Duration */ }
impl AxDeadline {
    pub const TAP_MAX: Duration = Duration::from_millis(250);
    pub fn within(total: Duration) -> Self;
    pub fn within_per_message(total: Duration, per_message: Duration) -> Self;
    pub fn remaining(&self) -> Duration;
    pub fn expired(&self) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AxUrl { pub absolute: String, pub file_path: Option<std::path::PathBuf> }

impl El {
    pub fn timeout(&self) -> Duration;
    /// Sets this element's timeout to min(remaining, per_message), floor 10 ms.
    /// Err(Error::Platform(-25204, "deadline")) when the budget is spent.
    pub fn arm(&self, deadline: &AxDeadline) -> Result<()>;

    // Ok(None) = kAXErrorNoValue (-25212) or a value of another type. Everything else is Err.
    pub fn try_el_attr(&self, name: &str) -> Result<Option<El>>;
    pub fn try_str_attr(&self, name: &str) -> Result<Option<String>>;
    pub fn try_bool_attr(&self, name: &str) -> Result<Option<bool>>;
    pub fn try_point_attr(&self, name: &str) -> Result<Option<(f64, f64)>>;
    pub fn try_size_attr(&self, name: &str) -> Result<Option<(f64, f64)>>;
    pub fn try_url_attr(&self, name: &str) -> Result<Option<AxUrl>>;   // accepts CFURL or CFString
    pub fn url_attr(&self, name: &str) -> Option<AxUrl>;
    pub fn try_settable(&self, attr: &str) -> Result<bool>;
    pub fn try_is_secure(&self) -> Result<bool>;

    pub fn set_point(&self, attr: &str, x: f64, y: f64) -> Result<()>;  // AXValueCreate type 1
    pub fn set_size(&self, attr: &str, w: f64, h: f64) -> Result<()>;   // AXValueCreate type 2
    /// AXPosition, AXSize, AXPosition. Returns the first failure; nothing is rolled back.
    pub fn set_frame(&self, frame: Rect) -> Result<()>;

    /// CFHash. Stable for the element's life; not unique, pair with `same`.
    pub fn identity_hash(&self) -> u64;
}

/// Hit test restricted to one app. No process-wide side effect.
pub fn element_at_position_in(pid: i32, x: f64, y: f64, opts: &AxOptions) -> Result<Option<El>>;

#[derive(Clone, Debug)]
pub struct ScopedHit { pub deadline: AxDeadline, pub skip_pids: Vec<i32>, pub max_candidates: usize }
/// Finds the owning pid from the on-screen window list (front to back, own pid skipped),
/// then calls `element_at_position_in`. No process-wide side effect.
pub fn element_at_position_scoped(x: f64, y: f64, hit: &ScopedHit) -> Result<Option<El>>;

/// The only named way to change the process-wide default.
pub fn set_process_default_timeout(timeout: Duration);

pub fn accessibility_trusted() -> bool;
/// AXIsProcessTrustedWithOptions with the prompt flag. Call only on an explicit user action.
pub fn prompt_for_accessibility() -> bool;
```

- **Derived elements.** Every element produced by `el_attr`, `list`, `children` or a hit test inherits the parent's stored timeout and has `AXUIElementSetMessagingTimeout` applied at creation.
- **Unchanged.** `system_wide`, `system_wide_with`, `element_at_position` and `element_at_position_with` keep their process-wide effect and their docs. No engine path calls them.
- **FFI to add in `src/macos/ffi.rs`.** `CFHash`, `CFURLGetTypeID`, `CFURLGetString`, `CFURLCopyAbsoluteURL`, `CFURLGetFileSystemRepresentation`, `AXIsProcessTrustedWithOptions`, the `AXObserver*` set, `CFRunLoop{GetCurrent,Run,Stop,WakeUp,AddSource,RemoveSource}`, `CFRunLoopSource{Create,Signal,Invalidate}`, `CGEventGetIntegerValueField`. `AXValueCreate` is already declared (`ffi.rs:115`).

### 1b. macOS `macos::probe` (new)

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrontmostApp {
    pub pid: i32,
    pub bundle_id: Option<String>,      // never the lowercased-name fallback of appkit.rs:43
    pub bundle_path: Option<std::path::PathBuf>,
    pub name: String,
    pub started_ms: Option<u64>,
}
/// NSWorkspace, no run-loop pump, no AX message. Fresh only when the host's main run loop runs.
pub fn frontmost_app() -> Option<FrontmostApp>;
/// Pumps the calling thread 20 ms first. Main thread of a CLI or test only.
pub fn frontmost_app_pumped() -> Option<FrontmostApp>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Secure { Yes, No, Unknown }

#[derive(Clone)]
pub struct TextInputProbe {
    pub element: El,
    pub pid: i32,
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub secure: Secure,
    pub value_settable: Option<bool>,          // None = the check itself failed
    pub selected_text_settable: Option<bool>,
    pub identity: u64,
}
#[derive(Clone)]
pub struct FocusedWindow { pub element: El, pub title: Option<String>, pub window_id: Option<u64>, pub identity: u64 }

/// Raw AXFocusedUIElement. Ok(None) = the app reports no focused element.
pub fn focused_element(pid: i32, deadline: &AxDeadline) -> Result<Option<El>>;
pub fn focused_text_input(pid: i32, deadline: &AxDeadline) -> Result<Option<TextInputProbe>>;
pub fn focused_window(pid: i32, deadline: &AxDeadline) -> Result<Option<FocusedWindow>>;
pub fn secure_field(el: &El, deadline: &AxDeadline) -> Secure;
/// The existing container-to-editor descent, made deadline-aware. Worker threads only.
pub fn focused_editor_within(pid: i32, deadline: &AxDeadline) -> Result<Option<El>>;
```

This covers the six macOS items of ledger row 126.

### 1c. macOS `macos::observe` (new)

```rust
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AxNotification {
    WindowCreated, FocusedWindowChanged, FocusedUiElementChanged, UiElementDestroyed,
    WindowMoved, WindowResized, WindowMiniaturized, WindowDeminiaturized,
    ApplicationActivated, ApplicationDeactivated, TitleChanged, ValueChanged, Other(String),
}
impl AxNotification { pub fn as_str(&self) -> &str; }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WatchId(u64);

#[derive(Clone)]
pub struct ObserverOptions {
    pub queue_capacity: usize,        // default 256
    pub command_timeout: Duration,    // default 1 s
    pub element_timeout: Duration,    // default 250 ms, applied to every El handed out
    pub max_watches: usize,           // default 64
    pub max_registrations_per_watch: usize, // default 256
    pub sink: Option<std::sync::Arc<dyn crate::events::ResourceSink>>,
}
impl Default for ObserverOptions { /* values above */ }

#[derive(Clone)]
pub struct AxEvent { pub seq: u64, pub watch: WatchId, pub pid: i32, pub notification: AxNotification, pub element: El, pub at: std::time::Instant }
pub enum AxDelivery { Event(AxEvent), Lagged { dropped: u64 } }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObserverStats { pub watches: usize, pub registrations: usize, pub delivered: u64, pub dropped: u64, pub queue_len: usize, pub queue_capacity: usize, pub thread_alive: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOutcome { Joined, Detached }

pub struct AxObserverHub { /* private */ }
pub struct AxEvents { /* private */ }

impl AxObserverHub {
    pub fn start(opts: ObserverOptions) -> Result<(AxObserverHub, AxEvents)>;
    pub fn watch_app(&self, pid: i32, notifications: &[AxNotification]) -> Result<WatchId>;
    pub fn add_element(&self, watch: WatchId, element: &El, notifications: &[AxNotification]) -> Result<()>;
    pub fn remove_element(&self, watch: WatchId, element: &El, notifications: &[AxNotification]) -> Result<()>;
    pub fn unwatch(&self, watch: WatchId) -> Result<()>;
    /// Drops watches whose pid is gone. No timer runs in the crate; the host calls this.
    pub fn reap_dead(&self) -> Vec<WatchId>;
    /// Called on the observer thread after an enqueue. Must not block.
    pub fn set_waker(&self, waker: Option<std::sync::Arc<dyn Fn() + Send + Sync>>);
    pub fn stats(&self) -> ObserverStats;
    pub fn stop(self, timeout: Duration) -> StopOutcome;
}
impl Drop for AxObserverHub { /* stop with 250 ms; never waits longer */ }
impl AxEvents {
    pub fn try_recv(&self) -> Option<AxDelivery>;
    pub fn recv_timeout(&self, timeout: Duration) -> Option<AxDelivery>;
}
```

Behaviour:
- **Thread.** One thread, `rightkit-ax-observer`, owns a CFRunLoop and every `AXObserver`. Commands reach it through a signalled version-0 run-loop source.
- **Bounded delivery.** The AX callback retains the element, builds an `AxEvent`, and calls `try_send` on a bounded channel. On a full queue the event is dropped and counted; the next receive yields `Lagged { dropped }` first, and the consumer must re-read state.
- **Registration hygiene.** On `UiElementDestroyed` the hub removes that element's entries from its own table with no IPC.
- **Bounded teardown.** `stop` sets a closed flag, removes run-loop sources, releases observers, stops the loop, and waits at most `timeout`. Past that it returns `Detached` and emits `ObserverStopped { joined: false }`.
- **Command deadline.** A command that outlives `command_timeout` returns `Err(Error::Platform(-25204, "observer command deadline"))`. It may still complete later, so `unwatch` is the cleanup.

### 1d. Caller marker: `marker` (new, platform-neutral)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind { KeyDown, KeyUp, FlagsChanged, MouseDown, MouseUp, MouseMove, Scroll }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventShape { pub kind: ShapeKind, pub code: u32, pub flags: u64 }
impl EventShape {
    pub fn mac_key(keycode: u16, down: bool, flags: u64) -> Self;   // flags masked with 0x00FF_0000
    pub fn win_key(vk: u16, up: bool, extended: bool) -> Self;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recognition { Own, Stale, Foreign }
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LedgerStats { pub issued: u64, pub own: u64, pub stale: u64, pub evicted: u64 }

pub struct PostLedger { /* tag, Mutex<VecDeque<Entry>>, ttl, capacity */ }
impl PostLedger {
    pub fn new(tag: u32) -> Self;                                   // ttl 2 s, capacity 256
    pub fn with_limits(tag: u32, ttl: Duration, capacity: usize) -> Self;
    pub fn tag(&self) -> u32;
    /// Records the shape and returns the value to stamp: (tag << 32) | nonce.
    pub fn issue(&self, shape: EventShape) -> u64;
    /// Own = live entry with this nonce and this shape; the entry is consumed.
    /// Stale = tag matches, no live entry (forged, replayed or expired). Foreign = other tag.
    pub fn recognise(&self, marker: u64, shape: EventShape) -> Recognition;
    pub fn has_tag(&self, marker: u64) -> bool;                     // no lock
    pub fn stats(&self) -> LedgerStats;
}
```

The nonce is 32 bits from `std::collections::hash_map::RandomState` (OS-seeded SipHash over a counter), so the crate keeps zero third-party dependencies on macOS.

### 1e. macOS raw post: `macos::input` (new)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostTap { Hid, Session, AnnotatedSession, Pid(i32) }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceState { Private, CombinedSession, HidSystem }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyPost { pub keycode: u16, pub down: bool, pub flags: u64, pub autorepeat: bool }
#[derive(Clone, Copy)]
pub enum Marker<'a> { None, Static(i64), Ledger(&'a PostLedger) }
#[derive(Clone, Copy)]
pub struct PostOptions<'a> { pub tap: PostTap, pub source: SourceState, pub marker: Marker<'a> }

/// Posts key events in order. No sleeps, no policy, no text or Unicode payload.
pub fn post_keys(events: &[KeyPost], opts: &PostOptions<'_>) -> Result<()>;
```

Internally, a `build_key_event(&KeyPost, source, marker_value) -> Cf` step is kept apart from the post so tests can read field 42 without posting.

Engine wiring, both platforms:

```rust
impl MacPlatform { pub fn with_post_ledger(self, ledger: std::sync::Arc<PostLedger>) -> Self; }
impl WinPlatform { pub fn with_post_ledger(self, ledger: std::sync::Arc<PostLedger>) -> Self; }

// win::input, 64-bit targets only
pub fn send_input_batch_marked(items: &[RawInput], ledger: &PostLedger) -> Result<()>;
```

With a ledger set, `events::send` stamps field 42 on every key, mouse and scroll event, and `sys::kb` and `sys::mouse` stamp `dwExtraInfo`. Windows tier 3 uses `PostMessageW` (`src/win/sys.rs:238-307`); low-level hooks never see those messages, so no marker applies there.

### 1f. Windows `win::probe` (new)

```rust
#[derive(Clone)]
pub struct UiaWorkerOptions {
    pub default_deadline: Duration,     // 600 ms
    pub queue_capacity: usize,          // 8
    pub connection_timeout: Duration,   // IUIAutomation2::ConnectionTimeout
    pub transaction_timeout: Duration,  // IUIAutomation2::TransactionTimeout
    pub max_generations: u32,           // 3 worker threads over the value's life
    pub max_text_bytes: usize,          // proof::MAX_PROOF_BYTES
    pub sink: Option<std::sync::Arc<dyn crate::events::ResourceSink>>,
}
impl Default for UiaWorkerOptions { /* values above */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiaIdentity {
    pub runtime_id: Vec<i32>,
    pub pid: u32,
    pub process_started_ms: Option<u64>,
    pub top_hwnd: isize,
    pub native_hwnd: Option<isize>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiaFacts {
    pub control_type: i32,
    pub control_name: &'static str,
    pub class_name: Option<String>,
    pub is_enabled: Option<bool>,
    pub is_keyboard_focusable: Option<bool>,
    pub has_keyboard_focus: Option<bool>,
    pub is_password: Option<bool>,       // None = the read failed; never coerced to false
    pub value_pattern: bool,
    pub value_read_only: Option<bool>,
    pub text_pattern: bool,
}
impl UiaFacts {
    /// Some(true): not disabled, and (writable ValuePattern, or TextPattern on Edit/Document).
    /// None: ValuePattern present, read-only unknown, no TextPattern. Otherwise Some(false).
    pub fn editable(&self) -> Option<bool>;
}
#[derive(Clone)]
pub struct FocusTarget { pub identity: UiaIdentity, pub facts: UiaFacts, /* private element */ }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetValidity { Same, Changed { now: Option<UiaIdentity> }, Gone }
#[derive(Clone, Debug, PartialEq)]
pub enum SetValueOutcome { Set, Refused(Error), NotStarted(Error), Unknown }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerHealth { pub generation: u32, pub stalled: bool, pub queued: usize, pub completed: u64, pub timed_out: u64, pub skipped_effects: u64 }

pub struct UiaWorker { /* private */ }
impl UiaWorker {
    pub fn start(opts: UiaWorkerOptions) -> Result<UiaWorker>;
    /// IUIAutomation::GetFocusedElement. Ok(None) = no focused element.
    pub fn focused(&self, deadline: Duration) -> Result<Option<FocusTarget>>;
    pub fn target_from_hwnd(&self, hwnd: isize, deadline: Duration) -> Result<FocusTarget>;
    pub fn validate(&self, target: &FocusTarget, deadline: Duration) -> Result<TargetValidity>;
    pub fn read_text(&self, target: &FocusTarget, deadline: Duration) -> Result<Option<crate::proof::FieldText>>;
    pub fn set_value(&self, target: &FocusTarget, text: &str, deadline: Duration) -> SetValueOutcome;
    /// Last successful `focused` result and its age. Never blocks on COM.
    pub fn last_focus(&self) -> Option<(FocusTarget, std::time::Instant)>;
    /// Non-blocking enqueue of a focus refresh; false when the queue is full or the worker is stalled.
    pub fn request_focus_refresh(&self) -> bool;
    pub fn health(&self) -> WorkerHealth;
    pub fn stop(self, timeout: Duration) -> StopOutcome;
}
```

Behaviour:
- **Thread.** One thread, `rightkit-uia`, joins the MTA, creates its own `CUIAutomation8`, and sets both `IUIAutomation2` timeouts. The caller's thread never initialises COM and never makes a UIA call.
- **Deadline.** The caller waits with `recv_timeout`. On expiry it gets `Err(Error::Platform(258, "uia deadline"))`.
- **Stall.** While a job is overdue the worker is stalled and new requests fail fast with `Err(Error::Platform(170, "uia worker busy"))`. Past 10 times the deadline a replacement thread starts, up to `max_generations`; after that every call returns `Unsupported("uia worker stalled")`.
- **Abandoned effects never run late.** Each job carries its deadline. `set_value` rechecks it immediately before the `SetValue` COM call and skips (`NotStarted`). If the deadline passes during that call the outcome is `Unknown`, and the caller must never retry.
- **`set_value` gates, in order, no override.** `is_password == Some(true)`, then `looks_secret(text)`, then `credential_label` over name, help text and automation id. These are the same three gates as `src/engine.rs:1126-1171`. An unknown password flag does not veto a write (`docs/LOCKED_RULES.md:16`).
- **`read_text` fails closed.** It refuses when `is_password` is `Some(true)` or `None`.
- **Portable core.** The worker's queue, deadline, stall and replacement logic lives in a platform-neutral private module generic over a crate-private `UiaBackend` trait, so it is testable on every host.

### 1g. Resource events (`events.rs` addition)

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResourceEvent {
    ObserverStarted, ObserverStopped { joined: bool }, ObserverLagged { dropped: u64 },
    ObserverCommandTimedOut { op: &'static str },
    UiaWorkerStarted { generation: u32 }, UiaWorkerStalled { op: &'static str, overdue_ms: u64 },
    UiaWorkerReplaced { generation: u32 }, UiaWorkerStopped { joined: bool },
}
pub trait ResourceSink: Send + Sync { fn event(&self, e: &ResourceEvent); }
```

## 2. Threading rules and latency bound

| Call | Event-tap thread (macOS) or LL-hook thread (Windows) |
|---|---|
| `El::try_*`, `arm`, `identity_hash`, `same` | Allowed, with an `AxDeadline` |
| `element_at_position_in`, `element_at_position_scoped` | Allowed |
| `probe::focused_element`, `focused_text_input`, `focused_window`, `secure_field` | Allowed |
| `probe::frontmost_app` | Allowed (no pump, no AX message) |
| `PostLedger::issue`, `recognise`, `has_tag` | Allowed (short mutex, no syscall held) |
| `macos::input::post_keys`, `send_input_batch_marked` | Allowed |
| `UiaWorker::last_focus`, `request_focus_refresh` | Allowed |
| `clip_change` | Allowed |
| `frontmost_app_pumped`, `Platform::frontmost`, `apps`, `app`, `activate` | Forbidden: 20 ms pump and run-loop re-entry |
| Anything on `Automation` (`act`, `deliver_text`, `snapshot`, `launch`) | Forbidden: sleeps at `src/engine.rs:1250`, `1289`, `1480`, `1508`, `1579` and `src/macos/events.rs:70`, `123-128` |
| `probe::focused_editor_within`, `El::set_frame`, `Platform::focused` | Forbidden: use main or a worker |
| `AxObserverHub` start, watch, unwatch, stop | Forbidden |
| Any blocking `UiaWorker` call | Forbidden on a hook thread |

**Bound.** A deadline-taking function sends each AX message with timeout `min(remaining, per_message)` and stops when the budget is spent, so it returns within `total` plus one timeout-enforcement slop. The slop is unmeasured; test M2 records it.

**Recommended budgets.** 150 ms for a hit test and 100 ms for a focus check, matching Pulse's own values (`pulse/mac/Notch/Sources/Conveniences/AXSupport.swift:10`). `AxDeadline::TAP_MAX` (250 ms) is guidance; the crate cannot know its calling thread.

**Not verified here.** The macOS threshold for `tapDisabledByTimeout`, and the Windows `LowLevelHooksTimeout` default. Do not design against either number.

## 3. What stays app policy

- **Paste safety gates and target policy.** HeardRight's stop-target selection and gating (`docs/LOCKED_RULES.md:24`), and Pulse's "is the user typing" role test over `TextInputProbe.role` and `subrole`.
- **What to do with `Secure::Unknown`, `editable() == None`, `TargetValidity::Changed`, `Recognition::Stale` and `Lagged`.**
- **Window-move guards.** Own pid, `AXFullScreen`, settable checks, remembered frames, screen geometry.
- **Auto Quit.** The debounce, the quit decision, and when to call `reap_dead`.
- **When to prompt for Accessibility.**
- **Launcher paste (ruling 2).** Pulse writes its own clipboard, activates, then calls `post_keys` with Command+V. RightKit never sees the payload, so there is no gate to bypass.

Why no override:
- **The refusal is real for Pulse.** By reading `src/policy.rs:145-150`, a 36-character UUID or a 40-character git SHA is credential-shaped, so some launcher items would be refused.
- **The engine path does not fit Pulse anyway.** `capture_pin` runs the allowlist (`src/delivery.rs:122`), and `Policy` has no wildcard (`src/policy.rs:92`), so pasting into "whatever app was in front" cannot go through it.
- **No consumer needs it.** HeardRight keeps its gates local (ledger row 126).
- **The engine never leaks the payload itself.** `fallback_copy` excludes `Blocked` (`src/delivery.rs:348-353`).

An app that wants proof-of-insertion delivery calls `looks_secret` first (exported at `src/lib.rs:64`) and routes shaped items to its own paste.

**If a consumer later proves a need**, the only acceptable shape is a per-call value on `DeliveryOptions` and `ActOptions` naming the attesting caller and a reason, recorded in the receipt and journal, permitting only the payload-shape gate. Secure-field and credential-label gates stay unconditional. That is a decision for Adrian, not an implementer.

### Marker: what is and is not achievable

| | macOS | Windows |
|---|---|---|
| Carrier | `kCGEventSourceUserData`, field 42; the hotkeys tap already reads it (`rightkit-hotkeys/src/macos/tap.rs:65-66`, `162`) | `dwExtraInfo` on `SendInput`, seen in the LL hook struct |
| OS-attested | Nothing I can confirm. Whether a poster can overwrite field 41 (source pid) is unverified; test M4 measures it | `LLKHF_INJECTED` and `LLKHF_LOWER_IL_INJECTED` are set by the system for `SendInput`; user-mode code cannot clear them, a kernel driver can |
| Forgeable | Any process allowed to post events can set field 42 to any value | Any process at equal or higher integrity can set any `dwExtraInfo` |
| With the ledger | A foreign process cannot mint an accepted mark without observing a live one, which needs an event tap; even then it gets one same-shape event accepted once, in place of ours | Same. `Own` should also require the injected flag |
| Not achievable | Proving an unmarked event is hardware; proving who posted | Proving which process injected |

Dependency: the Windows hook's `KeyEvent` carries only `injected` (`rightkit-hotkeys/src/windows/mod.rs:36`, `68`). Exposing `dwExtraInfo` there is the hotkeys lane's job; `PostLedger` takes plain integers so neither crate depends on the other.

## 4. Semver and migration

0.4.0 is breaking in these places:
- `El` loses its public tuple field.
- `MacPlatform` stops being a unit struct (`src/macos/mod.rs:17-18`).
- `El::set_str` becomes `pub(crate)`, if not already done before 0.3.0 lands.

Unchanged: the `Platform` trait, `ActOptions`, `DeliveryOptions`, `LifecycleEvent`, `Why`, `WindowInfo`, and every engine default. New enums are `#[non_exhaustive]`.

Migration steps:
- **Dependent.** `rightkit-agent-tools/Cargo.toml:21` moves to `0.4.0`.
- **Runtime-budget class.** `Cargo.toml:54` moves from `continuous = false` to `"held-resource"`, with a `resources` table per `docs/RUNTIME-BUDGETS.md:27-48`.
  - Entries: `AxObserverHub::start` and `UiaWorker::start` (thread, memory, released on Drop), and `FocusWatch::start` (thread).
  - The `AUTO` static at `src/win/uia.rs:38` needs a `static_note`.
  - **Open risk:** the held-resource rule refuses a continuous pattern in an unlisted file, and the per-call sleeps in `engine.rs`, `delivery.rs` and `events.rs` may trip it. Check `packages/release/runtime-budgets.mjs` first; if it refuses, report it as a gate finding rather than splitting the crate.
- **Publishing.** The crate is `publish = false` (`Cargo.toml:8`), and apps may only consume published versions. Flipping that is the lead's decision.
- **Docs in the same change.**
  - `README.md` capability table and Windows section.
  - `CHANGELOG.md`.
  - `docs/LOCKED_RULES.md:15-16` (observers and the worker now exist; correct the retry claim).
  - `docs/MOVED_FROM.md:17`.
  - `docs/PLATFORM-MATRIX.md:174`.
  - Ledger rows 126 and 130.
- **Hotkeys.** `ax_link_at` (`rightkit-hotkeys/src/macos/middle_mouse.rs:40-77`) should be replaced by app glue calling `element_at_position_scoped` and `url_attr`. Do not make hotkeys depend on automation.

## 5. Test plan

Rule for every group: no test posts to the HID or session tap, hit-tests the real desktop, reads the global focus, or reads another app. A test that did not run is reported as not run.

**Fixtures to build**
- `examples/ax_fixture` (macOS). A self-contained accessory-policy AppKit process with its own minimal objc FFI, since `ffi` is private.
  - It shows one small window with `orderFrontRegardless` and never activates.
  - It holds a text field, a secure text field, and a view whose `accessibilityURL` is overridden.
  - Stdin commands: `focus <field>`, `open-window`, `close-window`, `hang <ms>` (stops pumping the main loop), `clamp-size`.
- `examples/uia_fixture` (Windows). A test-owned window with EDIT, `ES_PASSWORD` and read-only EDIT children, shown with `SW_SHOWNOACTIVATE`, with a `hang` command that stops its message pump.
- A fake `UiaBackend` and a pure `ObserverCore`, both in-crate.

| Group | No permission, runs anywhere (including the broker) | Needs Accessibility on the Mac test host; fails without it |
|---|---|---|
| M1 setters, AXURL | AXValue point and size round-trip in process; `AxUrl` from an in-process CFURL and CFString; error-code mapping | `set_frame` on the fixture window, including `clamp-size`; `try_url_attr` on the fixture view |
| M2 timeout | `AxDeadline` with an injected clock; `arm` floor and expiry | Against `hang`: an app-element read, a derived-element read, and `element_at_position_in` each return within budget. This measures inheritance and slop, both currently unverified |
| M3 probes | `focus_outcome(code)` mapping; `Secure` from role and subrole; `frontmost_app` shape using the test process's own `NSRunningApplication` | Focused field, secure field, `Ok(None)` with no focus, `Err` under `hang`, window title and identity stable across two reads |
| M4 marker | Ledger: own once, replay stale, wrong shape stale, expiry, capacity eviction, tag-only forgery stale. `build_key_event` field 42 read back with no post | `PostTap::Pid(fixture)` only, with the fixture reporting fields 42 and 41. A second helper posts a forged field 41 to settle the unverified question |
| M5 observer | `ObserverCore`: bounded queue, `Lagged`, destroyed-element pruning, limits, stop after close delivers nothing | Fixture `open-window` and `close-window` produce events; overflow with capacity 2; `stop` returns `Joined` in time; under `hang`, `watch_app` times out and `stop` returns within its bound |
| W1 worker core | Fake backend on every host: deadline, busy, stall, replacement, generation cap, skipped `set_value`, `Unknown` when the deadline passes mid-effect | n/a |
| W2 UIA backend (Windows host, no grant) | `target_from_hwnd` on fixture controls: facts, `is_password`, `editable`, identity; `validate` returns Same, then Gone after destroy; `read_text` refused on password; `set_value` Set, Refused for password and for a shaped payload; `hang` yields deadline then stall | n/a |
| W3 marker | `build_inputs` with per-event ledger nonces, no injection (same pattern as `src/win/input.rs:419-449`) | n/a |

**Cannot run unattended**
- Every Accessibility column entry. The managed broker has no grant (`README.md:70`), so these run from a granted shell.
- The real `GetFocusedElement` path on Windows. It needs the fixture to hold foreground focus, which drives the session; run it only in a dedicated test session.
- Tier 4 marked posting to the HID tap, on both platforms. Build-level tests cover the stamping.

Also rewrite `live_system_wide_and_hit_test` (`src/macos/ax.rs:651-672`) to use the fixture. I could not confirm the Windows host builds any of this.

## 6. Implementation plan

Sonnet for all code lanes (unsafe FFI and security-relevant logic); Haiku for docs. One integration owner for HEAD.

1. **Serial, first.** Make `El::set_str` `pub(crate)`, then land the existing 0.3.0 tree as its own commit.
2. **Parallel, no shared files.**
   - A: `src/marker.rs` plus M4 pure tests.
   - B: the worker core module, fake backend and W1.
   - C: `examples/ax_fixture`.
   - D: `ResourceEvent` and `ResourceSink` in `src/events.rs`.
3. **Serial, one owner of `src/macos/ax.rs` and `src/macos/ffi.rs`.** The `El` timeout field with inheritance, `AxDeadline`, the `try_*` reads, setters, `AxUrl`, `identity_hash`, the two new hit tests, trust functions and FFI additions. Then M1 and M2.
4. **Parallel after step 3.**
   - E: `src/macos/probe.rs` and M3.
   - F: `src/macos/observe.rs` and M5 (needs D).
   - G: `src/macos/input.rs`, `MacPlatform::with_post_ledger`, the `events.rs` stamping (needs A).
5. **Parallel on the Windows host after A and B.**
   - H: `src/win/probe.rs` backend, `examples/uia_fixture`, W2.
   - I: `send_input_batch_marked`, `WinPlatform::with_post_ledger`, the `sys.rs` stamping, W3.
6. **Serial close.** Version 0.4.0, the runtime-budget class and table, the docs listed in section 4, the agent-tools bump, then `rightkit cargo test -p rightkit-automation` on both hosts plus the granted-shell fixture run.

## Unknown

- **HeardRight's exact `windows_uia` predicates.** Searches of `/Volumes/D/claude/heardright` timed out. `UiaFacts` exposes raw facts so HeardRight can keep its own test, but lane H must diff `editable()` and the stop-target validation against that source before freezing.
- **How Pulse's Mac app, which is Swift, would link a Rust crate.** I found no bridge. This does not change the crate design but decides whether Pulse Mac can adopt it.
- **What HEAD holds for this crate versus the working tree.** I could not run git.
