Sage design, 2026-10-11, for gap ledger row 130 (Pulse overlay acceptance list). Source reading only; nothing built or run.

Lead decisions on the open points: the hitbox fix ships in 0.2.0 (no consumer exists, and it aligns Rust with Swift); 0.1.1 is not published first, the crate goes straight to 0.2.0; the Notice hook for Tauri hosts and a Rust macOS backend wait for a HeardRight acceptance list. Experiments X1 to X3 touch a live desktop session and wait for Adrian's go-ahead.

# rightkit-overlay 0.2.0 and RightKitOverlay: design ruling

Everything below comes from reading source; nothing was built or run. No Council is needed.

## 0. Findings that shape the design

- **No app consumes either owner today.** There are no source or manifest matches for `rightkit_overlay`, `rightkit-overlay` or `RightKitOverlay` in pulse, heardright, cutright, coderight, voiceright, legion or sellright. The audit matrix records zero declarations (`/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/rightkit-consumer-matrix.json:901-906`). The crate is staged at 0.1.1 and never published (`/Volumes/D/claude/rightkit/INDEX.md:50`).
- **Consumer check is incomplete.** My grep of mailright, scraperight, viewright, genright and postright returned nothing, but I did not confirm those directories exist. Treat them as unknown.
- **HeardRight keeps local copies** of the no-activate and topmost code (`/Volumes/D/claude/heardright/tauri-app-next/src-tauri/src/pill/state.rs:718-761`, `853-865`). "Must not regress" therefore means the existing public items and RightKit's own Swift tests (`/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitOverlayTests/OverlayTests.swift`).
- **The staged 0.1.1 budget declaration will probably be refused.** `static ORIGINAL_WNDPROCS: OnceLock<Mutex<HashMap…>>` (`crates/rightkit-overlay/src/lib.rs:118`) matches the `resident-static` pattern (`packages/release/runtime-budgets.mjs:104`). The `continuous_reason` (`Cargo.toml:23`) does not name that token. I did not run the gate.
- **Rust and Swift models disagree.** Swift `place(in:)` translates an explicit hitbox (`Overlay.swift:136-143`). Rust `apply` freezes `hitbox = frame` and `place` never moves it (`lib.rs:367-376`). An earlier review flagged this (`/Volumes/D/claude/tasks/reviews/2026-10-08-astra-rightkit-review.md:43`).
- **`RightKitOverlayPanel` is `final`** (`Overlay.swift:179`). Pulse's window-level click, menu and drag handling (`pulse/mac/Notch/Sources/Notch/NotchPanel.swift:26-54`) cannot be a subclass.
- **Pulse's hover-card click-through rests on `HTTRANSPARENT`** (`pulse/windows/src/main.rs:3076-3080`). From recall of the Win32 documentation, that code only passes the hit to windows of the same thread. Whether it reaches another process's window is unverified; see experiment X1.

## 1. Windows public API (Rust)

### Decision: create and adopt

Keep adoption and add a crate-created `OverlayWindow`.

- **Adoption stays** because WebView hosts cannot let the crate create the window (HeardRight calls `window.hwnd()` at `state.rs:719`). The ledger already accepts the "app-created window + `make_noactivate`" shape (`docs/audits/2026-10-10-rightkit-gap-ledger.md:70`, `:141`).
- **Creation is added** because the invariants live in the window procedure and creation flags (`main.rs:961`, `2989-2995`). A helper-only API leaves every native host re-typing them. Pulse has eight such windows (ledger `:70`).
- **Every maintenance function is a free function on `Hwnd`**, so adopted and created windows share one path.

Constraints for the implementer:
- Units are physical pixels in virtual-screen coordinates, `i32`. Client coordinates are top-left origin.
- The crate never sets DPI awareness or initialises COM implicitly (the side-effect complaint at ledger `:70`).
- The crate spawns no thread and sets no timer; the host drives it.
- New Win32 functions are `cfg(windows)` only. Do not add more `false`-returning stubs (`lib.rs:285-300` stay as they are).
- Pure types and planners compile on every target.
- `windows-sys` has no COM wrappers, so `IVirtualDesktopManager` needs a hand-written vtable. Take the CLSID and IID from the SDK header `shobjidl_core.h`, not from memory.

### Crate root, all targets

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PxRect { pub left: i32, pub top: i32, pub right: i32, pub bottom: i32 }
impl PxRect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self;
    pub const fn from_xywh(x: i32, y: i32, width: i32, height: i32) -> Self;
    pub const fn width(self) -> i32;
    pub const fn height(self) -> i32;
    pub const fn is_empty(self) -> bool;
    pub const fn contains(self, x: i32, y: i32) -> bool;      // half-open
    pub const fn intersects(self, other: PxRect) -> bool;
    pub const fn covers(self, other: PxRect) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Hit { #[default] All, None, Regions(Vec<PxRect>) }   // client px, top-left origin
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HitAnswer { Client, Transparent }
pub fn hit_answer(hit: &Hit, client_x: i32, client_y: i32) -> HitAnswer;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Monitor { pub id: String, pub handle: isize, pub bounds: PxRect,
                     pub work: PxRect, pub dpi: u32, pub primary: bool }
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorChange { Added(Monitor), Removed(String), Changed { old: Monitor, new: Monitor } }
pub fn plan_monitor_changes(known: &[Monitor], found: &[Monitor]) -> Vec<MonitorChange>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Anchor { TopLeft, TopCenter, TopRight, CenterLeft, Center, CenterRight,
                  BottomLeft, BottomCenter, BottomRight }
pub fn anchored(area: PxRect, size: (i32, i32), anchor: Anchor, inset: (i32, i32)) -> PxRect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Occupancy { Outside, Covers, Partial }
pub fn classify_window(window: PxRect, monitor: PxRect, style: u32) -> Occupancy;
pub fn is_shell_class(name: &str) -> bool;

impl OverlayModel { pub fn set_hitbox(&mut self, hitbox: Option<Rect>); }   // ledger :29 item (J)
```

`classify_window` and `is_shell_class` port `pulse/windows/src/visibility.rs:13-64` rule for rule. `plan_monitor_changes` keys on `id` and ignores `handle`.

### `rightkit_overlay::windows`, `cfg(windows)`

```rust
pub type Hwnd = isize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayError {
    InvalidHandle, InvalidFrame, EmptyFrame, ComNotInitialized,
    Incomplete { call: &'static str },
    Win32 { call: &'static str, code: u32 },
}

// --- creation -------------------------------------------------------------
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Activation { #[default] NoActivate, Activatable }

#[derive(Clone, Debug)]
pub struct WindowConfig {
    pub title: String,
    pub rect: PxRect,
    pub activation: Activation,   // NoActivate adds WS_EX_NOACTIVATE and answers MA_NOACTIVATE
    pub topmost: bool,            // default true
    pub click_through: bool,      // WS_EX_TRANSPARENT; default false
    pub hit: Hit,                 // default Hit::All
}
impl Default for WindowConfig { /* as commented */ }

pub trait OverlayHandler {
    /// Every message the crate does not own, on the window's thread. Some(lresult) consumes it.
    fn message(&self, hwnd: Hwnd, message: u32, wparam: usize, lparam: isize) -> Option<isize> { None }
    fn notice(&self, hwnd: Hwnd, notice: Notice) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice { TaskbarCreated, DisplayChanged, DpiChanged { dpi: u32, suggested: PxRect },
                  WorkAreaChanged, Destroyed }

pub struct OverlayWindow { /* !Send + !Sync */ }
impl OverlayWindow {
    pub fn create(config: &WindowConfig, handler: Box<dyn OverlayHandler>) -> Result<Self, OverlayError>;
    pub fn hwnd(&self) -> Hwnd;
    pub fn set_hit(&self, hit: Hit);
    pub fn set_click_through(&self, enabled: bool) -> Result<(), OverlayError>;
    pub fn set_rect(&self, rect: PxRect) -> Result<(), OverlayError>;   // SWP_NOACTIVATE | SWP_NOZORDER
    pub fn present(&mut self, frame: &Frame<'_>) -> Result<(), OverlayError>;
    pub fn show(&self) -> Result<(), OverlayError>;
    pub fn hide(&self) -> Result<(), OverlayError>;
    pub fn is_shown(&self) -> bool;
}
// Drop destroys the window; the class is unregistered when the last window drops.

// --- presentation -----------------------------------------------------------
pub struct Frame<'a> {
    pub pixels: &'a [u32],                 // 0xAARRGGBB premultiplied, top-down, row-major
    pub width: u32, pub height: u32,       // each 1..=8192
    pub position: Option<(i32, i32)>,      // move in the same call
    pub refuse_fully_transparent: bool,    // Err(EmptyFrame) when every alpha is 0
}
pub fn present_layered(hwnd: Hwnd, frame: &Frame<'_>) -> Result<(), OverlayError>;

// --- any HWND: visibility, hit test, click-through -------------------------------
pub fn set_shown(hwnd: Hwnd, shown: bool, topmost: bool) -> Result<(), OverlayError>;
pub fn nchittest(hwnd: Hwnd, lparam: isize, hit: &Hit) -> isize;      // HTCLIENT or HTTRANSPARENT
pub fn set_click_through(hwnd: Hwnd, enabled: bool) -> Result<(), OverlayError>;
pub fn cursor_position() -> Option<(i32, i32)>;
pub fn screen_to_client(hwnd: Hwnd, x: i32, y: i32) -> Option<(i32, i32)>;
/// Makes the window take the pointer only inside `hit`. Returns the resulting click-through state.
pub fn update_click_through_for_cursor(hwnd: Hwnd, hit: &Hit) -> Result<bool, OverlayError>;

// --- topmost band ----------------------------------------------------------------
#[derive(Clone, Copy, Debug)]
pub struct TopmostOptions<'a> {
    pub own: &'a [Hwnd], pub skip_own_process: bool, pub skip_cloaked: bool,
    pub require_overlap: bool, pub max_walk: u32,
}
impl<'a> TopmostOptions<'a> {
    pub const fn overlap(own: &'a [Hwnd]) -> Self;   // true, true, true, 64
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopmostState { InBand, StyleLost, Covered { by: Hwnd }, Unknown }
pub fn topmost_state(hwnd: Hwnd, options: &TopmostOptions<'_>) -> TopmostState;
pub fn window_class(hwnd: Hwnd) -> Option<String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cloak(pub u32);
impl Cloak { pub const fn is_cloaked(self) -> bool; pub const fn by_shell(self) -> bool; }
pub fn cloak_state(hwnd: Hwnd) -> Option<Cloak>;      // None: the query failed

#[derive(Clone, Copy, Debug)]
pub struct RateLimit { /* min_interval, last */ }
impl RateLimit {
    pub const fn new(min_interval: std::time::Duration) -> Self;
    pub fn ready(&self, now: std::time::Instant) -> bool;
    pub fn record(&mut self, now: std::time::Instant);
    pub fn reset(&mut self);
}

#[derive(Clone, Copy, Debug)]
pub struct HealOptions<'a> { pub topmost: TopmostOptions<'a>,
                             pub follow_virtual_desktop: bool, pub companions: &'a [Hwnd] }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealAction { None, RateLimited, MovedToForegroundDesktop, DesktopMoveFailed, Raised, RaiseFailed }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HealOutcome { pub action: HealAction, pub state: TopmostState, pub cloak: Option<Cloak> }
pub fn heal(hwnd: Hwnd, options: &HealOptions<'_>, limit: &mut RateLimit,
            now: std::time::Instant, force: bool) -> HealOutcome;

// --- Explorer restart, display and work-area notices ---------------------------------
pub fn taskbar_created_message() -> u32;              // RegisterWindowMessageW("TaskbarCreated"); 0 on failure
pub fn notice_for_message(message: u32, wparam: usize, lparam: isize) -> Option<Notice>;

// --- virtual desktops (caller's thread must already have COM initialised) ----------------
pub fn is_on_current_desktop(hwnd: Hwnd) -> Result<bool, OverlayError>;
pub fn move_to_foreground_desktop(hwnds: &[Hwnd]) -> Result<(), OverlayError>;

// --- monitors and DPI ---------------------------------------------------------------------
pub fn monitors() -> Result<Vec<Monitor>, OverlayError>;   // Err(Incomplete) on a partial scan
pub fn monitor_for_rect(rect: PxRect) -> Option<Monitor>;
pub fn monitor_for_point(x: i32, y: i32) -> Option<Monitor>;
pub fn monitor_for_window(hwnd: Hwnd) -> Option<Monitor>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DpiMode { PerMonitorV2, PerMonitor, System, Unaware, Unknown }
pub fn request_per_monitor_v2() -> DpiMode;           // explicit, with fallback; never implicit
pub fn thread_dpi_mode() -> DpiMode;
pub fn coordinates_comparable() -> bool;              // PerMonitor or PerMonitorV2
pub fn dpi_for_window(hwnd: Hwnd) -> Option<u32>;
pub const BASE_DPI: u32 = 96;
pub fn scale(value: i32, dpi: u32) -> i32;

// --- full screen ----------------------------------------------------------------------------
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullScreen { Covered { by: Hwnd }, Clear, Unknown }
pub fn fullscreen_on(monitor: PxRect, own: &[Hwnd]) -> FullScreen;

// --- budget surface -------------------------------------------------------------------------
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OverlayStatus {
    pub live_windows: u32, pub subclassed_windows: u32, pub surface_bytes: u64,
    pub presents: u64, pub heals: u64, pub raises: u64, pub desktop_moves: u64,
    pub fullscreen_scans: u64, pub windows_walked: u64,
}
pub fn status() -> OverlayStatus;
```

### Behaviour contracts

**Creation (2.1, 2.10, 2.11, 2.12)**
- Style is `WS_POPUP` with `WS_EX_LAYERED | WS_EX_TOOLWINDOW`, plus `NOACTIVATE`, `TOPMOST` and `TRANSPARENT` from the config.
- The window is created hidden and without contents (`main.rs:957-959`).
- The crate's procedure owns `WM_MOUSEACTIVATE`, `WM_NCHITTEST`, `WM_ERASEBKGND` (returns 1), `WM_PAINT` (validate only) and `WM_NCDESTROY`.
- It turns `WM_DISPLAYCHANGE`, `WM_DPICHANGED`, `WM_SETTINGCHANGE` with `SPI_SETWORKAREA`, and TaskbarCreated into a `Notice`, then still forwards the message.
- Everything else goes to `OverlayHandler::message`, then `DefWindowProcW`. That covers Pulse's mouse, cursor, capture and `WM_DROPFILES` cases (`main.rs:2997-3052`).
- The handler takes `&self` because nested `SendMessage` re-enters the procedure. Hosts use interior mutability, as Pulse's `lock_state()` already does.

**Presentation and show/hide (2.4)**
- `present` is one `UpdateLayeredWindow` call with `ULW_ALPHA`, as `surface.rs:122-154`.
- `OverlayWindow::present` keeps one DIB section and rebuilds it only on a size change. `present_layered` allocates per call.
- `show` and `hide` go through `SetWindowPos`, so failure is a `Result` (`main.rs:1407-1426`). Retry bookkeeping and republish-after-show stay with the host.

**Hit testing (2.3)**
- `nchittest` unpacks screen coordinates from `lparam`, converts to client and applies `hit_answer`.
- `set_click_through` toggles `WS_EX_TRANSPARENT`. This is the cross-process route and matches what HeardRight's Tauri windows do through `set_ignore_cursor_events` (`state.rs:665`, `1114`).
- `update_click_through_for_cursor` pairs with a host cursor poll (ledger `:29` item G).
- The README must state the X1 result before promising pass-through from `HTTRANSPARENT` alone.

**Topmost (2.5)**
- `topmost_state` walks upward with `GW_HWNDPREV`, as `main.rs:1254-1311`. It applies own-process, cloak and overlap tests.
- Hitting `max_walk` returns `InBand`, matching Pulse.
- `heal` is the `heal_panel` decision (`main.rs:1141-1197`) without logging or republish. A shell-cloaked window moves to the foreground desktop when `follow_virtual_desktop` is set. Otherwise `StyleLost` or `Covered` raises with `HWND_TOPMOST` and `SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE`.
- `force` bypasses the limiter. Use it for TaskbarCreated and display changes only.
- The limiter is required whenever `require_overlap` can match another app's topmost window, or two always-on-top apps will raise each other indefinitely. Pulse passes 120 s (`main.rs:1114`).
- The 0.1.1 functions `is_out_of_topmost_band`, `reassert_topmost` and `reassert_if_needed` keep their exact semantics.

**Explorer restart**
- An `OverlayWindow` handles TaskbarCreated itself: forced raise, then `Notice::TaskbarCreated`. The host should republish and redock.
- Adopted windows call `notice_for_message` from their own procedure.
- A `Notice` hook for hosts that do not own their procedure (Tauri) is deferred until HeardRight supplies an acceptance item.

**Virtual desktops (2.6)**
- As `main.rs:1226-1250`, except that a missing COM apartment returns `ComNotInitialized` instead of being repaired silently.

**Monitors and DPI (2.7, 2.8)**
- `Monitor.id` is the device name. `dpi` is the effective DPI, or 96 when unreadable (`main.rs:797-805`).
- A partial enumeration is an error so the host never tears panels down on it (`main.rs:709-711`).

**Full screen (2.9)**
- Ports `enum_visible_window` (`main.rs:1460-1534`).
- Returns `Unknown` when coordinates are not comparable or any query fails on a candidate. `Unknown` must never hide an overlay.

### Runtime budget ruling

- **Class: held-resource, not always-on.** Under the definitions at `docs/RUNTIME-BUDGETS.md:11-12`, everything long-lived exists only while the host holds an `OverlayWindow` or keeps a subclassed window alive. A crate-owned timer or thread would make it always-on and is excluded from this design.
- **`continuous = false` would be dishonest.** The scanner would pass it only through the framework-callback blind spot the document names at `:119`.
- **Resources table:**

| `api` | `holds` | `scale` | `release` | `static_note` |
|---|---|---|---|---|
| `OverlayWindow::create` | memory | one HWND, one class reference, one DIB of width × height × 4 bytes | Drop | handler registry |
| `install_noactivate_mouse_guard` | memory | one map entry per subclassed window | `WM_NCDESTROY` | `ORIGINAL_WNDPROCS` |

- **Budget surface per host tick:**
  - `topmost_state`: one style read plus, per window walked (at most `max_walk`), five queries. No disk.
  - `fullscreen_on`: one `EnumWindows` pass with about six queries per visible top-level window until the first overlap.
  - `monitors`: only on a notice.
  - Wake-ups caused by the crate: zero. Pulse's cadence is 2 s shown and 10 s hidden (`pulse/windows/src/lifecycle.rs:6-7`).
- **Exposure:** `windows::status()`.
- **Ceilings: undeclared.** No source states them and the gate refuses inferred figures (`RUNTIME-BUDGETS.md:21`).
- **Proposed `requiredScale`:** unit `windows`, minimum 8, source ledger `:70`.
- **Proof point:** Pulse's installed-scope adoption measurement (`RUNTIME-BUDGETS.md:52`). Undeclared ceilings block Pulse's qualification until the lead declares them from that run.
- **Swift:** no budget gate applies, but `RightKitOverlayPointerTracker` is continuous while it exists. It costs one poll every 0.3 s plus one callback per mouse-moved event system-wide. It must stop in `stop()` and `deinit`, and expose `isRunning`.

## 2. Swift public API (RightKitOverlay, macOS)

```swift
public enum RightKitOverlayCoordinateOrigin: Sendable, Equatable { case lowerLeft, topLeft }

public struct RightKitOverlayHitRegions {           // existing; additions only
    public var origin: RightKitOverlayCoordinateOrigin               // default .lowerLeft
    public init(_ regions: [CGRect] = [], origin: RightKitOverlayCoordinateOrigin = .lowerLeft)
    /// `point` is lower-left window coordinates; converted when origin is .topLeft.
    public func contains(windowPoint point: CGPoint, windowHeight: CGFloat) -> Bool
}

public struct RightKitOverlayPanelConfiguration {
    public var level: NSWindow.Level
    public var collectionBehavior: NSWindow.CollectionBehavior
    public var canBecomeKey: Bool                    // false
    public var becomesKeyOnlyIfNeeded: Bool          // true
    public var isFloatingPanel: Bool                 // false
    public var hasShadow: Bool                       // false
    public var isMovable: Bool                       // true (today's value)
    public var isMovableByWindowBackground: Bool     // false
    public var animationBehavior: NSWindow.AnimationBehavior   // .default
    public var startsClickThrough: Bool              // false
    public var hitRegionOrigin: RightKitOverlayCoordinateOrigin  // .lowerLeft

    /// Exactly what init(contentRect:kind:level:) builds today.
    public static func standard(kind: RightKitOverlayKind, level: NSWindow.Level = .statusBar) -> Self
    public static let stationaryAllSpaces: NSWindow.CollectionBehavior           // [.canJoinAllSpaces, .stationary]
    public static let stationaryFullScreenAuxiliary: NSWindow.CollectionBehavior // + .fullScreenAuxiliary
    public static let transientFullScreenAuxiliary: NSWindow.CollectionBehavior  // [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
}

@MainActor public final class RightKitOverlayPanel: NSPanel {
    public init(contentRect: NSRect, kind: RightKitOverlayKind = .pill,
                configuration: RightKitOverlayPanelConfiguration)        // no default: see section 4
    public convenience init(contentRect: NSRect, kind: RightKitOverlayKind = .pill,
                            level: NSWindow.Level = .statusBar)          // existing signature

    public let configuration: RightKitOverlayPanelConfiguration
    public var hitRegions: [CGRect]                                      // existing
    public var hitRegionOrigin: RightKitOverlayCoordinateOrigin
    /// Live predicate in the region coordinate space; replaces hitRegions when set.
    public var hitShape: (@MainActor (CGPoint) -> Bool)?
    public func takesMouse(atWindowPoint point: CGPoint) -> Bool

    /// Sees every event first. Return true to consume it.
    public var eventFilter: (@MainActor (NSEvent) -> Bool)?
    public var onResignKey: (@MainActor () -> Void)?

    public func setClickThrough(_ enabled: Bool)                         // existing
    @discardableResult public func updateClickThrough(forMouseAt screenPoint: CGPoint) -> Bool  // existing
    public func showWithoutActivating()                                  // existing
    public func showAsKey()                                              // makeKeyAndOrderFront; needs canBecomeKey
    public func hide()                                                   // orderOut(nil)
    public override var canBecomeKey: Bool { get }                       // configuration.canBecomeKey
    public override var canBecomeMain: Bool { get }                      // always false
}

/// Content view for a mostly transparent panel. Never returns itself from hitTest.
@MainActor public final class RightKitOverlayContentView: NSView {
    public init(frame: NSRect, hosting content: NSView)                  // content fills and autoresizes
    public let content: NSView
    public override func hitTest(_ point: NSPoint) -> NSView?
}

@MainActor public final class RightKitOverlayPointerTracker {
    public init(panel: RightKitOverlayPanel, pollInterval: TimeInterval = 0.3,
                onPointer: (@MainActor (CGPoint) -> Void)? = nil)
    public func start(); public func stop(); public var isRunning: Bool { get }
}

public struct RightKitOverlayDisplay: Equatable {
    public let id: CGDirectDisplayID; public let frame: CGRect; public let visibleFrame: CGRect
}
public enum RightKitOverlayDisplays {
    @MainActor public static func current() -> [RightKitOverlayDisplay]
    public static func plan(current: Set<CGDirectDisplayID>, desired: [CGDirectDisplayID])
        -> (add: Set<CGDirectDisplayID>, remove: Set<CGDirectDisplayID>, hasDuplicates: Bool)
}

public enum RightKitOverlayFullScreen {
    public static func isFullScreen(screenBounds: CGRect, frontmostPID: pid_t,
        windows: [(pid: pid_t, layer: Int, bounds: CGRect)], safeAreaTopInset: CGFloat = 0) -> Bool
    @MainActor public static func frontmostAppIsFullScreen(on screen: NSScreen,
        ignoringBundleIDs: Set<String> = []) -> Bool
}
```

Decisions:

- **Key capability (2.23).** A stored flag replaces the hard-coded `false` (`Overlay.swift:224`). `canBecomeMain` stays false in every configuration; Pulse's launcher keeps it false too (`LauncherController.swift:11`). `onResignKey` serves hide-on-resign (`LauncherController.swift:12-15`).
- **Collection behaviour (2.21, 2.22).** A plain `NSWindow.CollectionBehavior` with three named presets. A `.transient` kind would add a second axis to an enum that is persisted (`Overlay.swift:9-13`).
- **Coordinate origin: explicit, default `.lowerLeft`.** Today's regions are lower-left (`Overlay.swift:155`, `198`). Pulse's are top-left (`NotchHostingView.swift:34-36`) and convert with `frame.maxY - mouse.y` (`NotchWindowController.swift:1487-1489`). Pulse sets `.topLeft` and keeps its rectangles. `hitRegions` and `hitShape` are always in the chosen space.
- **One source of truth, two layers.** The panel holds the regions and the shape. `updateClickThrough` and the content view's `hitTest` both call `takesMouse(atWindowPoint:)`.
- **Animated shapes.** `hitShape` is evaluated at hit time, so it reads the host's current presentation geometry instead of rectangles pushed per frame.
- **Content view.** It merges Pulse's container, which never claims a point (`NotchHostingView.swift:17-28`), with the rectangle filter (`:38-42`). It also gives 2.17 for free, because the hosting view is no longer the content view.
- **Class stays `final`.** `eventFilter` in `sendEvent` gives Pulse its right-click menu, click and drag hooks. An `open` class would let a subclass override `canBecomeKey` and defeat the owned invariant.
- **Pointer tracking (2.15).** The tracker is Pulse's pattern (`NotchWindowController.swift:1465-1485`): global and local monitors for `.mouseMoved` and `.leftMouseDragged`, plus a poll for a parked pointer. From recall, a global mouse monitor needs no Accessibility grant; confirm against Apple's `NSEvent` documentation before stating it in the README.
- **Displays (2.18).** Enumeration and the pure plan are shared. The duplicate-key fallback and single-controller reuse (`NotchFleet.swift:417-435`) stay with the host.
- **Full screen (2.19).** The geometry rule is `FullScreenDetector.swift:8-44` unchanged. The live query drops the hard-coded Finder and own-bundle exclusions (`:53`, `:97`) in favour of `ignoringBundleIDs`.

## 3. What stays app policy

- **Drawing and content:** shape, bitmap rendering, text, SwiftUI views, tooltips, cards, the reset and disk-image cards (2.24 is covered by regions).
- **Animation:** fold and unfold, wheel fade timing (0.6 s and 6 s at `ToolWheel.swift:144-149`, `180`), any spring curves, when `hitShape` changes.
- **Cadence and gating:** poll intervals, the `foldsForFullScreen` setting, the 0.25 s cache, the 0.35 s follow-up, heal and republish intervals, retry and logging.
- **Placement:** edge, slot, along-offset, notch size multiplier, launcher position at 38% or one third, which monitors are enabled.
- **Interaction meaning:** cell resolution by point, menus, click versus drag, grip, Option-drag, cursors, outside-click dismissal, Escape monitors (1.26).
- **File drops (2.25):** registration and the drop rectangle. The crate only has to forward `WM_DROPFILES`.
- **Foreground hand-back** for the launcher (2.11): rightkit-automation covers it (requirements doc `:430`).
- **HeardRight's pill visuals and no-activate policy:** already ruled local.

## 4. Semver and migration

**Rust: 0.2.0**, as the ledger names it (`:29`, `:70`).
- Purely additive. Every 0.1.1 item keeps its signature and behaviour, including the four non-Windows stubs.
- One flagged behaviour fix: `OverlayModel::place` translates an explicit hitbox as Swift does, and `apply` stops freezing the hitbox. Mirror the two Swift tests (`OverlayTests.swift:9-16`, `38-44`). If the lead prefers strict "defaults unchanged", drop this item; nothing else depends on it.
- `Cargo.toml` changes `continuous` to `"held-resource"` and adds the `windows-sys` features the new modules need. Take the feature list from the build, not from this design.
- With no consumer and no published version, whether 0.1.1 publishes first is the lead's call. If it does, its `continuous_reason` needs the `resident-static` token.

**Swift: next minor tag**, source compatible.
- The existing initializer keeps its signature as a convenience and delegates to `.standard(kind:level:)`.
- The new initializer has no default for `configuration`. With a default, `RightKitOverlayPanel(contentRect:kind:)` would be ambiguous.
- `.standard` reproduces `Overlay.swift:184-195` exactly: `.stationary` variants by kind, interactive start, key false, default animation behaviour. All 12 existing tests must pass unmodified.
- Mark the configuration struct `Sendable` only if it compiles under Swift 6 strict concurrency; I did not check.

**Pulse migration** is opt-in per window. `NotchPanel`, `WheelPanel`, `LauncherPanel` and the corner overlay each become a configuration. Pulse sets `isMovable = false`, `.topLeft` and `startsClickThrough = true`.

**Docs in the same change:** `docs/PLATFORM-MATRIX.md:208` and `:234`, `INDEX.md:50`, both changelogs, both READMEs (the crate README's "no tests" line at `README.md:35`).

**Deviation from the ledger:** #23's "macOS backend" in the Rust crate is not designed here. Swift owns AppKit hosts, and HeardRight's macOS pill has no acceptance list. That clause stays open.

## 5. The eight undetermined behaviours

| # | Behaviour | Settled by | Basis or experiment |
|---|---|---|---|
| 1 | Re-install after sleep, unlock, user switch | Experiment X2 | No documentation promises survival. |
| 2 | Silent removal of a slow low-level hook | Documentation, then a measurement | From recall, Microsoft's `LowLevelKeyboardProc` page says a hook exceeding `LowLevelHooksTimeout` is removed without notice. Fetch to confirm; then time Pulse's callback. Not an overlay item. |
| 3 | Notch survives an Explorer restart | Experiment X3 | TaskbarCreated is documented for tray icons only; z-order survival is not documented. |
| 4 | `GetEventDispatcherTarget` versus `GetApplicationEventTarget` | Small experiment | Register one Carbon hot key each way in an AppKit host and compare delivery. Hotkeys lane. |
| 5 | RightKit Windows code compiles | A native build on the Dell | Applies to this crate too: I have no evidence `lib.rs:104-283` has been compiled. |
| 6 | 1.5 s AX timeout leaking process-wide | Source and documentation | Read where rightkit-automation sets it on the system-wide element. From recall, Apple's header says that sets it for the whole process. Automation lane. |
| 7 | Stored shortcuts using `delete` | Source for defaults only | Real users' stored values are not observable. Make the parser accept Pulse's meaning through an alias so the question stops mattering. |
| 8 | Windows 8 DPI fallback | Documentation | From recall, Rust 1.78 and later require Windows 10, and the crate declares `rust-version = "1.98.0"` (`Cargo.toml:5`), so the binary cannot run on Windows 8. The fallback chain (`main.rs:565-598`) only matters on Windows 10 before 1703. Fetch the Rust platform-support page to confirm. |

**X1, added by this design: `HTTRANSPARENT` across processes.**
- Host: the Dell's interactive session. It moves the pointer, so it needs a quiet session and Adrian's consent.
- Put a known target window from another process under an opaque layered `OverlayWindow` with `Hit::None`. Move the pointer over it with `SendInput`, click, and record which window receives `WM_LBUTTONDOWN`.
- Repeat with `set_click_through(true)`.
- The result decides whether Pulse's hover cards need the `WS_EX_TRANSPARENT` path.

**X2: sleep and unlock.**
- Windows half on the Dell: an off-screen shown `OverlayWindow`; log `topmost_state`, `cloak_state` and `IsWindowVisible` every second across `LockWorkStation`, unlock, and a sleep-resume cycle.
- Mac half: an off-screen panel as in the existing fixture; log the window level, `isVisible`, the on-screen list entry and `ignoresMouseEvents` across lock and display sleep.
- Both lock a real session, so both need Adrian's go-ahead and a time he chooses.

**X3: Explorer restart.**
- Dell interactive session, with Adrian's go-ahead because the taskbar visibly restarts.
- Two off-screen shown windows, one with the TaskbarCreated handler and one without. Log `topmost_state` and style bits once per second, restart Explorer, continue for 180 s.
- Record whether TaskbarCreated arrives, whether either window leaves the band, and time to recovery.
- Without consent, only the handler path is testable, by posting the registered message to the test window.

## 6. Test plan

**Isolation rules for every lane**
- Test windows are 64 × 10 or smaller and placed left of the whole virtual screen. The Swift fixture already does this (`OverlayTests.swift:121-175`).
- No test synthesizes pointer or keyboard input, takes a screenshot, or raises a window on a visible monitor.
- Windows integration tests need the interactive session through the one-shot `schtasks /IT` route. A plain SSH session gets no usable desktop (ledger #92 item 1). They are `#[ignore]` by default behind an environment variable.
- Windows runs on the Dell; Swift runs on the Mac. Read `docs/rules/rightkit.md` before any Cargo command.

| Group | Host | Asserted | Cannot be asserted off-screen |
|---|---|---|---|
| Pure geometry, hit, monitor plan, anchors, classify, `RateLimit`, model hitbox | either | exact values; port Pulse's `visibility.rs` cases | none |
| W creation and presentation (2.1, 2.4, 2.10, 2.12) | Dell | style bits; created hidden; `present` Ok; `EmptyFrame` refusal; show and hide results; `IsWindowVisible`; handle and GDI counts flat over 1,000 presents | that pixels are composited; per-pixel alpha |
| W activation (2.2, 2.11) | Dell | `SendMessage(WM_MOUSEACTIVATE)` returns `MA_NOACTIVATE`; foreground window unchanged after show; `Activatable` lacks the bit | a real click not stealing focus |
| W hit test (2.3) | Dell | `SendMessage(WM_NCHITTEST)` at synthesized points for all three `Hit` values; the `WS_EX_TRANSPARENT` toggle | where a real click lands (X1) |
| W topmost (2.5) | Dell | a second off-screen topmost window from a helper process placed above gives `Covered`; same-process and non-overlapping give `InBand`; cleared style gives `StyleLost`; `heal` raises once and is then `RateLimited`; `force` bypasses | Explorer's real effect (X3) |
| W Explorer notice | Dell | posting the registered message yields `Notice::TaskbarCreated` and a raise | a real restart |
| W virtual desktops (2.6) | Dell | `ComNotInitialized` without COM; `is_on_current_desktop` true; moving to the current desktop succeeds | a shell-cloaked window; needs a real desktop switch |
| W monitors and DPI (2.7, 2.8) | Dell | `monitors()` non-empty with one primary; work within bounds; DPI at least 96; `notice_for_message` for each trigger | hot-plug and mixed-DPI moves |
| W full screen (2.9) | Dell | pass an off-screen rectangle as the "monitor" and cover it with a borderless popup: `Covered`; captioned: `Clear`; own window skipped | a real full-screen app or exclusive mode |
| W budget | Dell | `status()` counters move as expected; zero threads created | installed-scope figures (Pulse's adoption run) |
| M configuration (2.13, 2.20 to 2.23) | Mac | every field applied; `.standard` equals today's values; key-capable true or false; `canBecomeMain` false; `hide()` leaves the on-screen list; frontmost app unchanged after `showAsKey()` | above-menu-bar stacking; joining a full-screen Space; absence of a fade. `isKeyWindow` under XCTest is unknown; do not assert it. |
| M hit regions (2.14, 2.17, 2.24) | Mac | both origins against the same rectangles; `hitShape` precedence; content view returns nil outside, the subview inside, never itself; a changing `hitShape` flips the answer | a real pointer crossing an animating edge |
| M tracker (2.15) | Mac | start and stop are idempotent; `stop` removes both monitors and the timer; a poll tick with an injected location toggles click-through | global monitor delivery from real movement |
| M events (2.16) | Mac | `eventFilter` consumes a constructed `NSEvent` passed to `sendEvent`; `onResignKey` fires on `resignKey()` | menus and drags |
| M displays and full screen (2.18, 2.19) | Mac | plan add, remove and duplicates; port Pulse's geometry cases | a real full-screen Space |
| Regression | Mac | the 12 existing Swift tests unmodified; the existing Rust public items still compile | none |

## 7. Implementation order

Two lanes run in parallel; they share no source file.

**Windows lane (one Sonnet implementer, Dell)**
1. Build the crate as it stands on the Dell and record the result (settles item 5 for this crate).
2. Pure layer in the crate root with unit tests; split `lib.rs` into modules without changing any public path.
3. Monitors, DPI, cursor helpers, `notice_for_message`, `taskbar_created_message`.
4. `OverlayWindow`, `Frame`, `present_layered`, show and hide, hit test, click-through.
5. `topmost_state`, `cloak_state`, `RateLimit`, `heal`, virtual desktops.
6. `fullscreen_on`.
7. `status()`, the held-resource table, README and changelog.
8. Integration tests in the interactive session.

**Swift lane (one Sonnet implementer, Mac)**
1. Configuration struct, the new designated initializer, the convenience delegation; run the 12 existing tests.
2. Origin, `hitShape`, `takesMouse`, the content view.
3. `eventFilter`, `onResignKey`, `showAsKey`, `hide`.
4. Pointer tracker.
5. Displays and full-screen helpers.
6. Tests, README section, changelog.

**Integration (one owner, serialized)**
1. `PLATFORM-MATRIX.md`, `INDEX.md`, ledger rows #23, #64, #130.
2. Experiments X1 to X3, once Adrian agrees a time; README statements that depend on them wait for results.
3. Pulse adopts, then records the installed-scope budget measurement; the lead declares ceilings from it.

**Open for the lead**
- Whether the hitbox fix ships in 0.2.0.
- Whether 0.1.1 publishes first.
- The `Notice` hook for Tauri hosts and the Rust macOS backend both wait for a HeardRight acceptance list.
