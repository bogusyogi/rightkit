Sage design, 2026-10-11, for gap ledger rows 22 and 130 (Pulse hotkeys acceptance list). Source reading only; nothing built or run.

Lead decisions: rulings accepted. 0.1.0 is not published; first publication is 0.2.0. The Mac side gets a new RightKitSwift product `RightKitHotkeys` plus the Rust module, held to one shared chord fixture. Tier C tests (real input) run only in an isolated session and wait for Adrian's go-ahead.

# rightkit-hotkeys 0.2.0: adjudicated design

Read-only adjudication from source; nothing was built or run. External OS facts I could not fetch are marked **unverified**, and each has a spike step with a predetermined fallback, so no unit is OPEN (`open_questions == []`).

## 0. Rulings

1. **Do not publish 0.1.0; first publication is 0.2.0.** The Windows half has never compiled (`src/windows/mod.rs:3-5`). The publish gate refuses the crate anyway, because `Cargo.toml` declares no runtime class (`Cargo.toml:1-28`, `docs/RUNTIME-BUDGETS.md:7`). The only test target injects input into the live session (`tests/macos_e2e.rs:63-64`, `106`, `114`); line 114 posts a key-down with no key-up after the swallowing tap is dropped.
2. **No linking consumer exists, so breaking the API costs nothing.** A grep of `Cargo.toml` and `*.rs` under `/Volumes/D/claude` finds the crate only in itself. `genright/Cargo.toml:71` is a `[patch.crates-io]` path into a scratchpad. HeardRight and Pulse do not depend on it.
3. **The Mac side gets two homes under one contract.**
   - Pulse Mac is Swift, so its event tap, Carbon chords and chord parser become a new RightKitSwift product, `RightKitHotkeys`. `Package.swift:10-26` has no such product today.
   - The Rust `macos` module stays for Rust/Tauri hosts (HeardRight donor shapes; ledger #22 says "macOS path fits now") and gets the same semantics.
   - Drift is prevented by one shared fixture, `contracts/fixtures/hotkeys/chord-vectors.v1.json`, consumed by both test suites.
4. **Runtime class is held-resource.** `docs/RUNTIME-BUDGETS.md:12` names "a hook registration" for this class. Proof moves to app adoption (`RUNTIME-BUDGETS.md:15`).
5. **Windows cannot rewrite an event in a low-level hook.** Windows decisions are Pass or Swallow; a remap is Swallow plus a tagged `SendInput`, which rightkit-automation already owns (requirements 1.6, "Covers"). There is no dependency edge between the two crates.
6. **Council is not requested.**

## 1. Current-crate defects, verified

| # | Defect | Evidence |
|---|---|---|
| D1 | Hook thread publishes its id before any call that creates a message queue. | `windows/mod.rs:124` sends, `125-126` first touches the queue. |
| D2 | Same in `GlobalHotkeys`: id published first, the wake-up post result is ignored, and the reply wait is unbounded, so `register` can hang forever. | `mod.rs:199`, `270`, `272`. |
| D3 | `GetMessageW(...) != 0` treats the error return (-1) as "continue", so it spins. Pulse uses `> 0`. | `mod.rs:126`, `238`; `pulse/windows/src/keys.rs:225`. |
| D4 | One process-wide handler in a static; a second install fails. | `mod.rs:50`, `107-112`. |
| D5 | If the ready channel dies, the handler slot is never cleared, so every later install fails. | `mod.rs:141`. |
| D6 | Unbounded joins on drop. | `mod.rs:151-153`, `287-289`. |
| D7 | `KeyEvent` is `vk/down/injected` only. | `mod.rs:36-40`. |
| D8 | No per-id unregister, no key-up, no rollback. | `mod.rs:246-248`, `242-244`. |
| D9 | Mac tap drop loops with no bound. | `macos/tap.rs:208-212`. |
| D10 | One handler per tap; four event kinds; no autorepeat; pass or swallow only. | `tap.rs:111`, `26-31`, `55-67`, `164-167`. |
| D11 | `MiddleMouseTap` taps down only, has `on_toggle: Fn()`, and spawns a thread per click with a panicking `thread::spawn`. | `middle_mouse.rs:100`, `89`, `113`. |
| D12 | `ax_link_at` checks two ancestors, role only, no timeout. | `middle_mouse.rs:58`, `35`. |
| D13 | A bare key parses; `delete` is forward delete; `del` maps to it. | `chord.rs:325`, `217`, `101`. |
| D14 | The side of `lctrl` and similar is silently dropped in chords. | `chord.rs:314`. |
| D15 | `Meta`, `Super` and `Win` are distinct variants that map to one OS modifier, so `ChordRegistry` treats `Meta+K` and `Win+K` as different. | `chord.rs:20-23`; `conflict.rs:55`; `global.rs:160`; `windows/mod.rs:211`. |
| D16 | A handler panic crosses an `extern` boundary and aborts the process. | `mod.rs:56`, `tap.rs:118`. |
| D17 | `should_accept_fn_chord` is defined twice. | `decision.rs:14`, `ptt.rs:305`. |
| D18 | Fn-tap callbacks can fire after the handle is dropped: detached threads hold the shared state. | `fn_hotkey.rs:142-144`, `268-288`. |
| D19 | The module header cites `API.md`, which does not exist. | `mod.rs:4`. |

One fact settles part of undetermined item 7: Pulse's hub recorder does write `Backspace` as `"delete"` (`pulse/hub/src/views/Settings.tsx:745`). Stored `delete` shortcuts are therefore reachable; whether any exist is user data and stays unknown.

## 2. Public API (exact signatures)

### 2.1 Pure, all hosts

```rust
// src/dispatch.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Priority(pub i32);            // higher runs first; equal priority: earlier registration first
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HandlerId(u64);               // unique per process, never reused

pub struct HandlerList<H: ?Sized> { /* Mutex<Arc<[Entry<H>]>>, copy-on-write */ }
impl<H: ?Sized> HandlerList<H> {
    pub fn new() -> Self;
    pub fn add(&self, priority: Priority, handler: Arc<H>) -> HandlerId;
    pub fn remove(&self, id: HandlerId) -> bool;
    pub fn len(&self) -> usize;
    pub fn snapshot(&self) -> Arc<[(HandlerId, Priority, Arc<H>)]>; // lock released before any handler runs
}

pub struct PressTable { /* [u64; 4] bitset */ }   // "this press was swallowed" / "this key is down"
impl PressTable {
    pub const fn new() -> Self;
    pub fn set(&mut self, code: u8, on: bool) -> bool;   // returns previous
    pub fn get(&self, code: u8) -> bool;
    pub fn clear(&mut self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOutcome { Clean, TimedOut { os_released: bool } }
```

Semantics for both platforms:
- A handler may add or remove handlers from inside a callback.
- `remove` guarantees no new call starts after it returns; one call already in flight may finish.
- Handlers run under `catch_unwind`; a panic counts as Pass and is counted in status.

```rust
// src/chord.rs (additions; parse_chord keeps its 0.1.0 behaviour)
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect { Portable, Mac, Strict }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierRule { Any, Required, RequiredExceptFunctionKeys }
#[derive(Clone, Copy, Debug)]
pub struct ParseOptions { pub dialect: Dialect, pub modifiers: ModifierRule }

pub fn parse_chord(text: &str) -> Result<Chord, ChordError>;                 // = Portable + Any
pub fn parse_chord_with(text: &str, options: &ParseOptions) -> Result<Chord, ChordError>;
impl Chord { pub fn stored(&self, dialect: Dialect) -> String; }             // round-trips through the same dialect
// ChordError gains: NoModifier(String), AmbiguousKey { token: String, candidates: [&'static str; 2] },
//                   SidedModifier(String)

// crate root, shared by both platform registries
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)] pub struct HotkeyId(pub u32);
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum HotkeyState { Pressed, Released }
#[derive(Clone, Copy, Debug)]
pub struct HotkeyEvent { pub id: HotkeyId, pub state: HotkeyState, pub held: Option<Duration> } // Some on Released
#[derive(Clone, Copy, Debug, Default)] pub struct RegisterOptions { pub release: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictHolder { ThisRegistry(HotkeyId), System }   // System = another app or the OS
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RegisterError {
    Conflict { chord: String, holder: ConflictHolder, os_code: i32 },
    Unsupported { chord: String },
    Os { chord: String, os_code: i32, message: String },
    Stopped,           // registry thread gone or reply not received within 2 s
}
#[derive(Clone, Debug)] pub struct BatchError { pub failed: HotkeyId, pub error: RegisterError } // everything rolled back
```

### 2.2 Global chords (same method set on both platforms)

```rust
pub struct GlobalHotkeys { .. }
impl GlobalHotkeys {
    pub fn new(on_event: impl Fn(HotkeyEvent) + Send + 'static) -> Result<Self, String>; // macOS drops the Send bound
    pub fn register(&self, chord: &Chord) -> Result<HotkeyId, RegisterError>;            // default options
    pub fn register_with(&self, chord: &Chord, options: RegisterOptions) -> Result<HotkeyId, RegisterError>;
    pub fn register_id(&self, id: HotkeyId, chord: &Chord, options: RegisterOptions) -> Result<(), RegisterError>; // replaces that id
    pub fn register_all(&self, items: &[(HotkeyId, Chord, RegisterOptions)]) -> Result<(), BatchError>;
    pub fn unregister(&self, id: HotkeyId) -> bool;
    pub fn unregister_all(&self);
    pub fn registered(&self) -> usize;
    pub fn stop(self, timeout: Duration) -> StopOutcome;                                 // Drop uses 2 s
}
```

- **Conflict detection.** The registry checks its own chord map first and returns `ThisRegistry(id)`; the app maps the id to an action name for "also used by …" (requirement 1.21). Windows error 1409 and Carbon -9878 become `System`. `ChordRegistry` stays for cross-registry use and keys on the normalised chord (fixes D15).
- **Windows.**
  - `RegisterHotKey(NULL, …, MOD_NOREPEAT)` on the registry thread, `rightkit-hotkey-loop`.
  - The callback runs on that thread, never the hook thread. Pulse's two sites (requirements 1.9, 1.10) post to their own windows from it.
  - `release: true` makes the loop poll `GetAsyncKeyState` every 20 ms, only while that chord is held, and emit one `Released` with `held`. This covers ledger #22 A and E.
- **macOS.**
  - Main thread only (`!Send`); `new` fails off the main thread.
  - The callback runs on the main run loop; `Released` is filtered out unless requested.
  - The Rust event target stays `GetEventDispatcherTarget` (`global.rs:132`). There is no evidence against it; a test answers undetermined item 4.
- **Released contract.** Exactly one `Released` follows each `Pressed`, no later than the non-modifier key going up. It may come earlier when a modifier is released first. Carbon's exact timing is **unverified**.

### 2.3 Windows hook

```rust
// rightkit_hotkeys::windows
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalMods(pub u8);   // bits: LCTRL RCTRL LSHIFT RSHIFT LALT RALT LWIN RWIN
impl PhysicalMods { pub const fn ctrl(self)->bool; pub const fn shift(self)->bool;
                    pub const fn alt(self)->bool;  pub const fn win(self)->bool; pub const fn has(self, bit: u8)->bool; }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub vk: u32, pub scan: u32,
    pub down: bool,              // WM_KEYDOWN | WM_SYSKEYDOWN
    pub sys: bool,               // WM_SYSKEY*
    pub flags: u32,              // raw KBDLLHOOKSTRUCT.flags
    pub extra_info: usize,       // dwExtraInfo
    pub time: u32,
    pub repeat: bool,            // down while the crate already holds this vk as down
    pub press_swallowed: bool,   // an earlier down of this press was swallowed
    pub mods: PhysicalMods,      // physical state BEFORE this event, from non-injected events only
}
impl KeyEvent {
    pub const fn injected(&self) -> bool;            // LLKHF_INJECTED
    pub const fn lower_il_injected(&self) -> bool;   // LLKHF_LOWER_IL_INJECTED
    pub const fn alt_down(&self) -> bool;            // LLKHF_ALTDOWN
    pub const fn extended(&self) -> bool;            // LLKHF_EXTENDED
    pub const fn tagged(&self, tag: usize) -> bool;  // extra_info == tag
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum MouseButton { Left, Right, Middle, X1, X2 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseEvent { pub button: MouseButton, pub down: bool, pub x: i32, pub y: i32,   // MSLLHOOKSTRUCT.pt
                        pub flags: u32, pub extra_info: usize, pub time: u32, pub press_swallowed: bool }
impl MouseEvent { pub const fn injected(&self) -> bool; pub const fn tagged(&self, tag: usize) -> bool; }

#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum HookDecision { Pass, Swallow }

#[derive(Clone, Copy, Debug)]
pub enum Liveness { Off, ReinstallOnStarvation { check_every: Duration, min_gap: Duration } }
pub type HealthSink = Arc<dyn Fn(HookHealthEvent) + Send + Sync + 'static>;
#[non_exhaustive] #[derive(Clone)]
pub struct HookConfig { pub thread_name: String, pub keyboard: bool, pub mouse_buttons: bool,
    pub pair_swallowed: bool, pub stop_timeout: Duration, pub slow_callback: Duration,
    pub liveness: Liveness, pub on_health: Option<HealthSink> }
impl Default for HookConfig; // "rightkit-input-hook", true, false, true, 2 s, 50 ms, Off, None

#[derive(Debug)] pub enum HookError { Install { os_error: u32, message: String }, Thread(String), ReadyTimeout, Stopped }
#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum HookHealth { Healthy, Suspect, Dead }
#[derive(Clone, Debug)]
pub enum HookHealthEvent { SlowCallback { elapsed: Duration }, OverBudget { elapsed: Duration, os_timeout: Duration },
    Reinstalled { previous_unhook_error: Option<u32> }, ReinstallFailed { os_error: u32 },
    HandlerPanicked, LoopError { os_error: u32 } }
#[derive(Clone, Debug)]
pub struct HookStatus { pub installed: bool, pub thread_id: u32, pub generation: u32, pub handlers: usize,
    pub key_events: u64, pub mouse_events: u64, pub swallowed: u64, pub max_callback: Duration,
    pub slow_callbacks: u64, pub over_budget_callbacks: u64, pub handler_panics: u64,
    pub os_timeout: Duration, pub last_event_age: Option<Duration>, pub health: HookHealth }
#[derive(Clone, Copy, Debug)] pub struct ProbeResult { pub seen: bool, pub elapsed: Duration }

pub struct InputHook { .. }   // Send + Sync
impl InputHook {
    pub fn install(config: HookConfig) -> Result<InputHook, HookError>;
    pub fn add_key_handler(&self, priority: Priority,
        handler: impl Fn(&KeyEvent) -> HookDecision + Send + Sync + 'static) -> HandlerId;
    pub fn add_mouse_handler(&self, priority: Priority,
        handler: impl Fn(&MouseEvent) -> HookDecision + Send + Sync + 'static) -> HandlerId;
    pub fn remove_handler(&self, id: HandlerId) -> bool;
    pub fn status(&self) -> HookStatus;
    pub fn check(&self);                                  // run the starvation test now; no injection
    pub fn reinstall(&self) -> Result<(), HookError>;     // unhook + re-hook on the hook thread, then resync key tables
    pub fn probe(&self, timeout: Duration) -> ProbeResult; // EXPLICIT call only: injects one tagged VK 0xE8 down/up that the hook swallows
    pub fn stop(self, timeout: Duration) -> StopOutcome;  // Drop uses config.stop_timeout
}
```

- **Threads.** Key and mouse handlers run on the hook thread, inside the hook procedure, highest priority first; the first `Swallow` wins. `on_health` runs on the hook thread from the message loop, never inside the hook procedure.
- **State lookup.** The hook procedure finds its state through a `thread_local!`, not a process static. That removes D4 and D5 and allows several hooks, which tests need.
- **Pairing (requirement 1.5).** With `pair_swallowed`, once a key-down is swallowed, its repeats and its release are still offered to handlers with `press_swallowed = true`, and the final verdict is forced to Swallow. This is Pulse's rule (`keys.rs:289`, `309`, `356`), and it lets HeardRight's push-to-talk see the release it needs. Mouse buttons pair the same way.
- **Modifier state (requirement 1.3, ledger #22 C).** Seeded from `GetAsyncKeyState` at install, updated from non-injected events only, and resynced on reinstall. The resync runs outside callbacks because async key state is stale for the event being delivered (`heardright/.../recording_cancel_hook/hook.rs:493-503`).
- **Mouse.** `WM_MOUSEMOVE` and wheel messages return through `CallNextHookEx` as the first instruction: no lock, no timing, no handler.

### 2.4 macOS tap (Rust)

```rust
pub enum TapKind { KeyDown, KeyUp, FlagsChanged, LeftMouseDown, LeftMouseUp,
                   RightMouseDown, RightMouseUp, OtherMouseDown, OtherMouseUp }   // no moved/dragged/scroll, by design
#[derive(Clone, Copy, Debug)]
pub struct TapEvent { pub kind: TapKind, pub keycode: u16, pub flags: u64,
    pub button: i64,              // 0 left, 1 right, 2 middle, 3..; -1 for key events
    pub location: (f64, f64),     // global display points, top-left origin; filled for every kind
    pub user_data: i64,           // kCGEventSourceUserData: the caller marker
    pub autorepeat: bool,         // kCGKeyboardEventAutorepeat
    pub source_pid: i64,          // kCGEventSourceUnixProcessID; 0 = hardware
    pub press_swallowed: bool, pub timestamp_ns: u64 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TapRewrite { pub flags: Option<u64>, pub keycode: Option<u16>, pub user_data: Option<i64> }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapDecision { Pass, Swallow, Rewrite(TapRewrite),
    Repost { rewrite: TapRewrite, at: TapLocation } }  // swallow the original, post a rewritten copy
#[non_exhaustive] #[derive(Clone)]
pub struct TapConfig { pub name: String, pub location: TapLocation, pub listen_only: bool, pub kinds: Vec<TapKind>,
    pub pair_swallowed: bool, pub stop_timeout: Duration, pub slow_callback: Duration }
impl TapConfig { pub fn new(name: impl Into<String>, location: TapLocation, kinds: Vec<TapKind>) -> Self; }
#[derive(Clone, Debug)]
pub struct TapStatus { pub running: bool, pub enabled: bool /* CGEventTapIsEnabled, read on demand */,
    pub handlers: usize, pub events: u64, pub swallowed: u64, pub reenabled_after_timeout: u64,
    pub reenabled_after_user_input: u64, pub max_callback: Duration, pub slow_callbacks: u64, pub handler_panics: u64 }

impl EventTap {
    pub fn start(config: TapConfig) -> Result<EventTap, TapError>;
    pub fn spawn(config: TapConfig,
        handler: impl Fn(&TapEvent) -> TapDecision + Send + Sync + 'static) -> Result<EventTap, TapError>; // kept; now needs Sync
    pub fn add_handler(&self, priority: Priority,
        handler: impl Fn(&TapEvent) -> TapDecision + Send + Sync + 'static) -> HandlerId;
    pub fn remove_handler(&self, id: HandlerId) -> bool;
    pub fn status(&self) -> TapStatus;
    pub fn ensure_enabled(&self) -> bool;                 // re-enable if the system disabled it; call after wake or unlock
    pub fn stop(self, timeout: Duration) -> StopOutcome;
}

// src/macos/post.rs: production posting with a caller marker (synth.rs stays test-only)
pub enum SourceState { Private, CombinedSession, HidSystem }
pub struct PostOptions { pub at: TapLocation, pub source: SourceState, pub marker: i64 }
pub fn post_key(options: &PostOptions, keycode: u16, down: bool, flags: u64) -> bool;
pub fn post_flags_changed(options: &PostOptions, keycode: u16, flags: u64) -> bool;
#[derive(Clone, Copy, Debug)] pub struct InputPermission { pub accessibility: bool, pub listen: bool, pub post: bool }
pub fn input_permission() -> InputPermission;             // silent; never prompts
```

- **Thread.** Handlers run on the tap's own thread.
- **Rewrite.** `Rewrite` is applied at once, and lower-priority handlers see the rewritten event. This matches Pulse, where a handler mutates the event and returns pass (`pulse/.../FnCommand.swift:73-76`, `EventTapHub.swift:117-121`). `Swallow` and `Repost` are terminal. `Repost` exists because Pulse must post a modifier press first and then a marked copy of the key (`FnCommand.swift:77-82`).
- **`MiddleMouseTap`.**
  - Taps down and up, and swallows the paired up (fixes D11).
  - New constructor: `spawn_with(location, on_click: impl Fn(MiddleClick) + Send + Sync + 'static, is_link) -> Result<Self, TapError>` with `pub struct MiddleClick { pub x: f64, pub y: f64 }`.
  - `spawn` is kept as a wrapper.
  - Callback threads use `thread::Builder` and handle the error.
- **`ax_link_at_with(x, y, &LinkProbe { ancestors: u8, check_url: bool, messaging_timeout: Option<f32> })`.** Default is three ancestors, URL check on, no timeout change. Setting a timeout on the system-wide element is process-global (undetermined item 6), so it is the caller's explicit choice.
- **`FnHotkeyTap`.** Gains a `stopped` flag checked before every callback (fixes D18). `FnHotkeyCallbacks` gains `on_ptt_end: Arc<dyn Fn(PttEnd, Duration) + Send + Sync>`, filled by `noop()`, so existing struct-update literals compile (ledger #22 E).

### 2.5 Swift product `RightKitHotkeys`

macOS only; an empty target on iOS, like `RightKitWatch`. No dependency on other targets.

```swift
public enum RightKitChordDialect: Sendable { case portable, mac, strict }
public enum RightKitModifierRule: Sendable { case any, required, requiredExceptFunctionKeys }
public struct RightKitChord: Equatable, Hashable, Sendable {
    public let keyCode: UInt32; public let carbonModifiers: UInt32
    public init(parsing text: String, dialect: RightKitChordDialect, modifiers: RightKitModifierRule) throws
    public var canonical: String { get }
    public func stored(_ dialect: RightKitChordDialect) -> String
    public var display: String { get }            // ⌃⌥⇧⌘ then key glyph
}
public enum RightKitHotKeyError: Error, Equatable {
    case usedByThisRegistry(id: UInt32), takenBySystem, os(OSStatus) }
@MainActor public final class RightKitGlobalHotKeys {
    public init(signature: OSType)                // target: GetApplicationEventTarget, as Pulse uses today
    public func register(id: UInt32, chord: RightKitChord,
                         action: @escaping @MainActor () -> Void) -> Result<Void, RightKitHotKeyError>  // replaces that id
    public func unregister(id: UInt32); public func unregister(from start: UInt32); public func unregisterAll()
}
public struct RightKitTapEvent { public let type: CGEventType; public let event: CGEvent
    public let keyCode: Int64; public let flags: CGEventFlags; public let isAutorepeat: Bool
    public let buttonNumber: Int64; public let location: CGPoint; public let marker: Int64
    public let pressWasSwallowed: Bool }
public enum RightKitTapDecision { case pass, swallow, replace(CGEvent) }   // mutate `event` and return .pass to rewrite flags
public struct RightKitTapToken: Hashable, Sendable {}
public enum RightKitTapError: Error { case notPermitted, runLoopSource }
public enum RightKitTapStopOutcome { case clean, timedOut }
public final class RightKitEventTap: @unchecked Sendable {
    public init(name: String, location: CGEventTapLocation = .cgSessionEventTap, events: [CGEventType],
                listenOnly: Bool = false, pairSwallowed: Bool = true, stopTimeout: TimeInterval = 2)
    public func register(priority: Int, handler: @escaping @Sendable (RightKitTapEvent) -> RightKitTapDecision) -> RightKitTapToken
    public func unregister(_ token: RightKitTapToken)
    public func start() -> Result<Void, RightKitTapError>      // never prompts
    @discardableResult public func stop() -> RightKitTapStopOutcome
    public var isRunning: Bool { get }; public var isEnabled: Bool { get }
    @discardableResult public func ensureEnabled() -> Bool
}
public enum RightKitTapPost {
    public static func key(_ keyCode: CGKeyCode, down: Bool, flags: CGEventFlags, marker: Int64,
                           source: CGEventSourceStateID, tap: CGEventTapLocation, asFlagsChanged: Bool = false)
    public static func repost(copyOf event: CGEvent, marker: Int64, tap: CGEventTapLocation)
}
public enum RightKitInputPermission {
    public static var accessibility: Bool { get }; public static var listen: Bool { get }
    public static func requestAccessibilityPrompt()            // only on an explicit user action
}
public enum RightKitTapState: Equatable, Sendable { case off, running, needsAccessibility, failed(String) }
@MainActor public final class RightKitTapSupervisor {
    public init(recheckInterval: TimeInterval = 3, wanted: @escaping @MainActor () -> Bool,
                start: @escaping @MainActor () -> Result<Void, RightKitTapError>,
                stop: @escaping @MainActor () -> Void, ensureEnabled: @escaping @MainActor () -> Void)
    public private(set) var state: RightKitTapState; public var onChange: (@MainActor () -> Void)?
    public func reconcile(); public func invalidate()
}
```

- **Tap thread.** Named by `name`, user-interactive QoS, handlers called on it; apps hop to the main queue themselves, as Pulse does.
- **Supervisor.** The recheck timer exists only in `needsAccessibility` or `failed`, never while running. It observes `NSWorkspace` wake and session-active notifications and calls `ensureEnabled`. This is the detection for undetermined item 1 and uses no polling.

## 3. Not owned by the crate (app policy)

- Which keys remap to what: Alt-as-Ctrl, Fn-as-Command, screenshot chords, Finder cut and paste, send-hover Command+V (requirements 1.4, 1.7, 1.17, 1.23, 1.24).
- `SendInput` batches and modifier restoration: rightkit-automation (1.6). UI Automation link detection on Windows (ledger #22 D): also automation; hotkeys supplies only the mouse hook.
- When a hook or tap exists (settings, feature flags), retry cadence after a conflict, and all user-facing wording, including the Spotlight hint (1.9, 1.10, 1.21).
- Whether a bare key or Shift-only chord is acceptable: the app picks `ModifierRule`; registration itself does not judge.
- The recheck loop in Rust (no Rust consumer needs one). The Accessibility prompt, except the explicit Swift call.
- Link tests through Accessibility in Swift (Pulse keeps `AXSupport`), `NSEvent` global monitors for wheel dismissal (1.26), and main-queue delivery in Rust.
- A Windows push-to-talk gesture tap. HeardRight keeps its state machine on top of `InputHook`; candidate for 0.3.0.
- Mapping a Mac-authored `cmd+…` to the Win key on Windows.

## 4. Windows thread and queue lifecycle; silent hook removal

**Hook thread, in order:**
1. `PeekMessageW(&mut msg, NULL, WM_USER, WM_USER, PM_NOREMOVE)` to create the queue.
2. `PostThreadMessageW(own id, WM_APP_SELFTEST)` must succeed, else return `HookError::Install`. This turns the invariant behind D1 into a runtime check, not only a test.
3. Set the thread-local state, read `LowLevelHooksTimeout` into `os_timeout`, and seed the modifier bits.
4. `SetWindowsHookExW`; on failure read `GetLastError` at once, report, exit.
5. Only now send `Ok(thread_id)`. The installer waits with `recv_timeout(5 s)` and returns `ReadyTimeout` otherwise.
6. Loop on `GetMessageW`: `> 0` handle, `0` quit, `-1` emit `LoopError` and exit (fixes D3). Thread messages carry reinstall, check and probe-ack commands.
7. On exit: `UnhookWindowsHookEx` (result recorded), drop handlers on this thread, signal done.

The registry thread follows steps 1, 2, 5 and 6. Every `PostThreadMessageW` result is checked and every reply wait is bounded at 2 s (fixes D2).

**Bounded stop:**
- Post `WM_QUIT` every 50 ms until the done signal or the timeout.
- On timeout, set a `disabled` flag so the hook procedure only forwards. Attempt `UnhookWindowsHookEx` from the stopping thread (cross-thread unhook is **unverified**; the result goes in `os_released`). Detach the thread and return `TimedOut`.
- Shared state is `Arc`, so nothing is freed under a running callback.

**Slow callbacks: what is knowable.**
- Knowable: each callback's duration (measured), the configured OS timeout (registry), and the last event the hook saw.
- Not knowable: the removal itself. Microsoft's low-level hook documentation, as I recall it, says a timed-out hook on Windows 7 and later is removed silently with no notification. I did not fetch it this session, so treat it as **unverified**.

**Detection and repair, none of it injecting input:**
- **T1, after an over-budget callback.** When a callback's measured time reaches `os_timeout`, the thread posts itself a reinstall, emits `OverBudget`, then `Reinstalled { previous_unhook_error }`. A re-hook is two system calls, so the crate repairs without needing to know.
- **T2, starvation.** `check()`, or `Liveness::ReinstallOnStarvation`, compares `GetLastInputInfo` with the last event the hook saw. If the system saw input the hook did not, it reinstalls, at most once per `min_gap`. It cannot fire while the user is idle, so it never disturbs idle timers. A keyboard-only hook will reinstall during mouse-only use; that is cheap and is why the mode is opt-in.
- **Ground truth.** `probe()` is never automatic, because an injected key resets the system idle timer.
- **After any reinstall.** The key-down and swallowed tables are resynced against `GetAsyncKeyState`, so a release missed during removal, UAC or the lock screen cannot leave a key stuck as "repeat, swallowed". Pulse's static table has that latent fault (`keys.rs:87`).

Apps should also call `reinstall()` or `check()` from their own session-change or power messages (undetermined item 1 on Windows).

## 5. Chord parsing

- Two unambiguous key names: **`Backspace`** (mac 51, `VK_BACK`) and **`ForwardDelete`** (mac 117, `VK_DELETE`). `canonical()` never prints `Delete`.
- Always-safe aliases: `backspace`, `bksp` map to Backspace; `forwarddelete`, `del` map to ForwardDelete.
- The token `delete` is resolved by provenance, never guessed:

| Dialect | `delete` means | Sided modifiers (`lctrl`…) | Use |
|---|---|---|---|
| `Portable` | ForwardDelete | accepted, side dropped | exactly 0.1.0 `parse_chord`; existing stored strings keep their meaning |
| `Mac` | Backspace | rejected | Pulse's stored format; `stored(.mac)` writes `ctrl+opt+shift+cmd` order, `return`, `delete`, literal punctuation |
| `Strict` | `AmbiguousKey` error | rejected | new stores and recorders |

- `ModifierRule::Required` gives `NoModifier`. `parse_chord` keeps `Any`, so "bare key accepted" survives for callers that want it; Pulse passes `Required`.
- `cmd`, `command`, `meta`, `super` and `win` all parse to `Modifier::Meta`. The `Super` and `Win` variants stay but are deprecated and never produced.
- Mac display gains key glyphs (← → ↑ ↓ ↩ ⌫ ⌦ ⇥ ⎋) to match Pulse (`WindowShortcut.swift:77-89`).
- **Migration:** Pulse reads its existing preferences with `Mac` forever, or converts once to `canonical()` under its own store-version marker. Either way no stored shortcut changes meaning, so undetermined item 7 stops mattering.
- The fixture `chord-vectors.v1.json` lists, per dialect and text, the expected modifiers, canonical key, mac keycode, Windows VK or error. The Rust and Swift tests both load it.

## 6. Semver and migration

- **rightkit-hotkeys 0.1.0 (unpublished) → 0.2.0.** RightKitSwift takes a minor bump for the new product, pinned by tag as usual.
- **Windows.** No shim, because nothing ever compiled against it.
  - `LowLevelKeyboardHook` → `InputHook`.
  - `HookError::AlreadyInstalled` removed.
  - `KeyEvent.injected` field → method.
  - `GlobalHotkeys::new(Fn(i32))` → `Fn(HotkeyEvent)`.
- **macOS source breaks.**
  - `GlobalHotkeys` callback `(HotkeyId, HotkeyState)` → `HotkeyEvent`; `register` now takes `&self`.
  - `RegisterError::Conflict` gains `holder`.
  - `TapConfig` literals → `TapConfig::new`.
  - `spawn` handlers need `Sync`.
  - `TapDecision` and `TapKind` gain variants.
- **Kept.** `EventTap::spawn`, `MiddleMouseTap::spawn`, `FnHotkeyTap::spawn*`, `parse_chord`, `ChordRegistry`, `PttHandoff`. `decision::should_accept_fn_chord` becomes a re-export of the `ptt` one.
- **Same change set.** Lead with the release tooling: correct `packages/release/rightkit-versions.json:342` and the staged-release state before any 0.2.0 version edit — verify against the release tooling, not inspected. Then: `README.md`, `CHANGELOG.md`, `Cargo.toml` runtime metadata, `docs/PLATFORM-MATRIX.md:61` and `:173`, gap ledger rows #22, #120, #130, and removal of the `API.md` citation.

## 7. Always-on budget surface

Held-resource: nothing below exists unless the caller holds the value.

| Held value | Threads | Wake-ups | Holds |
|---|---|---|---|
| `InputHook`, keyboard | 1, blocked in `GetMessageW` | one per key event; zero when idle | handler list, two 256-bit tables, counters; no growth |
| `InputHook`, `mouse_buttons` | same thread | one per mouse event **including moves**, up to the pointer report rate | same |
| `Liveness::ReinstallOnStarvation` | same thread | one per `check_every` | — |
| `GlobalHotkeys` (Windows) | 1, blocked | one per hotkey or command; 50/s only while a release-tracked chord is held | id map |
| `GlobalHotkeys` (macOS, Rust and Swift) | none (main run loop) | one per hotkey | Carbon refs |
| `EventTap`, `RightKitEventTap` | 1, in `CFRunLoopRun` | one per masked event; no move or scroll kinds exist | mach port, run-loop source, handler list |
| `FnHotkeyTap`, `MiddleMouseTap` | tap thread plus one short thread per gesture edge | as the tap | shared state |
| `RightKitTapSupervisor` | none | one per 3 s only while waiting for permission | one timer |

- No disk activity anywhere.
- The mouse hook is a separate switch so keyboard-only apps such as Pulse never pay per-move wake-ups.
- `Cargo.toml` needs `continuous = "held-resource"` and a `resources` entry for every file that spawns a thread, plus a `static_note` for the `thread_local!` in `windows/hook.rs`.
- `status()` exposes the counters. Measurement happens at app adoption.

## 8. Test plan

The governing rule: no test posts input into a session a person is using. `docs/CARGO-PUBLICATION.md:36` stays in force. The current `macos_e2e` is retired from default runs.

**Seams:**
- **(a) Pure processing.** `process_key(state, RawKey) -> bool`, `process_mouse`, and `process_tap(state, RawTapEvent) -> RawVerdict` operate on plain structs.
- **(b) OS traits.** `HookSys` (set/unhook), `HotkeySys` (register/unregister/key-down) and a tap `enable` function, each with a fake.
- **(c) Process-local delivery.** `PostThreadMessageW(WM_HOTKEY)` to the registry thread; a Carbon hotkey event built in-process and sent to the event target; CGEvents created but never posted and handed to the Swift tap's internal dispatch.
- **(d) Self-directed tap (spike).** A `TapTarget::Process(own pid)` tap with `CGEventPostToPid(own pid)`. Whether a per-process tap sees those events is **unverified**; the fallback is tier C.

| Tier | Where | Proves |
|---|---|---|
| **A: headless, any host, `cargo test` / `swift test`** | Mac and Dell, over ssh | Chord vectors, all dialects (1.22, U7). Handler order, ties, add/remove inside a callback, panic isolation (1.8, 1.13). Pairing and repeat including "Alt released mid-repeat" (1.5). Modifier tracking from non-injected raw events, AltGr sequence (1.3). Field decode of `dwExtraInfo` and `LLKHF_ALTDOWN` (1.1, 1.2). Rewrite composition and `Repost` verdicts (1.17). Autorepeat and all nine kinds (1.24, 1.25). Re-enable on both disabled types with a fake `enable` (1.14). Conflict mapping for 1409 and -9878, `ThisRegistry`, batch rollback, per-id unregister (1.9, 1.10, 1.20, 1.21, #22 B). Release polling with a fake key state (#22 A, E). T1/T2 decision logic and table resync (U2). |
| **A-win: headless but real Win32 threads, fake `HookSys`** | Dell | 1,000 install/stop cycles where a post to the thread succeeds immediately after `install` returns (D1, D2). Stop with a blocked handler returns `TimedOut` within timeout + 100 ms (bounded teardown). The Windows module compiles: `rightkit cargo test -p rightkit-hotkeys` on the Dell (U5). |
| **B: real OS objects, no input** | interactive desktop login; Mac part needs Accessibility | Real `SetWindowsHookExW` install/stop/reinstall with zero handlers. Real `RegisterHotKey` and Carbon registration of an obscure chord, self-conflict, unregister, 200 register/unregister cycles (1.10). Carbon delivery through seam (c) under an `NSApplication` host for both event targets (U4, dispatch half). Real tap create, bounded stop, `NotPermitted` in an untrusted binary (1.15). Idle thread-count and CPU sample for the budget table. |
| **C: real input, isolated session only** | a second logon session or VM on each host; tests refuse to run unless `RIGHTKIT_INPUT_TEST_SESSION=isolated` and the user name matches `RIGHTKIT_INPUT_TEST_USER` | Windows: tagged injection round trip, `LLKHF_INJECTED`, `LLKHF_ALTDOWN` under an injected Alt, swallow observed by an in-process window, a real hotkey firing, conflict against a helper process, and the removal experiment (block callbacks past the timeout, then `probe()`, reinstall, `probe()`), which answers U2 for that build. macOS: swallow, rewrite and repost through real taps at HID and session level, paired mouse-up, a real Carbon hotkey firing and release timing (U4), sleep/wake by hand (U1). |

**Limit.** In an isolated session every event is injected, so hardware-origin paths are proven only by tier A raw events and by the consumer's installed adoption run.

## 9. Ordered implementation plan

1. **S0, shared contract (serial; one writer; any host).**
   - Files: `src/chord.rs`, `src/dispatch.rs`, root hotkey types in `src/lib.rs`, `ptt`/`decision` dedupe, the fixture JSON, `Cargo.toml` metadata skeleton, tier A pure tests.
   - Typing: EXACT for signatures and the fixture; BOUNDED for internals.
2. **Parallel antichain after S0; each lane consumes only S0 files. One Sonnet implementer per lane (OS-framework code).**
   - **W, Dell.** `src/windows/{mod,hook,hotkeys,sys}.rs` per §2.2, §2.3, §4; tiers A-win and B. First act: get the module compiling.
   - **M-R, Mac.** `src/macos/{tap,post,global,middle_mouse,fn_hotkey}.rs` per §2.4. Rewrite `tests/macos_e2e.rs` into tier-gated tests. Spike seam (d) first: half a session, then use it or fall back to tier C.
   - **M-S, Mac.** `swift/RightKitSwift/Sources/RightKitHotkeys/`, tests, `Package.swift` product. This lane depends on the fixture only.
3. **Integration (serial, one integration owner).** Docs and version files from §6, release-tooling item first. Then `pnpm --dir rightkit test:release` and `node --test rightkit/packages/release/right-suite-contract.test.mjs`. Then the runtime-budget structural check; the exact command for a held-resource crate was not inspected.
4. **Tier C receipts** in the dedicated session on each host, recorded against the exact commit. Results for U1, U2 and U4 go back into the README and the Pulse requirements document.
5. **Adoption, outside this contract.** Pulse Windows moves `keys.rs` onto `InputHook`. Pulse Mac moves onto `RightKitHotkeys`. HeardRight adopts per ledger #22. Each records an installed-scope budget run. Publication follows Legion's standing instructions and is not decided here.

## 10. Acceptance disposition

| Items | Disposition | Proven in |
|---|---|---|
| 1.1, 1.2, 1.3, 1.5, 1.8, 1.12 | owned: Windows `InputHook` | A, A-win, B, C |
| 1.9, 1.10 | owned: Windows `GlobalHotkeys` | A, B, C |
| 1.4, 1.7 | app policy on the 1.1 primitive | Pulse adoption |
| 1.6 | rightkit-automation (already covers) | — |
| 1.11 | Pulse-side: no direct `RegisterHotKey` after cutover | Pulse adoption |
| 1.13, 1.14, 1.24, 1.25 | owned: Swift `RightKitEventTap`, Rust parity | A, B, C |
| 1.15, 1.16 | owned: Swift permission/state/supervisor; Rust `input_permission`, `ensure_enabled` | A, B, C (manual wake) |
| 1.17, 1.23 | app policy; primitives owned (HID tap, rewrite, repost, marker, pairing) | A, C |
| 1.18 | Swift tap is sufficient; Rust `MiddleMouseTap` fixed | A, C |
| 1.19 | Rust `ax_link_at_with`; Swift not owned | A with a fake, C |
| 1.20, 1.21 | owned: Swift `RightKitGlobalHotKeys`; Rust adds `ConflictHolder` | A, B, C |
| 1.22 | owned: dialects, both languages | A |
| 1.26 | not owned | — |
| U1, U2, U4 | detection designed; the answer comes from tier C | C |
| U5 | lane W, first act | A-win |
| U7 | made irrelevant by dialects | A |
| U3, U6, U8 | not hotkeys (U6 touches only the opt-in timeout in `ax_link_at_with`) | — |

**Unverified items for implementers to confirm against SDK headers or documentation before relying on them:**
- `LowLevelHooksTimeout` semantics.
- Cross-thread `UnhookWindowsHookEx`, and its return on a handle the system already removed.
- Ordering of `SendInput` issued inside a hook callback. Pulse does this in production (`keys.rs:330`).
- `MSLLHOOKSTRUCT.pt` coordinate space.
- CGEvent field and type numbers: autorepeat, source pid, left/right mouse types.
- Carbon release timing.
- `CGEventTapCreateForPid` behaviour.

**Files:**
- `/Volumes/D/claude/rightkit/crates/rightkit-hotkeys/` (`src/windows/mod.rs`, `src/macos/{tap,global,middle_mouse,fn_hotkey,synth,ffi}.rs`, `src/{chord,conflict,decision,ptt,lib}.rs`, `tests/macos_e2e.rs`, `Cargo.toml`, `README.md`)
- `/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift`
- `/Volumes/D/claude/rightkit/docs/RUNTIME-BUDGETS.md`
- `/Volumes/D/claude/rightkit/docs/PLATFORM-MATRIX.md`
- `/Volumes/D/claude/rightkit/docs/CARGO-PUBLICATION.md`
- `/Volumes/D/claude/rightkit/docs/audits/2026-10-10-rightkit-gap-ledger.md`
- `/Volumes/D/claude/rightkit/packages/release/rightkit-versions.json`
- `/Volumes/D/claude/pulse/docs/rightkit-requirements-hotkeys-overlay-automation.md`
- `/Volumes/D/claude/pulse/windows/src/keys.rs`
- `/Volumes/D/claude/pulse/mac/Notch/Sources/Conveniences/{EventTapHub,ConveniencesService,ToolWheel,FinderCutPaste}.swift`
- `/Volumes/D/claude/pulse/mac/Notch/Sources/Keyboard/FnCommand.swift`
- `/Volumes/D/claude/pulse/mac/Notch/Sources/Conveniences/Windows/WindowShortcut.swift`
- `/Volumes/D/claude/pulse/mac/Notch/Sources/Launcher/LauncherHotKey.swift`
- `/Volumes/D/claude/pulse/hub/src/views/Settings.tsx`
- `/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/{hotkey.rs,middle_mouse_win.rs,recording_cancel_hook/hook.rs}`
- `/Volumes/D/claude/genright/Cargo.toml`
