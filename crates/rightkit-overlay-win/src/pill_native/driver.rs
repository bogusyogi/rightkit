//! The native pill's single owner thread.
//!
//! One thread (`hr-pill-native`) owns the three overlays (pill, hint, clip), the visual state
//! the painters read, and every timer. It blocks in `recv()` with no timeout whenever nothing is
//! scheduled, so an idle pill costs no wakeups; frames are pumped only while something animates
//! (recording bars, a size morph, a fade, the launch bloom).
//!
//! Lock discipline: the shared visual state is only locked for short, pure reads and writes.
//! No overlay handle method is ever called while that lock is held, because a painter running on
//! the overlay thread takes the same lock.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rightkit_overlay::pill::PillEvent;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
#[cfg(not(feature = "qa-native"))]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE, WDA_MONITOR,
};

use super::clipboard::{self, ClipItem, ClipState};
use super::gfx;
use super::hint::{self, HintContent, HintState};
use super::pill::{painter::PillPainter, BarState, Effect, PillModel};
use super::visuals::{
    self, build_pill, Mode, VRect, AUX_SPACING, BOTTOM_MARGIN, BUTTONS, PILL_WINDOW_BOTTOM_PAD,
    PILL_WINDOW_H, PILL_WINDOW_W,
};

/// Window title / `OverlayConfig::name` of each surface (the titles the existing no-activate
/// sweep already knows).
const NAMES: [&str; 3] = ["pill", "Pill Hint", "Pill Recent"];

/// Visual-only hit region of the pill window: the area the capsule paints at while it morphs.
/// The overlay window is shaped to the union of its hit regions, so without it a shrinking or
/// entering capsule would be clipped to its settled rectangle. It never counts as hover and is
/// not a button (`ButtonId::region()` is `2..=7`, the capsule itself is `1`).
const R_VISUAL: u32 = 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surf {
    Pill = 0,
    Hint = 1,
    Clip = 2,
}

/// Whether an applied event also shows or hides the pill window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Show,
    Hide,
    Keep,
}

pub(crate) struct Visual {
    pub pill: PillModel,
    pub hint: Option<HintState>,
    pub clip: Option<ClipState>,
    /// Monitor scale (`dpi / 96`) the painters multiply points by.
    pub scale: f32,
}

pub(crate) struct Shared {
    pub visual: Mutex<Visual>,
    /// Latest microphone level (`f32` bits); read by the pill painter every frame.
    pub level_bits: AtomicU32,
    /// A frame pump is running for this surface.
    pub pump: [AtomicBool; 3],
}

pub(crate) enum Cmd {
    Apply { event: PillEvent, vis: Visibility },
    SetVisibility(Visibility),
    Reveal { intro: bool },
    Overlay(Surf, gfx::OverlayEvent),
    PumpIdle(Surf),
    ShowHint { content: HintContent, kind: String },
    DismissHint,
    ShowClip { items: Vec<ClipItem>, single: bool },
    DismissClip,
    Reassert,
    Reposition,
}

struct Controller {
    tx: Sender<Cmd>,
    shared: Arc<Shared>,
    handles: [gfx::Handle; 3],
}

static CONTROLLER: OnceLock<Option<Controller>> = OnceLock::new();
static PILL_SHOWN: AtomicBool = AtomicBool::new(false);
static HINT_SHOWN: AtomicBool = AtomicBool::new(false);
static CLIP_SHOWN: AtomicBool = AtomicBool::new(false);
static MONITOR_RECT: Mutex<Option<[f64; 4]>> = Mutex::new(None);
static CAPSULE_RECT: Mutex<Option<[f64; 4]>> = Mutex::new(None);
/// Last frame handed to each overlay: `[x, y, width, height, scale]`, x/y physical px, size logical.
static OVERLAY_FRAMES: Mutex<[Option<[f64; 5]>; 3]> = Mutex::new([None; 3]);

// ---------------------------------------------------------------------------------------------
// Public surface (thread-safe, non-blocking after install)
// ---------------------------------------------------------------------------------------------

/// Create the overlays and start the driver. Idempotent; `false` if the overlays could not be
/// created (every later call is then a no-op).
pub fn ensure_installed() -> bool {
    controller().is_some()
}

fn controller() -> Option<&'static Controller> {
    CONTROLLER.get_or_init(install).as_ref()
}

fn send(cmd: Cmd) {
    if let Some(controller) = controller() {
        let _ = controller.tx.send(cmd);
    }
}

fn install() -> Option<Controller> {
    gfx::register_fonts();
    let now = Instant::now();
    let shared = Arc::new(Shared {
        visual: Mutex::new(Visual {
            pill: {
                let mut pill = PillModel::new(now);
                pill.exit_grace_ms = (visuals::HOVER_EXIT_DELAY_MS as u64)
                    .saturating_sub(super::pill::HOVER_GATE_LEAVE_MS);
                pill
            },
            hint: None,
            clip: None,
            scale: 1.0,
        }),
        level_bits: AtomicU32::new(0f32.to_bits()),
        pump: [
            AtomicBool::new(false),
            AtomicBool::new(false),
            AtomicBool::new(false),
        ],
    });
    let (tx, rx) = mpsc::channel::<Cmd>();
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<[gfx::Handle; 3], String>>(1);
    let thread_shared = Arc::clone(&shared);
    let thread_tx = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("hr-pill-native".to_string())
        .spawn(move || run(thread_shared, thread_tx, rx, ready_tx));
    if let Err(error) = spawned {
        super::actions::log_message("pill_native_install_error", &format!("spawn: {error}"));
        return None;
    }
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(handles)) => {
            super::actions::log_message("pill_native_ready", "overlays=pill,hint,clip");
            Some(Controller {
                tx,
                shared,
                handles,
            })
        }
        Ok(Err(error)) => {
            super::actions::log_message("pill_native_install_error", &error);
            None
        }
        Err(error) => {
            super::actions::log_message("pill_native_install_error", &format!("ready: {error}"));
            None
        }
    }
}

/// Microphone level for the recording bars. Never installs and never wakes the driver.
pub fn voice_level(level: f32) {
    if let Some(Some(controller)) = CONTROLLER.get() {
        controller
            .shared
            .level_bits
            .store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }
}

/// Apply a pill event (`PillModel.push`) and optionally show or hide the window.
pub fn apply_event(event: &PillEvent, vis: Visibility) {
    if let PillEvent::Voice { level } = event {
        voice_level(*level);
        return;
    }
    send(Cmd::Apply {
        event: event.clone(),
        vis,
    });
}

pub fn set_visibility(vis: Visibility) {
    send(Cmd::SetVisibility(vis));
}

/// `PillModel.reveal()`: show with the spring entrance, and the one-time bloom when `intro`.
pub fn reveal(intro: bool) {
    send(Cmd::Reveal { intro });
}

pub fn is_visible() -> bool {
    PILL_SHOWN.load(Ordering::Relaxed)
}

pub fn hint_visible() -> bool {
    HINT_SHOWN.load(Ordering::Relaxed)
}

pub fn clipboard_visible() -> bool {
    CLIP_SHOWN.load(Ordering::Relaxed)
}

/// Re-assert TOPMOST on every visible overlay (guardian passes).
pub fn reassert_topmost() {
    send(Cmd::Reassert);
}

/// Recompute the placement (monitor, work area, DPI) and move the overlays.
pub fn reposition() {
    send(Cmd::Reposition);
}

/// Physical `[left, top, right, bottom]` of the monitor the pill sits on.
pub fn monitor_rect() -> Option<[f64; 4]> {
    *MONITOR_RECT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Physical-pixel `[x, y, width, height]` of the capsule as last laid out (settled target, so it
/// is valid while the pill is hidden too). The splash and Hub transitions anchor to it.
pub fn capsule_rect() -> Option<[f64; 4]> {
    *CAPSULE_RECT.lock().unwrap_or_else(|e| e.into_inner())
}

/// `[x, y, width, height, scale]` of the pill, hint and clip overlay windows (position in
/// physical px, size in logical px) plus whether each is shown. QA journeys read it to find where
/// a real pointer must go.
pub fn overlay_frames() -> [(Option<[f64; 5]>, bool); 3] {
    let frames = *OVERLAY_FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    [
        (frames[0], is_visible()),
        (frames[1], hint_visible()),
        (frames[2], clipboard_visible()),
    ]
}

/// HWNDs of the pill, hint and clip overlays that exist (delivery excludes them when it looks
/// for the dictation target).
pub fn overlay_hwnds() -> Vec<isize> {
    match controller() {
        Some(controller) => controller
            .handles
            .iter()
            .map(gfx::Handle::hwnd)
            .filter(|hwnd| *hwnd != 0)
            .collect(),
        None => Vec::new(),
    }
}

pub fn show_hint(content: HintContent, kind: &str) {
    send(Cmd::ShowHint {
        content,
        kind: kind.to_string(),
    });
}

pub fn dismiss_hint() {
    send(Cmd::DismissHint);
}

pub fn show_clipboard(items: Vec<ClipItem>, single: bool) {
    send(Cmd::ShowClip { items, single });
}

pub fn dismiss_clipboard() {
    send(Cmd::DismissClip);
}

// ---------------------------------------------------------------------------------------------
// Driver thread
// ---------------------------------------------------------------------------------------------

struct Surfaces {
    /// Kept alive for the lifetime of the thread; dropping an overlay closes it.
    _windows: Vec<gfx::Window>,
    handles: [gfx::Handle; 3],
}

/// Display-affinity values to try, in order, so a transcript on the clip card or recovery popup
/// never shows in a screen share or recording. `WDA_EXCLUDEFROMCAPTURE` (Windows 10 2004+) leaves
/// the window out of the capture entirely; older builds reject it and get `WDA_MONITOR`, which
/// captures the window as a black box. Returns the value that was accepted.
pub fn apply_capture_affinity(
    mut set: impl FnMut(u32) -> bool,
    exclude: u32,
    fallback: u32,
) -> Option<u32> {
    [exclude, fallback]
        .into_iter()
        .find(|affinity| set(*affinity))
}

/// Keep one overlay out of screen capture (Teams, Zoom and OBS shares, screenshots). Dev builds
/// that carry the QA control server skip it so `rightkit-qa` can still capture the pill; a release
/// build never has that feature.
#[cfg(not(feature = "qa-native"))]
fn exclude_from_capture(hwnd: isize, name: &str) {
    let accepted = apply_capture_affinity(
        |affinity| unsafe { SetWindowDisplayAffinity(hwnd as _, affinity) } != 0,
        WDA_EXCLUDEFROMCAPTURE,
        WDA_MONITOR,
    );
    match accepted {
        Some(WDA_EXCLUDEFROMCAPTURE) => {}
        Some(_) => super::actions::log_message(
            "pill_native_capture_affinity",
            &format!("surface={name} affinity=monitor"),
        ),
        None => super::actions::log_message(
            "pill_native_capture_affinity_error",
            &format!("surface={name} no display affinity accepted"),
        ),
    }
}

#[cfg(feature = "qa-native")]
fn exclude_from_capture(_hwnd: isize, _name: &str) {}

fn build_surfaces(shared: &Arc<Shared>, tx: &Sender<Cmd>) -> Result<Surfaces, String> {
    let mut windows = Vec::with_capacity(3);
    let mut handles: Vec<gfx::Handle> = Vec::with_capacity(3);
    for (index, name) in NAMES.into_iter().enumerate() {
        let window = gfx::create(name)?;
        let handle = window.handle();
        exclude_from_capture(handle.hwnd(), name);
        let painter = match index {
            0 => gfx::boxed(PillPainter {
                shared: Arc::clone(shared),
                tx: tx.clone(),
                bars: BarState::new(),
            }),
            1 => gfx::boxed(hint::painter::HintPainter {
                shared: Arc::clone(shared),
                tx: tx.clone(),
            }),
            _ => gfx::boxed(clipboard::painter::ClipPainter {
                shared: Arc::clone(shared),
                tx: tx.clone(),
            }),
        };
        handle.set_painter(painter);
        let surf = match index {
            0 => Surf::Pill,
            1 => Surf::Hint,
            _ => Surf::Clip,
        };
        let events = handle.events();
        let forward = tx.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("hr-pill-native-events-{index}"))
            .spawn(move || {
                for event in events.iter() {
                    if forward.send(Cmd::Overlay(surf, event)).is_err() {
                        break;
                    }
                }
            });
        if let Err(error) = spawned {
            return Err(format!("event forwarder {index}: {error}"));
        }
        windows.push(window);
        handles.push(handle);
    }
    let [a, b, c]: [gfx::Handle; 3] = handles
        .try_into()
        .map_err(|_| "overlay handle count".to_string())?;
    Ok(Surfaces {
        _windows: windows,
        handles: [a, b, c],
    })
}

fn run(
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    rx: Receiver<Cmd>,
    ready: mpsc::SyncSender<Result<[gfx::Handle; 3], String>>,
) {
    let surfaces = match build_surfaces(&shared, &tx) {
        Ok(surfaces) => surfaces,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut core = Core::new(shared, surfaces.handles.clone());
    let _ = ready.send(Ok(surfaces.handles.clone()));
    drop(tx);
    loop {
        let now = Instant::now();
        let received = match core.next_deadline() {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(deadline) => rx.recv_timeout(deadline.saturating_duration_since(now)),
        };
        match received {
            Ok(cmd) => {
                core.handle(cmd, Instant::now());
                while let Ok(more) = rx.try_recv() {
                    core.handle(more, Instant::now());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        core.on_timers(Instant::now());
    }
}

struct Placement {
    /// Horizontal centre of the monitor, physical px.
    center_x: f64,
    /// Capsule bottom edge, physical px (work-area bottom minus the 4 pt margin).
    capsule_bottom: f64,
    scale: f32,
}

impl Placement {
    fn fallback() -> Placement {
        Placement {
            center_x: 960.0,
            capsule_bottom: 1030.0,
            scale: 1.0,
        }
    }
}

fn compute_placement() -> (Placement, Option<[f64; 4]>) {
    let (x, y) = gfx::cursor_pos().unwrap_or((0, 0));
    match rightkit_overlay::windows::monitor_at(x, y) {
        Some(monitor) => {
            let s = monitor.scale();
            (
                Placement {
                    center_x: (monitor.bounds.left + monitor.bounds.right) * 0.5,
                    capsule_bottom: monitor.work.bottom - f64::from(BOTTOM_MARGIN) * s,
                    scale: s as f32,
                },
                Some([
                    monitor.bounds.left,
                    monitor.bounds.top,
                    monitor.bounds.right,
                    monitor.bounds.bottom,
                ]),
            )
        }
        None => (Placement::fallback(), None),
    }
}

/// Whether an applied pill event must re-assert the pill's TOPMOST band. Every event that changes
/// what a visible pill shows does; `Hide` and the resting `Idle` sliver do not, which keeps an
/// idle desktop free of window-position calls.
pub fn reassert_on(event: &PillEvent, pill_shown: bool) -> bool {
    pill_shown
        && matches!(
            event,
            PillEvent::Starting
                | PillEvent::Recording
                | PillEvent::Processing { .. }
                | PillEvent::Success
                | PillEvent::Status { .. }
                | PillEvent::Error { .. }
        )
}

/// Window top-left in physical screen px, size in logical px (`set_frame`'s contract).
type Frame = (i32, i32, f32, f32);

struct Core {
    shared: Arc<Shared>,
    handles: [gfx::Handle; 3],
    hover: [HashSet<u32>; 3],
    shown: [bool; 3],
    frames: [Option<Frame>; 3],
    regions: [Vec<(u32, VRect)>; 3],
    placement: Placement,
    /// Capsule rectangle in physical screen px, from the last layout (hover reconciliation).
    capsule_phys: Option<(f64, f64, f64, f64)>,
    region_refresh_at: Option<Instant>,
    click_watch_at: Option<Instant>,
    click_primed: bool,
    prev_down: bool,
    last_hitbox: Option<(f32, f32, f32, f32)>,
    pending_hitbox: Option<(f64, f64, f64, f64)>,
}

impl Core {
    fn new(shared: Arc<Shared>, handles: [gfx::Handle; 3]) -> Core {
        let (placement, monitor) = compute_placement();
        *MONITOR_RECT.lock().unwrap_or_else(|e| e.into_inner()) = monitor;
        {
            let mut visual = shared.visual.lock().unwrap_or_else(|e| e.into_inner());
            visual.scale = placement.scale;
        }
        Core {
            shared,
            handles,
            hover: [HashSet::new(), HashSet::new(), HashSet::new()],
            shown: [false; 3],
            frames: [None, None, None],
            regions: [Vec::new(), Vec::new(), Vec::new()],
            placement,
            capsule_phys: None,
            region_refresh_at: None,
            click_watch_at: None,
            click_primed: false,
            prev_down: false,
            last_hitbox: None,
            pending_hitbox: None,
        }
    }

    fn with_visual<R>(&self, f: impl FnOnce(&mut Visual) -> R) -> R {
        let mut guard = self.shared.visual.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    // --- command dispatch ---------------------------------------------------------------------

    fn handle(&mut self, cmd: Cmd, now: Instant) {
        match cmd {
            Cmd::Apply { event, vis } => self.apply(event, vis, now),
            Cmd::SetVisibility(vis) => {
                self.apply_visibility(vis, now);
                self.after_change(now);
            }
            Cmd::Reveal { intro } => {
                self.apply_visibility(Visibility::Show, now);
                let effects = self.with_visual(|v| v.pill.reveal(now, intro));
                self.run_effects(effects, now);
                self.after_change(now);
            }
            Cmd::Overlay(surf, event) => self.on_overlay(surf, event, now),
            Cmd::PumpIdle(surf) => self.on_pump_idle(surf, now),
            Cmd::ShowHint { content, kind } => self.show_hint(content, &kind, now),
            Cmd::DismissHint => self.dismiss_hint(now),
            Cmd::ShowClip { items, single } => self.show_clip(items, single, now),
            Cmd::DismissClip => self.dismiss_clip(now),
            Cmd::Reassert => {
                for index in 0..3 {
                    if self.shown[index] {
                        self.handles[index].reassert_topmost();
                    }
                }
            }
            Cmd::Reposition => {
                self.place();
                self.after_change(now);
            }
        }
    }

    fn apply(&mut self, event: PillEvent, vis: Visibility, now: Instant) {
        if !matches!(event, PillEvent::Hide) {
            let generation = if matches!(event, PillEvent::Error { .. }) {
                super::actions::actions().and_then(|a| a.terminal_error_generation())
            } else {
                None
            };
            let (effects, mode) = self.with_visual(|v| {
                let effects = v.pill.apply_event(&event, now, generation);
                (effects, v.pill.mode)
            });
            if mode != Mode::Recording {
                self.shared
                    .level_bits
                    .store(0f32.to_bits(), Ordering::Relaxed);
            }
            self.run_effects(effects, now);
        }
        self.apply_visibility(vis, now);
        // A hotkey start raises no foreground event, so nothing else would lift a pill that the
        // topmost band dropped: re-assert on every visible state change (one `SetWindowPos`).
        if reassert_on(&event, self.shown[Surf::Pill as usize]) {
            self.handles[Surf::Pill as usize].reassert_topmost();
        }
        self.after_change(now);
    }

    fn apply_visibility(&mut self, vis: Visibility, now: Instant) {
        match vis {
            Visibility::Show => self.show_pill(now),
            Visibility::Hide => self.with_visual(|v| v.pill.set_visible(false, now)),
            Visibility::Keep => {}
        }
    }

    fn show_pill(&mut self, now: Instant) {
        self.with_visual(|v| v.pill.set_visible(true, now));
        if !self.shown[Surf::Pill as usize] {
            self.place();
            self.layout_all(now);
            let handle = &self.handles[Surf::Pill as usize];
            handle.show();
            handle.reassert_topmost();
            self.shown[Surf::Pill as usize] = true;
            PILL_SHOWN.store(true, Ordering::Relaxed);
        }
    }

    fn hide_pill_window(&mut self, now: Instant) {
        let index = Surf::Pill as usize;
        if self.shown[index] {
            self.handles[index].hide();
            self.shown[index] = false;
            PILL_SHOWN.store(false, Ordering::Relaxed);
        }
        self.hover[index].clear();
        self.shared.pump[index].store(false, Ordering::SeqCst);
        self.with_visual(|v| {
            v.pill.set_pointer(false, now);
            v.pill.clear_button_hot(now);
        });
    }

    fn run_effects(&mut self, effects: Vec<Effect>, now: Instant) {
        for effect in effects {
            match effect {
                Effect::HideWindow => self.hide_pill_window(now),
                Effect::PlayIntroSound => gfx::play_intro_sound(),
                Effect::RevealPill => {
                    std::thread::spawn(|| {
                        if let Some(actions) = super::actions::actions() {
                            actions.reveal_pill();
                        }
                    });
                }
                Effect::RecoverTerminalError(generation) => {
                    std::thread::spawn(move || {
                        if let Some(actions) = super::actions::actions() {
                            if let Err(error) = actions.recover_terminal_error(generation) {
                                super::actions::log_message(
                                    "pill_native_terminal_reset_rejected",
                                    &error,
                                );
                            }
                        }
                    });
                }
            }
        }
    }

    // --- placement and layout -----------------------------------------------------------------

    fn place(&mut self) {
        let (placement, monitor) = compute_placement();
        *MONITOR_RECT.lock().unwrap_or_else(|e| e.into_inner()) = monitor;
        let scale = placement.scale;
        self.placement = placement;
        self.with_visual(|v| v.scale = scale);
    }

    /// An aux card can appear while the pill window is hidden (`recording_only`); anchor it to
    /// the monitor the user is on then.
    fn place_if_idle(&mut self) {
        if !self.shown.iter().any(|shown| *shown) {
            self.place();
        }
    }

    fn set_frame(&mut self, surf: Surf, rect: Frame) {
        let index = surf as usize;
        if self.frames[index] == Some(rect) {
            return;
        }
        self.frames[index] = Some(rect);
        OVERLAY_FRAMES.lock().unwrap_or_else(|e| e.into_inner())[index] = Some([
            f64::from(rect.0),
            f64::from(rect.1),
            f64::from(rect.2),
            f64::from(rect.3),
            f64::from(self.placement.scale),
        ]);
        self.handles[index].set_frame(gfx::frame_rect(rect.0, rect.1, rect.2, rect.3));
    }

    fn set_regions(&mut self, surf: Surf, regions: Vec<(u32, VRect)>) {
        let index = surf as usize;
        if self.regions[index] == regions {
            return;
        }
        let converted: Vec<_> = regions
            .iter()
            .map(|(id, rect)| gfx::region(*id, *rect))
            .collect();
        self.regions[index] = regions;
        self.handles[index].set_hit_regions(converted);
    }

    /// Recompute every frame and hit region from the settled state (no animation values).
    fn layout_all(&mut self, now: Instant) {
        let window_h = PILL_WINDOW_H + PILL_WINDOW_BOTTOM_PAD;
        let (scale, target_h, pill_frame, hint, clip, extent) = self.with_visual(|v| {
            let elapsed = v
                .pill
                .rec_start
                .map(|s| now.saturating_duration_since(s).as_secs() as u32)
                .unwrap_or(0);
            let scene = v.pill.settled_scene(elapsed);
            let target_h = scene.cap_h;
            let frame = build_pill(&scene, PILL_WINDOW_W, window_h, PILL_WINDOW_BOTTOM_PAD);
            let hint = v
                .hint
                .as_ref()
                .map(|h| hint::build_hint(&h.content, &[0.0; hint::REGION_COUNT], f32::MAX));
            let clip = v
                .clip
                .as_ref()
                .map(|c| clipboard::build_clip(c, &[0.0; clipboard::REGION_COUNT], f32::MAX));
            (
                v.scale,
                target_h,
                frame,
                hint,
                clip,
                v.pill.visual_extent(now),
            )
        });
        let s = f64::from(scale);

        // Pill window: fixed envelope, capsule bottom pinned 4 pt above the work area. Size is
        // logical; only the top-left is physical.
        let win_w_phys = (f64::from(PILL_WINDOW_W) * s).round();
        let win_h_phys = (f64::from(window_h) * s).round();
        let x = (self.placement.center_x - win_w_phys * 0.5).round() as i32;
        let y = (self.placement.capsule_bottom + f64::from(PILL_WINDOW_BOTTOM_PAD) * s - win_h_phys)
            .round() as i32;
        self.set_frame(Surf::Pill, (x, y, PILL_WINDOW_W, window_h));
        // Later regions sit on top of earlier ones: the animation extent (visual only), then the
        // settled capsule, then its buttons.
        let (ext_w, ext_h, ext_extra) = extent;
        let visual = VRect::new(
            (PILL_WINDOW_W - ext_w) * 0.5,
            window_h - PILL_WINDOW_BOTTOM_PAD - ext_h,
            ext_w,
            ext_h + ext_extra,
        );
        let mut pill_regions = vec![(R_VISUAL, visual), (1u32, pill_frame.capsule)];
        for (id, rect) in &pill_frame.buttons {
            pill_regions.push((id.region(), *rect));
        }
        self.set_regions(Surf::Pill, pill_regions);
        let capsule = pill_frame.capsule;
        let capsule_phys = (
            f64::from(x) + f64::from(capsule.x) * s,
            f64::from(y) + f64::from(capsule.y) * s,
            f64::from(capsule.w) * s,
            f64::from(capsule.h) * s,
        );
        self.capsule_phys = Some(capsule_phys);
        *CAPSULE_RECT.lock().unwrap_or_else(|e| e.into_inner()) = Some([
            capsule_phys.0,
            capsule_phys.1,
            capsule_phys.2,
            capsule_phys.3,
        ]);
        self.publish_hitbox(capsule, x, y, scale);

        // Aux cards stack above the capsule: clip next to it, hint above the clip (`VStack`).
        let capsule_top = self.placement.capsule_bottom - f64::from(target_h) * s;
        let mut stack_top = capsule_top;
        if let Some(frame) = clip {
            let anchor_bottom = stack_top - f64::from(AUX_SPACING) * s;
            let rect = self.aux_frame(frame.outer, frame.window, anchor_bottom, s);
            self.set_frame(Surf::Clip, rect);
            let mut regions = vec![(clipboard::R_CARD, frame.outer)];
            regions.extend(frame.regions.iter().copied());
            self.set_regions(Surf::Clip, regions);
            stack_top = anchor_bottom - f64::from(frame.outer.h) * s;
        }
        if let Some(frame) = hint {
            let anchor_bottom = stack_top - f64::from(AUX_SPACING) * s;
            let rect = self.aux_frame(frame.card, frame.window, anchor_bottom, s);
            self.set_frame(Surf::Hint, rect);
            let mut regions = vec![(hint::R_CARD, frame.card)];
            regions.extend(frame.regions.iter().copied());
            self.set_regions(Surf::Hint, regions);
        }
    }

    /// Frame of an aux window whose card (`outer`, window-relative logical px) is centred on the
    /// pill and whose bottom edge sits at `anchor_bottom` (physical px).
    fn aux_frame(&self, outer: VRect, window: (f32, f32), anchor_bottom: f64, s: f64) -> Frame {
        let x = (self.placement.center_x - f64::from(outer.cx()) * s).round() as i32;
        let y = (anchor_bottom - f64::from(outer.bottom()) * s).round() as i32;
        (x, y, window.0, window.1)
    }

    /// Keep the legacy hitbox/frame snapshots current: the Hub open/close transition anchors to
    /// them (`visible_pill_transition_frame`).
    fn publish_hitbox(&mut self, capsule: VRect, win_x: i32, win_y: i32, scale: f32) {
        let hitbox = (capsule.x, capsule.y, capsule.w, capsule.h);
        if self.last_hitbox != Some(hitbox) {
            self.last_hitbox = Some(hitbox);
            self.pending_hitbox = Some((
                f64::from(capsule.x),
                f64::from(capsule.y),
                f64::from(capsule.w),
                f64::from(capsule.h),
            ));
        }
        if let (Some(hitbox), Some(actions)) = (self.pending_hitbox, super::actions::actions()) {
            actions.publish_pill_geometry(
                hitbox,
                (f64::from(win_x), f64::from(win_y), f64::from(scale)),
            );
        }
    }

    // --- hover --------------------------------------------------------------------------------

    fn on_overlay(&mut self, surf: Surf, event: gfx::OverlayEvent, now: Instant) {
        match event {
            gfx::OverlayEvent::HoverEnter(id) => self.hover_changed(surf, id, true, now),
            gfx::OverlayEvent::HoverLeave(id) => self.hover_changed(surf, id, false, now),
            gfx::OverlayEvent::Click(id) => self.click(surf, id, now),
            gfx::OverlayEvent::DpiChanged(_) | gfx::OverlayEvent::MonitorChanged => {
                if surf == Surf::Pill {
                    self.place();
                    self.after_change(now);
                }
            }
            gfx::OverlayEvent::Closed => {
                super::actions::log_message(
                    "pill_native_overlay_closed",
                    &format!("surface={surf:?}"),
                );
            }
            _ => {}
        }
    }

    fn hover_changed(&mut self, surf: Surf, id: u32, entered: bool, now: Instant) {
        let index = surf as usize;
        if entered {
            self.hover[index].insert(id);
        } else {
            self.hover[index].remove(&id);
        }
        let ids: Vec<u32> = self.hover[index].iter().copied().collect();
        self.with_visual(|v| {
            let ms = v.pill.ms(now);
            match surf {
                Surf::Pill => {
                    v.pill
                        .set_pointer(ids.iter().any(|region| *region != R_VISUAL), now);
                    for button in BUTTONS {
                        v.pill
                            .set_button_hot(button, ids.contains(&button.region()), now);
                    }
                }
                Surf::Hint => {
                    if let Some(hint) = v.hint.as_mut() {
                        hint.set_hot(id, entered, ms);
                    }
                }
                Surf::Clip => {
                    if let Some(clip) = v.clip.as_mut() {
                        if id != clipboard::R_CARD {
                            clip.set_hot(id, entered, ms);
                        }
                        if entered {
                            // The user is reading or using the card: stop the 5 s auto-dismiss.
                            clip.autodismiss_at = None;
                        }
                    }
                }
            }
        });
        self.after_change(now);
    }

    /// `reconcileHoverFromPointer`: after any state change the pointer's real position wins over
    /// events that may predate the new layout. Returns true when the model changed.
    fn reconcile_pointer(&mut self, now: Instant) -> bool {
        if !self.shown[Surf::Pill as usize] {
            return false;
        }
        let (Some((cx, cy)), Some((rx, ry, rw, rh))) = (gfx::cursor_pos(), self.capsule_phys)
        else {
            return false;
        };
        let (px, py) = (f64::from(cx), f64::from(cy));
        let inside = px >= rx && px <= rx + rw && py >= ry && py <= ry + rh;
        if !inside {
            self.hover[Surf::Pill as usize].clear();
        }
        self.with_visual(|v| {
            let before = (v.pill.pointer_inside, v.pill.hovered);
            v.pill.set_pointer(inside, now);
            if !inside {
                v.pill.clear_button_hot(now);
            }
            before != (v.pill.pointer_inside, v.pill.hovered)
        })
    }

    // --- clicks ---------------------------------------------------------------------------------

    fn click(&mut self, surf: Surf, id: u32, now: Instant) {
        match surf {
            Surf::Pill => {
                let Some(button) = visuals::ButtonId::from_region(id) else {
                    return;
                };
                super::actions::log_message(
                    "pill_native_action",
                    &format!("action={button:?} phase=clicked"),
                );
                use visuals::ButtonId::*;
                match button {
                    Hub => {
                        if self.clip_open() {
                            self.dismiss_clip(now);
                        }
                        act_open_hub("home");
                    }
                    Start => {
                        if self.clip_open() {
                            self.dismiss_clip(now);
                        }
                        act_start();
                    }
                    Recent => act_toggle_clipboard(),
                    Cancel => act_cancel(),
                    Stop => act_stop(false),
                    Send => act_stop(true),
                }
            }
            Surf::Hint => match id {
                hint::R_CLOSE => self.dismiss_hint(now),
                hint::R_CLIPBOARD => {
                    act_screenshot_destination("clipboard");
                    self.dismiss_hint(now);
                }
                hint::R_DISK => {
                    act_screenshot_destination("disk");
                    self.dismiss_hint(now);
                }
                _ => {}
            },
            Surf::Clip => self.click_clip(id, now),
        }
    }

    fn clip_open(&self) -> bool {
        self.with_visual(|v| v.clip.is_some())
    }

    fn click_clip(&mut self, id: u32, now: Instant) {
        enum Next {
            None,
            Close,
            Copy(String, String),
            Edit,
            Hub(&'static str),
        }
        let next = self.with_visual(|v| {
            let Some(clip) = v.clip.as_mut() else {
                return Next::None;
            };
            clip.autodismiss_at = None;
            match id {
                clipboard::R_CLOSE => Next::Close,
                clipboard::R_COPY => match clip.current() {
                    Some(item) => Next::Copy(item.delivery_id.clone(), clip.draft.clone()),
                    None => Next::None,
                },
                clipboard::R_POLISH => {
                    clip.toggle_polish();
                    Next::None
                }
                clipboard::R_EDIT => Next::Edit,
                clipboard::R_VIEW_MORE => Next::Hub("history"),
                clipboard::R_OPEN_HUB => Next::Hub("home"),
                row if (clipboard::R_ROW0..clipboard::R_ROW0 + 3).contains(&row) => {
                    match clip.items.get((row - clipboard::R_ROW0) as usize) {
                        Some(item) => Next::Copy(item.delivery_id.clone(), item.transcript.clone()),
                        None => Next::None,
                    }
                }
                _ => Next::None,
            }
        });
        match next {
            Next::None => {}
            Next::Close => self.dismiss_clip(now),
            Next::Copy(delivery_id, text) => {
                act_copy(text);
                self.with_visual(|v| {
                    if let Some(clip) = v.clip.as_mut() {
                        clip.copied_id = Some(delivery_id);
                        if clip.single {
                            clip.close_at =
                                Some(now + Duration::from_millis(clipboard::CLOSE_AFTER_COPY_MS));
                        } else {
                            clip.copied_reset_at =
                                Some(now + Duration::from_millis(clipboard::COPIED_RESET_MS));
                        }
                    }
                });
            }
            Next::Edit => {
                // No in-place editor yet (see `clipboard::INLINE_EDIT_SUPPORTED`): the Hub's
                // history list is where a transcript is edited.
                act_open_hub("history");
                self.dismiss_clip(now);
            }
            Next::Hub(route) => {
                act_open_hub(route);
                self.dismiss_clip(now);
            }
        }
        self.after_change(now);
    }

    // --- hint / clip lifecycle --------------------------------------------------------------------

    fn show_hint(&mut self, content: HintContent, kind: &str, now: Instant) {
        let hide_after = content.hide_after_ms();
        self.place_if_idle();
        self.with_visual(|v| {
            let ms = v.pill.ms(now);
            let hide_at = hide_after.map(|d| now + Duration::from_millis(d));
            v.hint = Some(HintState::new(content, kind, ms, hide_at));
            v.pill.set_hint_visible(true, now);
        });
        self.hover[Surf::Hint as usize].clear();
        self.layout_all(now);
        let handle = &self.handles[Surf::Hint as usize];
        if !self.shown[Surf::Hint as usize] {
            handle.show();
            self.shown[Surf::Hint as usize] = true;
            HINT_SHOWN.store(true, Ordering::Relaxed);
        }
        handle.reassert_topmost();
        super::actions::log_message("pill_hint_show", &format!("path=native kind={kind}"));
        self.after_change(now);
    }

    fn dismiss_hint(&mut self, now: Instant) {
        let had = self.with_visual(|v| {
            let had = v.hint.take().is_some();
            v.pill.set_hint_visible(false, now);
            had
        });
        let index = Surf::Hint as usize;
        if self.shown[index] {
            self.handles[index].hide();
            self.shown[index] = false;
            HINT_SHOWN.store(false, Ordering::Relaxed);
        }
        self.hover[index].clear();
        self.regions[index].clear();
        self.shared.pump[index].store(false, Ordering::SeqCst);
        self.handles[index].stop_animation();
        if had {
            super::actions::log_message("pill_hint_dismiss", "path=native");
        }
        self.after_change(now);
    }

    fn show_clip(&mut self, items: Vec<ClipItem>, single: bool, now: Instant) {
        if items.is_empty() {
            self.dismiss_clip(now);
            return;
        }
        // `setClipboard`: opening the clip hides any hint.
        if self.with_visual(|v| v.hint.is_some()) {
            self.dismiss_hint(now);
        }
        self.place_if_idle();
        self.with_visual(|v| {
            let ms = v.pill.ms(now);
            v.clip = Some(ClipState::new(items, single, ms, now));
            v.pill.set_clip_visible(true, now);
        });
        self.hover[Surf::Clip as usize].clear();
        self.layout_all(now);
        let handle = &self.handles[Surf::Clip as usize];
        if !self.shown[Surf::Clip as usize] {
            handle.show();
            self.shown[Surf::Clip as usize] = true;
            CLIP_SHOWN.store(true, Ordering::Relaxed);
        }
        handle.reassert_topmost();
        // Click-outside watcher: first look after 250 ms, so the click that opened the card
        // (or stopped the recording) is never mistaken for an outside click.
        self.click_watch_at = Some(now + Duration::from_millis(250));
        self.click_primed = false;
        super::actions::log_message(
            "pill_clipboard_show",
            &format!("path=native single={single}"),
        );
        self.after_change(now);
    }

    fn dismiss_clip(&mut self, now: Instant) {
        let had = self.with_visual(|v| {
            let had = v.clip.take().is_some();
            v.pill.set_clip_visible(false, now);
            had
        });
        let index = Surf::Clip as usize;
        if self.shown[index] {
            self.handles[index].hide();
            self.shown[index] = false;
            CLIP_SHOWN.store(false, Ordering::Relaxed);
        }
        self.hover[index].clear();
        self.regions[index].clear();
        self.click_watch_at = None;
        self.click_primed = false;
        self.shared.pump[index].store(false, Ordering::SeqCst);
        self.handles[index].stop_animation();
        // Release the app-side holds (override transcript, "pill stays expanded").
        if let Some(actions) = super::actions::actions() {
            actions.reset_clipboard_state();
        }
        if had {
            super::actions::log_message("pill_clipboard_dismiss", "path=native");
        }
        self.after_change(now);
    }

    // --- frames, pumps and timers -------------------------------------------------------------------

    fn needs_frames(&self, surf: Surf, now: Instant) -> bool {
        self.with_visual(|v| match surf {
            Surf::Pill => v.pill.needs_frames(now),
            Surf::Hint => {
                let ms = v.pill.ms(now);
                v.hint.as_ref().is_some_and(|h| h.needs_frames(ms))
            }
            Surf::Clip => {
                let ms = v.pill.ms(now);
                v.clip.as_ref().is_some_and(|c| c.needs_frames(ms))
            }
        })
    }

    /// Repaint every shown surface and make sure animating ones have a frame pump.
    fn kick(&mut self, now: Instant) {
        for (index, surf) in [Surf::Pill, Surf::Hint, Surf::Clip].into_iter().enumerate() {
            if !self.shown[index] {
                continue;
            }
            if self.needs_frames(surf, now) && !self.shared.pump[index].swap(true, Ordering::SeqCst)
            {
                self.handles[index].animate(gfx::pump());
            }
            self.handles[index].invalidate();
        }
    }

    fn on_pump_idle(&mut self, surf: Surf, now: Instant) {
        let index = surf as usize;
        if self.needs_frames(surf, now) {
            // Something started animating after the painter went idle; keep the pump alive.
            self.shared.pump[index].store(true, Ordering::SeqCst);
            return;
        }
        self.handles[index].stop_animation();
        self.handles[index].invalidate();
    }

    fn after_change(&mut self, now: Instant) {
        self.layout_all(now);
        if self.reconcile_pointer(now) {
            self.layout_all(now);
        }
        self.kick(now);
        // The hovered recording dock re-lays out as the timer digits change width.
        let refresh = self
            .with_visual(|v| v.pill.mode == Mode::Recording && v.pill.hovered && v.pill.visible);
        self.region_refresh_at = if refresh {
            Some(now + Duration::from_millis(500))
        } else {
            None
        };
    }

    fn next_deadline(&self) -> Option<Instant> {
        let model = self.with_visual(|v| {
            [
                v.pill.next_deadline(),
                v.hint.as_ref().and_then(|h| h.hide_at),
                v.clip.as_ref().and_then(ClipState::next_deadline),
            ]
        });
        model
            .into_iter()
            .flatten()
            .chain(self.region_refresh_at)
            .chain(self.click_watch_at)
            .min()
    }

    fn on_timers(&mut self, now: Instant) {
        match self.next_deadline() {
            Some(deadline) if now >= deadline => {}
            _ => return,
        }
        struct Due {
            effects: Vec<Effect>,
            hint: bool,
            clip_close: bool,
        }
        let due = self.with_visual(|v| {
            let effects = v.pill.on_timers(now);
            let hint = v
                .hint
                .as_ref()
                .and_then(|h| h.hide_at)
                .is_some_and(|t| now >= t);
            let mut clip_close = false;
            if let Some(clip) = v.clip.as_mut() {
                if clip.autodismiss_at.is_some_and(|t| now >= t) {
                    clip.autodismiss_at = None;
                    clip_close = true;
                }
                if clip.close_at.is_some_and(|t| now >= t) {
                    clip.close_at = None;
                    clip_close = true;
                }
                if clip.copied_reset_at.is_some_and(|t| now >= t) {
                    clip.copied_reset_at = None;
                    clip.copied_id = None;
                }
            }
            Due {
                effects,
                hint,
                clip_close,
            }
        });
        self.run_effects(due.effects, now);
        if due.hint {
            self.dismiss_hint(now);
        }
        if due.clip_close {
            self.dismiss_clip(now);
        }
        if self.click_watch_at.is_some_and(|t| now >= t) {
            self.click_watch_tick(now);
        }
        if self.region_refresh_at.is_some_and(|t| now >= t) {
            self.region_refresh_at = None;
        }
        // Timer-driven model changes (hover exit, status expiry, fade end) move the capsule
        // target, so frames and regions are recomputed once per wake.
        self.after_change(now);
    }

    /// Dismiss the clip when the primary button goes down outside both the card and the pill
    /// (the Windows overlays never take focus, so there is no focus-loss event to use).
    fn click_watch_tick(&mut self, now: Instant) {
        let down = unsafe { GetAsyncKeyState(0x01) } < 0;
        if !self.click_primed {
            self.click_primed = true;
            self.prev_down = down;
            self.click_watch_at = Some(now + Duration::from_millis(50));
            return;
        }
        let fresh = down && !self.prev_down;
        self.prev_down = down;
        if fresh {
            if let Some((cx, cy)) = gfx::cursor_pos() {
                let scale = f64::from(self.placement.scale);
                let inside_clip = self.frames[Surf::Clip as usize].is_some_and(|(x, y, w, h)| {
                    let (px, py) = (f64::from(cx), f64::from(cy));
                    px >= f64::from(x)
                        && px <= f64::from(x) + f64::from(w) * scale
                        && py >= f64::from(y)
                        && py <= f64::from(y) + f64::from(h) * scale
                });
                let inside_pill = self.capsule_phys.is_some_and(|(x, y, w, h)| {
                    let (px, py) = (f64::from(cx), f64::from(cy));
                    px >= x && px <= x + w && py >= y && py <= y + h
                });
                if !inside_clip && !inside_pill {
                    super::actions::log_message(
                        "pill_clipboard_dismiss",
                        "reason=outside_click path=native",
                    );
                    self.dismiss_clip(now);
                    return;
                }
            }
        }
        self.click_watch_at = Some(now + Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------------------------
// App actions (mirror `pill_bridge_macos.rs` callbacks / the commands the React pill invoked)
// ---------------------------------------------------------------------------------------------

fn act_open_hub(route: &'static str) {
    std::thread::spawn(move || {
        let Some(actions) = super::actions::actions() else {
            return;
        };
        let started = Instant::now();
        let outcome = actions.open_hub(route);
        super::actions::log_message(
            "pill_native_action",
            &format!(
                "action=open_hub route={route} phase=result outcome={} elapsed_ms={}",
                if outcome.is_ok() { "ok" } else { "error" },
                started.elapsed().as_millis()
            ),
        );
    });
}

fn act_start() {
    std::thread::spawn(|| {
        if let Some(actions) = super::actions::actions() {
            if let Err(error) = actions.start_dictation() {
                super::actions::log_message("pill_native_start_error", &error);
            }
        }
    });
}

fn act_stop(send_enter: bool) {
    std::thread::spawn(move || {
        if let Some(actions) = super::actions::actions() {
            if let Err(error) = actions.stop_dictation(send_enter) {
                super::actions::log_message("pill_native_stop_error", &error);
            }
        }
    });
}

fn act_cancel() {
    super::actions::log_message(
        "cancel_input_candidate",
        "source=pill_cancel_button phase=clicked",
    );
    std::thread::spawn(|| {
        if let Some(actions) = super::actions::actions() {
            if let Err(error) = actions.cancel_dictation() {
                super::actions::log_message("pill_native_cancel_error", &error);
            }
        }
    });
}

fn act_toggle_clipboard() {
    std::thread::spawn(|| {
        if let Some(actions) = super::actions::actions() {
            if let Err(error) = actions.toggle_clipboard() {
                super::actions::log_message("pill_native_clipboard_toggle_error", &error);
            }
        }
    });
}

fn act_copy(text: String) {
    std::thread::spawn(move || {
        if let Some(actions) = super::actions::actions() {
            if let Err(error) = actions.copy_text(text) {
                super::actions::log_message("pill_native_copy_error", &error);
            }
        }
    });
}

fn act_screenshot_destination(destination: &'static str) {
    std::thread::spawn(move || {
        let patch = serde_json::json!({
            "screenshot_destination": destination,
            "screenshot_prompt_selected": true,
        });
        if let Some(actions) = super::actions::actions() {
            if let Err(error) = actions.apply_settings_patch(patch) {
                super::actions::log_message("pill_native_screenshot_destination_error", &error);
            }
        }
    });
}
