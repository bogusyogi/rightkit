//! The owner thread: one thread per overlay creates the window, runs its message loop and does
//! every Win32 call for it, so the window never needs cross-thread marshalling.
//!
//! Idle invariant (heardright#293): when no animation is running and the pointer is not over the
//! window, the thread is parked in `GetMessageW` and no timer exists. Only two things ever run it
//! without a message from outside:
//!
//! * an active [`Animation`], paced one frame per display refresh by `DwmFlush` (no timer; the
//!   thread polls its queue between frames and returns to blocking when the animation ends);
//! * the hover leave-debounce: after the pointer leaves a hit region, a 40 ms timer delivers the
//!   remaining `HoverGate` ticks and is killed the moment the gate settles.
//!
//! Re-entrancy rule: `UpdateLayeredWindow`, `SetWindowPos`, `SetWindowRgn` and friends can send
//! messages back into the window procedure before they return. No `RefCell` borrow of the owner
//! state is held across such a call, and display-change messages are deferred with a posted
//! message instead of being handled inside whatever call raised them.

pub(crate) mod gdi;

mod backdrop;

use std::cell::RefCell;
use std::mem::size_of;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rightkit_overlay::windows as rk;
use rightkit_overlay::{OverlayRect, OverlayStyle, Rect};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows_sys::Win32::Graphics::Dwm::DwmFlush;
use windows_sys::Win32::Graphics::Gdi::{
    CombineRgn, CreateRectRgn, DeleteObject, SetWindowRgn, ValidateRect, RGN_OR,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows_sys::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    IsWindow, KillTimer, LoadCursorW, PeekMessageW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SetTimer, SetWindowLongPtrW, SetWindowPos, TranslateMessage,
    UnregisterClassW, GWLP_USERDATA, HWND_TOP, IDC_ARROW, MA_NOACTIVATE, MSG, PM_REMOVE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, WM_APP, WM_CANCELMODE,
    WM_CAPTURECHANGED, WM_CLOSE, WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_PAINT, WM_QUIT,
    WM_SETTINGCHANGE, WM_TIMER, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WTS_CONSOLE_CONNECT,
    WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT, WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK,
    WTS_SESSION_UNLOCK,
};

use crate::anim::{Animation, FrameInfo};
use crate::events::{EventQueue, OverlayEvent};
use crate::geometry::{
    client_to_logical, copy_rows, device_regions, hit_test, place_frame, plan_backing,
    scale_for_dpi, Backing, HitRegion, Placement, Size,
};
use crate::hover::{HoverTracker, Sample, TimerAction, HOVER_TICK_MS};
use crate::paint::PaintCtx;
use crate::window::{lock, Command, OverlayConfig, OverlayError, Painter, Shared};

use self::gdi::{present, Surface};

/// Posted by handles to tell the owner thread its command queue is not empty.
pub(crate) const WM_APP_WAKE: u32 = WM_APP + 1;
/// Posted by the owner to itself to re-place the window after a display or DPI change.
const WM_APP_DISPLAY: u32 = WM_APP + 2;
/// `WM_MOUSELEAVE` lives in the Win32_UI_Controls feature; the value is fixed by WinUser.h.
const WM_MOUSELEAVE: u32 = 0x02A3;
/// `SPI_SETWORKAREA`, the `wparam` of the `WM_SETTINGCHANGE` that follows a taskbar move.
const SPI_SETWORKAREA: usize = 0x002F;
const HOVER_TIMER_ID: usize = 1;
/// How long `OverlayWindow::drop` waits for the owner thread before detaching it.
const JOIN_TIMEOUT: Duration = Duration::from_millis(1500);

/// Starts the owner thread and waits until its window exists (or failed to).
pub fn spawn_owner(
    config: OverlayConfig,
    shared: Arc<Shared>,
) -> Result<JoinHandle<()>, OverlayError> {
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), OverlayError>>(1);
    let thread_shared = Arc::clone(&shared);
    let join = std::thread::Builder::new()
        .name(format!("rk-overlay-{}", config.name))
        .spawn(move || run(config, thread_shared, ready_tx))
        .map_err(|error| OverlayError::Thread(error.to_string()))?;
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(join),
        Ok(Err(error)) => {
            let _ = join.join();
            Err(error)
        }
        Err(_) => {
            let _ = join.join();
            Err(OverlayError::Thread(
                "owner thread exited during startup".to_owned(),
            ))
        }
    }
}

/// Waits for a closing owner thread, giving up (detaching it) after [`JOIN_TIMEOUT`].
pub(crate) fn join_bounded(thread: JoinHandle<()>) {
    let deadline = Instant::now() + JOIN_TIMEOUT;
    while !thread.is_finished() {
        if Instant::now() >= deadline {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = thread.join();
}

fn run(
    config: OverlayConfig,
    shared: Arc<Shared>,
    ready: mpsc::SyncSender<Result<(), OverlayError>>,
) {
    // Per-monitor-v2 for this thread, whatever the process is: the window then reports physical
    // pixels and receives WM_DPICHANGED. `SetThreadDpiAwarenessContext` is a static user32 import
    // (Windows 10 1607+), not a run-time lookup, so a build older than that fails to load the
    // binary rather than ignoring the call; the crate's floor is 1703 (see the README).
    // SAFETY: plain thread-scoped setting.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let owner = match Owner::create(config, Arc::clone(&shared)) {
        Ok(owner) => owner,
        Err(error) => {
            let _ = ready.send(Err(error));
            shared.finish();
            return;
        }
    };
    let _ = ready.send(Ok(()));
    // A panic in the loop (a bug here, not in a painter, which render_present already contains)
    // must still end the window cleanly: `finish` below always runs, so subscribers get `Closed`
    // and handles stop posting to a thread that is gone.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: runs on the thread that created the window.
        unsafe { message_loop(&owner) };
        // Events raised by the last messages (a hover ending with the window) still go out.
        owner.flush_events();
    }));
    drop(owner);
    shared.finish();
    drop(outcome);
}

struct ActiveAnimation {
    animation: Animation,
    /// Set when the first frame is painted, so an animation started while hidden begins at show.
    start: Option<Instant>,
}

struct State {
    /// The last frame requested, logical size and physical position.
    frame: Option<Rect>,
    clamp: bool,
    placement: Option<Placement>,
    visible: bool,
    surface: Option<Surface>,
    ctx: Option<PaintCtx>,
    painter: Option<Box<dyn Painter>>,
    animation: Option<ActiveAnimation>,
    /// Where the last animation stopped, for repaints with none running.
    last_t: f32,
    last_elapsed: u64,
    regions: Vec<HitRegion>,
    /// Device rectangles of the window region currently set (click-through mode).
    applied_region: Option<Vec<[i32; 4]>>,
    hover: HoverTracker,
    hover_timer: bool,
    events: EventQueue,
    tracking_leave: bool,
    pressed: Option<u32>,
    /// The session is locked or disconnected: nothing is shown, so animation frames are not
    /// painted until it is back.
    session_locked: bool,
}

struct Owner {
    hwnd: HWND,
    config: OverlayConfig,
    shared: Arc<Shared>,
    state: RefCell<State>,
    /// `WM_WTSSESSION_CHANGE` registration succeeded (so it must be undone).
    session_registered: bool,
    /// Keeps the window class registered; declared last so it is released after the window is
    /// destroyed (fields drop after `Drop::drop` has run).
    _class: ClassRef,
}

fn class_name_for(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("RightKitOverlayWin.{clean}")
}

fn register_class(hinstance: HINSTANCE, wide_name: &[u16]) -> Result<(), OverlayError> {
    // SAFETY: the class struct and its NUL-terminated name outlive the call; the window procedure
    // is a plain `extern "system"` function.
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(overlay_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: wide_name.as_ptr(),
            hIconSm: std::ptr::null_mut(),
        };
        if RegisterClassExW(&class) == 0 {
            let code = GetLastError();
            // A class of this name left over from elsewhere is reused.
            if code != ERROR_CLASS_ALREADY_EXISTS {
                return Err(OverlayError::Win32 {
                    call: "RegisterClassExW",
                    code,
                });
            }
        }
    }
    Ok(())
}

/// Window classes this crate has registered and how many live owner windows use each. The class
/// is registered by the first window and unregistered when the last one is gone, so a process
/// that creates overlays over and over does not accumulate classes.
static CLASSES: Mutex<Vec<(String, usize)>> = Mutex::new(Vec::new());

/// Counts one more window on `name`. True when it is the first, so the class must be registered.
fn class_acquire(table: &mut Vec<(String, usize)>, name: &str) -> bool {
    if let Some(entry) = table.iter_mut().find(|(known, _)| known == name) {
        entry.1 += 1;
        return false;
    }
    table.push((name.to_owned(), 1));
    true
}

/// Counts one window fewer on `name`. True when it was the last, so the class must be
/// unregistered. Releasing a class that is not counted does nothing.
fn class_release(table: &mut Vec<(String, usize)>, name: &str) -> bool {
    let Some(at) = table.iter().position(|(known, _)| known == name) else {
        return false;
    };
    table[at].1 -= 1;
    if table[at].1 == 0 {
        table.remove(at);
        true
    } else {
        false
    }
}

/// One owner window's claim on its window class; releasing the last claim unregisters the class.
struct ClassRef {
    name: String,
    wide: Vec<u16>,
    hinstance: HINSTANCE,
}

impl ClassRef {
    fn acquire(hinstance: HINSTANCE, name: String) -> Result<ClassRef, OverlayError> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // The registry lock is held across registration so a window starting while the last one
        // closes sees a consistent class.
        let mut table = lock(&CLASSES);
        if class_acquire(&mut table, &name) {
            if let Err(error) = register_class(hinstance, &wide) {
                class_release(&mut table, &name);
                return Err(error);
            }
        }
        Ok(ClassRef {
            name,
            wide,
            hinstance,
        })
    }
}

impl Drop for ClassRef {
    fn drop(&mut self) {
        let mut table = lock(&CLASSES);
        if class_release(&mut table, &self.name) {
            // SAFETY: the name is NUL-terminated; every window of this class on this thread has
            // been destroyed (the Owner destroys its window before its fields drop). A failure
            // (another window of the class somewhere) leaves the class registered, which is
            // harmless.
            unsafe {
                UnregisterClassW(self.wide.as_ptr(), self.hinstance);
            }
        }
    }
}

impl Owner {
    fn create(config: OverlayConfig, shared: Arc<Shared>) -> Result<Box<Owner>, OverlayError> {
        // SAFETY: Win32 window creation on the calling thread, which then owns the window.
        unsafe {
            let hinstance = GetModuleHandleW(std::ptr::null());
            // Held from here: every early return below releases the class again.
            let class = ClassRef::acquire(hinstance, class_name_for(config.name))?;

            // The suite creator makes a hidden, topmost, non-activating tool window without
            // WS_EX_LAYERED; the effective style below then adds the layered bit.
            let creation = OverlayStyle {
                topmost: config.topmost,
                takes_focus: !config.noactivate,
                click_through: false,
            };
            let raw = rk::create_overlay_window(
                hinstance as isize,
                &class.name,
                config.name,
                OverlayRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                creation,
            );
            if raw == 0 {
                return Err(OverlayError::Win32 {
                    call: "CreateWindowExW",
                    code: GetLastError(),
                });
            }
            let hwnd = raw as HWND;
            let effective = OverlayStyle {
                topmost: config.topmost,
                takes_focus: !config.noactivate,
                click_through: config.style.click_through,
            };
            rk::apply_style(raw, effective);
            let applied = backdrop::apply(hwnd, config.backdrop);
            shared.backdrop_applied.store(applied, Ordering::Release);
            // Lock and disconnect notifications for this session, to stop painting while nothing
            // is on screen. Best effort: without it animations simply keep running when locked.
            let session_registered =
                WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) != 0;

            let owner = Box::new(Owner {
                hwnd,
                config,
                shared: Arc::clone(&shared),
                state: RefCell::new(State {
                    frame: None,
                    clamp: true,
                    placement: None,
                    visible: false,
                    surface: None,
                    ctx: None,
                    painter: None,
                    animation: None,
                    last_t: 0.0,
                    last_elapsed: 0,
                    regions: Vec::new(),
                    applied_region: None,
                    hover: HoverTracker::default(),
                    hover_timer: false,
                    events: EventQueue::default(),
                    tracking_leave: false,
                    pressed: None,
                    session_locked: false,
                }),
                session_registered,
                _class: class,
            });
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*owner as *const Owner as isize);
            shared.hwnd.store(raw, Ordering::Release);
            Ok(owner)
        }
    }

    // ---- commands ----------------------------------------------------------------------------

    fn drain_commands(&self) {
        // Clear the flag before taking the queue: a command pushed after the take posts again.
        self.shared.wake_pending.store(false, Ordering::Release);
        let commands = std::mem::take(&mut *lock(&self.shared.commands));
        for command in commands {
            self.process(command);
        }
    }

    fn is_visible(&self) -> bool {
        self.state.borrow().visible
    }

    fn process(&self, command: Command) {
        match command {
            Command::SetFrame(frame) => {
                if !frame.finite() {
                    return;
                }
                self.state.borrow_mut().frame = Some(frame);
                if self.apply_frame() {
                    self.refresh_geometry();
                }
            }
            Command::SetClamp(clamp) => {
                self.state.borrow_mut().clamp = clamp;
                if self.apply_frame() {
                    self.refresh_geometry();
                }
            }
            Command::Show => {
                self.state.borrow_mut().visible = true;
                self.render_present();
                self.show_window();
                self.apply_region();
                self.resample();
            }
            Command::Hide => self.hide_window(),
            Command::SetPainter(painter) => {
                self.state.borrow_mut().painter = Some(painter);
                if self.is_visible() {
                    self.render_present();
                }
            }
            Command::Invalidate => {
                if self.is_visible() {
                    self.render_present();
                }
            }
            Command::Animate(animation) => {
                let visible = {
                    let mut state = self.state.borrow_mut();
                    state.animation = Some(ActiveAnimation {
                        animation,
                        start: None,
                    });
                    state.last_t = 0.0;
                    state.last_elapsed = 0;
                    state.visible
                };
                // The message loop paints the frames, starting with this one.
                if visible {
                    self.animation_frame();
                }
            }
            Command::StopAnimation => self.state.borrow_mut().animation = None,
            Command::SetHitRegions(regions) => {
                self.state.borrow_mut().regions = regions;
                self.apply_region();
                self.resample();
            }
            Command::ReassertTopmost => {
                if self.config.topmost && self.is_visible() {
                    rk::reassert_if_needed(self.hwnd as isize);
                }
            }
            Command::Close => {
                // SAFETY: destroys this thread own window; WM_DESTROY then ends the loop.
                unsafe {
                    DestroyWindow(self.hwnd);
                }
            }
        }
    }

    /// Resolves the requested frame against the monitors. True when the placement changed.
    fn apply_frame(&self) -> bool {
        let (frame, clamp) = {
            let state = self.state.borrow();
            match state.frame {
                Some(frame) => (frame, state.clamp),
                None => return false,
            }
        };
        let Some(new) = place_frame(frame, clamp, rk::monitor_at) else {
            return false;
        };
        let mut state = self.state.borrow_mut();
        let old = state.placement.replace(new);
        if let Some(old) = old {
            if old.dpi != new.dpi {
                state
                    .events
                    .push(OverlayEvent::DpiChanged(scale_for_dpi(new.dpi)));
                // A new scale starts a new canvas, which drops the glass and text caches.
                state.ctx = None;
            }
            if old.monitor != new.monitor {
                state.events.push(OverlayEvent::MonitorChanged);
            }
        }
        old != Some(new)
    }

    /// After a placement change: repaint if visible, refit the window shape, re-read the pointer.
    fn refresh_geometry(&self) {
        if self.is_visible() {
            self.render_present();
        }
        self.apply_region();
        self.resample();
    }

    fn on_display_change(&self) {
        if self.apply_frame() {
            self.refresh_geometry();
        }
    }

    fn show_window(&self) {
        if self.config.topmost {
            rk::show_noactivate(self.hwnd as isize);
        } else {
            // SAFETY: shows this window above its siblings without activating or moving it.
            unsafe {
                SetWindowPos(
                    self.hwnd,
                    HWND_TOP,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
    }

    fn hide_window(&self) {
        let action = {
            let mut state = self.state.borrow_mut();
            state.visible = false;
            state.animation = None;
            state.pressed = None;
            // A hidden window gets no WM_MOUSELEAVE reliably; ask for tracking again on the next
            // pointer move after it is shown.
            state.tracking_leave = false;
            let mut out = Vec::new();
            let action = state.hover.reset(&mut out);
            state.events.extend(out);
            // Hidden windows hold no surface or caches.
            state.surface = None;
            state.ctx = None;
            action
        };
        self.apply_timer_action(action);
        // SAFETY: releases a capture this thread may hold.
        unsafe {
            ReleaseCapture();
        }
        rk::hide(self.hwnd as isize);
    }

    // ---- painting ----------------------------------------------------------------------------

    /// Paints one frame with the current painter and publishes it with the window position.
    fn render_present(&self) {
        struct Job {
            placement: Placement,
            ctx: PaintCtx,
            surface: Surface,
            painter: Option<Box<dyn Painter>>,
            frame: FrameInfo,
        }
        let job = {
            let Ok(mut state) = self.state.try_borrow_mut() else {
                return;
            };
            let Some(placement) = state.placement else {
                return;
            };
            let scale = scale_for_dpi(placement.dpi);
            let (w, h) = (
                placement.device.width.max(1) as u32,
                placement.device.height.max(1) as u32,
            );
            // A canvas of another size but the same scale is resized in place: its text and glass
            // caches are keyed in device pixels and stay valid, so animating the window size does
            // not throw them away every frame. A new scale starts a new canvas.
            let ctx = match state.ctx.take() {
                Some(mut ctx) if ctx.scale() == scale => {
                    ctx.resize(w, h);
                    ctx
                }
                _ => PaintCtx::with_scale(w, h, scale),
            };
            // The DIB is grow-only with hysteresis: it is rebuilt only when the window outgrows it
            // or shrinks to under half of it, never on every small size change.
            let current = state.surface.as_ref().map(|s| (s.width, s.height));
            let surface = match plan_backing(current, (w as usize, h as usize)) {
                Backing::Keep => state.surface.take(),
                Backing::Rebuild { width, height } => {
                    // Release the old DIB first so the peak is one surface, not two.
                    state.surface = None;
                    Surface::new(width, height).or_else(|| Surface::new(w as usize, h as usize))
                }
            };
            let Some(surface) = surface else {
                state.ctx = Some(ctx);
                return;
            };
            Job {
                placement,
                ctx,
                surface,
                painter: state.painter.take(),
                frame: FrameInfo {
                    t: state.last_t,
                    elapsed_ms: state.last_elapsed,
                    dpi: placement.dpi,
                    size: Size {
                        width: w as f32 / scale,
                        height: h as f32 / scale,
                    },
                },
            }
        };
        let Job {
            placement,
            mut ctx,
            mut surface,
            mut painter,
            frame,
        } = job;

        // No state borrow is held while user code or Win32 runs.
        ctx.clear();
        if let Some(active) = painter.as_mut() {
            let outcome = catch_unwind(AssertUnwindSafe(|| active.paint(&mut ctx, frame)));
            if outcome.is_err() {
                // A painter that panicked is not called again; the window stays up, blank.
                painter = None;
            }
        }
        // The DIB may be larger than the window: copy row by row and publish only the window part.
        let (window_w, window_h) = ctx.pixel_size();
        let (window_w, window_h) = (window_w as usize, window_h as usize);
        let stride = surface.width;
        if copy_rows(
            surface.pixels_mut(),
            stride,
            ctx.pixels(),
            window_w,
            window_h,
        ) {
            present(
                self.hwnd,
                &surface,
                placement.device.x,
                placement.device.y,
                window_w,
                window_h,
            );
        }

        if let Ok(mut state) = self.state.try_borrow_mut() {
            state.ctx = Some(ctx);
            state.surface = Some(surface);
            if state.painter.is_none() {
                state.painter = painter;
            }
        }
    }

    /// An animation is running and there is somewhere to paint it (visible, with a frame).
    fn is_animating(&self) -> bool {
        let state = self.state.borrow();
        should_animate(
            state.visible,
            state.placement.is_some(),
            state.animation.is_some(),
            state.session_locked,
        )
    }

    /// Advances the active animation by the wall clock and paints that frame.
    fn animation_frame(&self) {
        {
            let mut guard = self.state.borrow_mut();
            let state = &mut *guard;
            let Some(active) = state.animation.as_mut() else {
                return;
            };
            let start = *active.start.get_or_insert_with(Instant::now);
            let elapsed = start.elapsed().as_millis() as u64;
            let (t, finished) = active.animation.sample(elapsed);
            if finished {
                state.animation = None;
            }
            state.last_t = t;
            state.last_elapsed = elapsed;
        }
        self.render_present();
    }

    /// Clips the window to the hit regions (click-through mode only).
    fn apply_region(&self) {
        if !self.config.click_through {
            return;
        }
        let rects = {
            let state = self.state.borrow();
            let Some(placement) = state.placement else {
                return;
            };
            let rects = device_regions(&state.regions, f64::from(scale_for_dpi(placement.dpi)));
            if state.applied_region.as_ref() == Some(&rects) {
                return;
            }
            rects
        };
        // SAFETY: GDI regions are created, combined and either handed to the window (which owns
        // them after a successful SetWindowRgn) or deleted here.
        let applied = unsafe {
            let region = CreateRectRgn(0, 0, 0, 0);
            if region.is_null() {
                return;
            }
            for r in &rects {
                let part = CreateRectRgn(r[0], r[1], r[2], r[3]);
                if part.is_null() {
                    continue;
                }
                CombineRgn(region, region, part, RGN_OR);
                DeleteObject(part);
            }
            if SetWindowRgn(self.hwnd, region, 0) == 0 {
                DeleteObject(region);
                false
            } else {
                true
            }
        };
        if applied {
            self.state.borrow_mut().applied_region = Some(rects);
        }
    }

    // ---- pointer -----------------------------------------------------------------------------

    /// The pointer position in window client device pixels, as a hover sample.
    fn sample_client(&self, x: i32, y: i32) -> Sample {
        let state = self.state.borrow();
        let scale = state
            .placement
            .map_or(1.0, |placement| scale_for_dpi(placement.dpi));
        let (lx, ly) = (client_to_logical(x, scale), client_to_logical(y, scale));
        Sample {
            region: hit_test(&state.regions, lx, ly),
            x: lx as f32,
            y: ly as f32,
        }
    }

    /// A fresh look at the pointer in screen space, or `Sample::outside` when the window is
    /// hidden or the pointer is not over it.
    fn sample_cursor(&self) -> Sample {
        let placement = {
            let state = self.state.borrow();
            match (state.visible, state.placement) {
                (true, Some(placement)) => placement,
                _ => return Sample::outside(),
            }
        };
        let mut point = POINT { x: 0, y: 0 };
        // SAFETY: GetCursorPos writes one POINT.
        if unsafe { GetCursorPos(&mut point) } == 0 {
            return Sample::outside();
        }
        let (x, y) = (point.x - placement.device.x, point.y - placement.device.y);
        if x < 0 || y < 0 || x >= placement.device.width || y >= placement.device.height {
            return Sample::outside();
        }
        self.sample_client(x, y)
    }

    /// Re-reads the pointer after the window, its regions or its visibility changed under it.
    fn resample(&self) {
        let sample = self.sample_cursor();
        self.on_pointer(sample);
    }

    fn on_pointer(&self, sample: Sample) {
        let mut out = Vec::new();
        let action = {
            let Ok(mut state) = self.state.try_borrow_mut() else {
                return;
            };
            let action = state.hover.on_pointer(sample, &mut out);
            state.events.extend(out);
            action
        };
        self.apply_timer_action(action);
    }

    fn apply_timer_action(&self, action: TimerAction) {
        match action {
            TimerAction::None => {}
            TimerAction::Arm => {
                let arm = {
                    let mut state = self.state.borrow_mut();
                    !std::mem::replace(&mut state.hover_timer, true)
                };
                if arm {
                    // SAFETY: a thread timer on this window, killed when the gate settles.
                    unsafe {
                        SetTimer(self.hwnd, HOVER_TIMER_ID, HOVER_TICK_MS, None);
                    }
                }
            }
            TimerAction::Kill => {
                let kill = std::mem::replace(&mut self.state.borrow_mut().hover_timer, false);
                if kill {
                    // SAFETY: kills the timer armed above.
                    unsafe {
                        KillTimer(self.hwnd, HOVER_TIMER_ID);
                    }
                }
            }
        }
    }

    fn on_hover_tick(&self) {
        let sample = self.sample_cursor();
        let mut out = Vec::new();
        let action = {
            let Ok(mut state) = self.state.try_borrow_mut() else {
                return;
            };
            let action = state.hover.on_tick(sample, &mut out);
            state.events.extend(out);
            action
        };
        self.apply_timer_action(action);
    }

    fn on_mouse_move(&self, lparam: LPARAM) {
        let (x, y) = lparam_point(lparam);
        let need_tracking = !self.state.borrow().tracking_leave;
        if need_tracking {
            let mut track = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: self.hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: `track` is a valid TRACKMOUSEEVENT for the call.
            if unsafe { TrackMouseEvent(&mut track) } != 0 {
                self.state.borrow_mut().tracking_leave = true;
            }
        }
        let sample = self.sample_client(x, y);
        self.on_pointer(sample);
    }

    fn on_mouse_leave(&self) {
        self.state.borrow_mut().tracking_leave = false;
        self.on_pointer(Sample::outside());
    }

    fn on_button_down(&self, lparam: LPARAM) {
        let (x, y) = lparam_point(lparam);
        let sample = self.sample_client(x, y);
        self.state.borrow_mut().pressed = sample.region;
        if sample.region.is_some() {
            // SAFETY: captures the pointer for this window until the button is released.
            unsafe {
                SetCapture(self.hwnd);
            }
        }
    }

    fn on_button_up(&self, lparam: LPARAM) {
        let pressed = self.state.borrow_mut().pressed.take();
        let Some(pressed) = pressed else {
            return;
        };
        // SAFETY: releases the capture taken on button down.
        unsafe {
            ReleaseCapture();
        }
        let (x, y) = lparam_point(lparam);
        // A click is a press and a release on the same region.
        if self.sample_client(x, y).region == Some(pressed) {
            self.state
                .borrow_mut()
                .events
                .push(OverlayEvent::Click(pressed));
        }
    }

    fn flush_events(&self) {
        let events = match self.state.try_borrow_mut() {
            Ok(mut state) => state.events.drain(),
            Err(_) => return,
        };
        if !events.is_empty() {
            self.shared.emit(events);
        }
    }

    // ---- window procedure --------------------------------------------------------------------

    fn handle_message(&self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_APP_WAKE => {
                self.drain_commands();
                0
            }
            WM_APP_DISPLAY => {
                self.on_display_change();
                0
            }
            WM_MOUSEACTIVATE if self.config.noactivate => MA_NOACTIVATE as LRESULT,
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                // The contents come from UpdateLayeredWindow; there is nothing to paint here.
                // SAFETY: validates the whole client area of this window.
                unsafe {
                    ValidateRect(hwnd, std::ptr::null());
                }
                0
            }
            WM_MOUSEMOVE => {
                self.on_mouse_move(lparam);
                0
            }
            WM_MOUSELEAVE => {
                self.on_mouse_leave();
                0
            }
            WM_LBUTTONDOWN => {
                self.on_button_down(lparam);
                0
            }
            WM_LBUTTONUP => {
                self.on_button_up(lparam);
                0
            }
            WM_CAPTURECHANGED | WM_CANCELMODE => {
                if let Ok(mut state) = self.state.try_borrow_mut() {
                    state.pressed = None;
                }
                0
            }
            WM_TIMER if wparam == HOVER_TIMER_ID => {
                self.on_hover_tick();
                0
            }
            WM_DPICHANGED | WM_DISPLAYCHANGE => {
                // Re-placing repaints, so it is deferred out of whatever call raised this.
                // SAFETY: posts to this window.
                unsafe {
                    PostMessageW(hwnd, WM_APP_DISPLAY, 0, 0);
                }
                0
            }
            WM_SETTINGCHANGE => {
                if wparam == SPI_SETWORKAREA {
                    // SAFETY: posts to this window.
                    unsafe {
                        PostMessageW(hwnd, WM_APP_DISPLAY, 0, 0);
                    }
                }
                // SAFETY: default handling of the same message.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_CLOSE => {
                // SAFETY: destroys this window; WM_DESTROY follows.
                unsafe {
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_WTSSESSION_CHANGE => {
                let (was, now) = {
                    let mut state = self.state.borrow_mut();
                    let was = state.session_locked;
                    state.session_locked = session_locked_after(wparam as u32, was);
                    (was, state.session_locked)
                };
                if was && !now && self.is_visible() {
                    // Back from the lock screen: put the current frame up. A running animation
                    // resumes by itself, because the message loop re-checks `is_animating`.
                    self.render_present();
                }
                0
            }
            WM_DESTROY => {
                // The handle is dead from here on. Stop publishing it: a recycled handle value
                // must never receive this overlay's wake messages while the thread winds down.
                self.shared.hwnd.store(0, Ordering::Release);
                // SAFETY: undoes the session registration (a no-op if it never succeeded) and
                // ends this thread message loop.
                unsafe {
                    if self.session_registered {
                        WTSUnRegisterSessionNotification(hwnd);
                    }
                    PostQuitMessage(0);
                }
                0
            }
            // SAFETY: default handling of a message this window does not take.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        // SAFETY: runs on the owner thread. The state pointer is cleared first so a message sent
        // while the window is destroyed cannot reach this half-dropped object.
        unsafe {
            KillTimer(self.hwnd, HOVER_TIMER_ID);
            if IsWindow(self.hwnd) != 0 {
                if self.session_registered {
                    WTSUnRegisterSessionNotification(self.hwnd);
                }
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
        }
        // Covers a loop that ended without WM_DESTROY (a failed GetMessage).
        self.shared.hwnd.store(0, Ordering::Release);
    }
}

unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the pointer was stored by Owner::create and is cleared before the Owner is freed;
    // it refers to a live Owner for as long as it is non-null, and Owner is only used through
    // shared references (its mutable state is behind a RefCell).
    let owner = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Owner;
    if owner.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    let owner = unsafe { &*owner };
    match catch_unwind(AssertUnwindSafe(|| {
        owner.handle_message(hwnd, msg, wparam, lparam)
    })) {
        Ok(result) => result,
        Err(_) => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam & 0xFFFF) as u16 as i16 as i32,
        ((lparam >> 16) & 0xFFFF) as u16 as i16 as i32,
    )
}

/// The message loop. Blocks in `GetMessageW` unless an animation is running.
///
/// # Safety
/// Must run on the thread that created the owner window.
unsafe fn message_loop(owner: &Owner) {
    let mut msg: MSG = unsafe { std::mem::zeroed() };
    loop {
        if owner.is_animating() {
            // Drain what is queued without blocking, then paint a frame and wait for vsync.
            while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                if msg.message == WM_QUIT {
                    return;
                }
                unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            owner.flush_events();
            if owner.is_animating() {
                owner.animation_frame();
                pace_to_vsync();
            }
        } else {
            let got = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
            if got <= 0 {
                // 0 is WM_QUIT; -1 is an error. Either way the window is finished.
                return;
            }
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // Whatever else is already queued, so events from a burst leave as one batch.
            while unsafe { PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
                if msg.message == WM_QUIT {
                    return;
                }
                unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            owner.flush_events();
        }
    }
}

/// Whether the message loop should be painting animation frames: there is a visible window with a
/// placement and an animation, and the session is not locked or disconnected (nothing is on screen
/// then, so painting at refresh rate would only burn CPU and battery).
fn should_animate(visible: bool, placed: bool, animation: bool, session_locked: bool) -> bool {
    visible && placed && animation && !session_locked
}

/// The locked state after a `WM_WTSSESSION_CHANGE` with `event` in its `wparam`. Lock and the two
/// disconnects pause painting; unlock and the two connects resume it; any other event (logon,
/// logoff, remote control, ...) leaves the state as it was.
fn session_locked_after(event: u32, was_locked: bool) -> bool {
    match event {
        WTS_SESSION_LOCK | WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => true,
        WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => false,
        _ => was_locked,
    }
}

/// Blocks until the next display composition. If DWM returns at once or fails (composition off,
/// session locked), waits a fixed 8 ms instead so the loop cannot spin.
fn pace_to_vsync() {
    let before = Instant::now();
    // SAFETY: DwmFlush takes no arguments.
    let hr = unsafe { DwmFlush() };
    if hr < 0 || before.elapsed() < Duration::from_millis(1) {
        std::thread::sleep(Duration::from_millis(8));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_names_are_clean() {
        assert_eq!(class_name_for("pill"), "RightKitOverlayWin.pill");
        assert_eq!(class_name_for("my pill/2"), "RightKitOverlayWin.my_pill_2");
    }

    #[test]
    fn the_class_is_registered_by_the_first_window_and_released_by_the_last() {
        let mut table = Vec::new();
        assert!(class_acquire(&mut table, "a"), "the first window registers");
        assert!(!class_acquire(&mut table, "a"), "the second reuses it");
        assert!(
            class_acquire(&mut table, "b"),
            "another name is another class"
        );
        assert!(!class_release(&mut table, "a"), "one window is still open");
        assert!(class_release(&mut table, "a"), "the last one unregisters");
        assert!(class_release(&mut table, "b"));
        assert!(table.is_empty());
        // An unknown class is not a release, and a closed class registers again.
        assert!(!class_release(&mut table, "a"));
        assert!(class_acquire(&mut table, "a"));
    }

    #[test]
    fn animation_pauses_while_the_session_is_locked() {
        assert!(should_animate(true, true, true, false));
        assert!(!should_animate(true, true, true, true), "locked: no frames");
        assert!(!should_animate(false, true, true, false), "hidden");
        assert!(!should_animate(true, false, true, false), "not placed yet");
        assert!(
            !should_animate(true, true, false, false),
            "nothing to animate"
        );
    }

    #[test]
    fn session_events_lock_and_unlock() {
        assert!(session_locked_after(WTS_SESSION_LOCK, false));
        assert!(!session_locked_after(WTS_SESSION_UNLOCK, true));
        assert!(session_locked_after(WTS_CONSOLE_DISCONNECT, false));
        assert!(session_locked_after(WTS_REMOTE_DISCONNECT, false));
        assert!(!session_locked_after(WTS_CONSOLE_CONNECT, true));
        assert!(!session_locked_after(WTS_REMOTE_CONNECT, true));
        // Logon, logoff, remote control and unknown events change nothing.
        for event in [5, 6, 9, 10, 11, 0, 99] {
            assert!(
                session_locked_after(event, true),
                "event {event} while locked"
            );
            assert!(
                !session_locked_after(event, false),
                "event {event} while unlocked"
            );
        }
    }

    #[test]
    fn mouse_coordinates_decode_as_signed_16_bit() {
        assert_eq!(lparam_point((20 << 16) | 10), (10, 20));
        // Negative client coordinates (a captured pointer left of or above the window).
        let lparam = (((-5i16) as u16 as isize) << 16) | ((-7i16) as u16 as isize);
        assert_eq!(lparam_point(lparam), (-7, -5));
    }
}
