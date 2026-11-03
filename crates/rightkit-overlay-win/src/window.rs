//! The public overlay types: configuration, the owning [`OverlayWindow`], the cloneable
//! [`OverlayHandle`] and the [`Painter`] trait.
//!
//! These types hold no Win32 state themselves. A handle posts commands into a queue shared with
//! the owner thread and wakes that thread with one posted message; the owner (see `win`) applies
//! them in order on the thread that owns the window. On other platforms `OverlayWindow::create`
//! reports [`OverlayError::Unsupported`].

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use rightkit_overlay::{OverlayStyle, Rect};

use crate::anim::{Animation, FrameInfo};
use crate::color::Rgba;
use crate::events::OverlayEvent;
use crate::geometry::HitRegion;
use crate::paint::PaintCtx;

/// Draws one frame of an overlay. Called on the overlay owner thread, never concurrently, once per
/// repaint: after `invalidate`, a frame or hit-region change, `show`, and once per display
/// refresh while an [`Animation`] runs. `ctx` arrives cleared (fully transparent); paint the whole
/// frame every time. A painter that panics is dropped and the window keeps running, blank.
///
/// A closure `FnMut(&mut PaintCtx, FrameInfo) + Send` is a painter too.
pub trait Painter: Send {
    fn paint(&mut self, ctx: &mut PaintCtx, frame: FrameInfo);
}

impl<F> Painter for F
where
    F: FnMut(&mut PaintCtx, FrameInfo) + Send,
{
    fn paint(&mut self, ctx: &mut PaintCtx, frame: FrameInfo) {
        self(ctx, frame);
    }
}

/// A DWM backdrop behind the window. Best effort: see `OverlayHandle::backdrop_applied`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    /// Nothing behind the painted pixels.
    None,
    /// DWM blur-behind.
    Blur,
    /// Acrylic with the given tint (alpha is the tint strength; use at least 1).
    Acrylic { tint: Rgba },
}

/// How to build an overlay window.
///
/// `style` is the suite `OverlayStyle` template; `topmost` and `noactivate` are authoritative for
/// those two behaviours (`OverlayConfig::new` copies them from the style, so a preset such as
/// `OverlayStyle::PASSIVE` works as is). `style.click_through` keeps its suite meaning: the whole
/// window is invisible to the pointer (a pure drawing surface; no hover events). The separate
/// `click_through` flag below is different: it makes the window respond to the pointer **only
/// inside its hit regions**, see [`OverlayHandle::set_hit_regions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverlayConfig {
    /// Names the window class and the owner thread; keep it short and unique per overlay.
    pub name: &'static str,
    pub style: OverlayStyle,
    /// Keep the window in the topmost band.
    pub topmost: bool,
    /// Never activate: showing or clicking the window leaves the user app in front.
    pub noactivate: bool,
    /// Window shape and input follow the union of the hit regions (see `set_hit_regions`).
    pub click_through: bool,
    pub backdrop: Backdrop,
}

impl OverlayConfig {
    /// A config with `topmost` and `noactivate` taken from `style`, no region click-through and no
    /// backdrop.
    pub const fn new(name: &'static str, style: OverlayStyle) -> Self {
        Self {
            name,
            style,
            topmost: style.topmost,
            noactivate: !style.takes_focus,
            click_through: false,
            backdrop: Backdrop::None,
        }
    }

    /// A topmost, never-activating panel that is seen and clicked (the notch and pill kind).
    pub const fn passive(name: &'static str) -> Self {
        Self::new(name, OverlayStyle::PASSIVE)
    }
}

/// Why an overlay could not be created.
#[derive(Debug)]
pub enum OverlayError {
    /// This platform has no overlay backend (the crate is Windows-only at run time).
    Unsupported,
    /// The configuration cannot be honoured.
    InvalidConfig(&'static str),
    /// A Win32 call failed.
    Win32 { call: &'static str, code: u32 },
    /// The owner thread could not be started or died while starting.
    Thread(String),
}

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OverlayError::Unsupported => {
                f.write_str("overlay windows are only available on Windows")
            }
            OverlayError::InvalidConfig(why) => write!(f, "invalid overlay configuration: {why}"),
            OverlayError::Win32 { call, code } => write!(f, "{call} failed (Win32 error {code})"),
            OverlayError::Thread(why) => write!(f, "overlay thread: {why}"),
        }
    }
}

impl std::error::Error for OverlayError {}

/// How many events one [`OverlayHandle::events`] receiver can have waiting. A consumer that falls
/// further behind loses the newest events (the window never blocks on a slow reader and its memory
/// never grows with one). Events are coalesced before they are sent, so a reader that keeps up
/// stays far below this; see `OverlayHandle::events` for what a lagging reader sees.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;

/// A request to the owner thread.
pub(crate) enum Command {
    SetFrame(Rect),
    SetClamp(bool),
    Show,
    Hide,
    SetPainter(Box<dyn Painter>),
    Invalidate,
    Animate(Animation),
    StopAnimation,
    SetHitRegions(Vec<HitRegion>),
    ReassertTopmost,
    /// A caller-drawn frame: `width * height` premultiplied BGRA pixels in device pixels.
    Present {
        width: u32,
        height: u32,
        pixels: Vec<u32>,
    },
    SetFocusable(bool),
    Close,
}

/// State shared between the handles and the owner thread.
pub(crate) struct Shared {
    /// The window handle, 0 before it exists and after it is gone.
    pub(crate) hwnd: AtomicIsize,
    pub(crate) visible: AtomicBool,
    pub(crate) closed: AtomicBool,
    pub(crate) backdrop_applied: AtomicBool,
    /// A wake message is already on its way to the owner thread.
    pub(crate) wake_pending: AtomicBool,
    pub(crate) commands: Mutex<Vec<Command>>,
    pub(crate) subscribers: Mutex<Vec<mpsc::SyncSender<OverlayEvent>>>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            hwnd: AtomicIsize::new(0),
            visible: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            backdrop_applied: AtomicBool::new(false),
            wake_pending: AtomicBool::new(false),
            commands: Mutex::new(Vec::new()),
            subscribers: Mutex::new(Vec::new()),
        })
    }

    /// Sends events to every subscriber, dropping the ones whose receiver is gone. A subscriber
    /// whose channel is full (a reader that has stopped reading) misses the event instead of
    /// blocking the owner thread or growing its queue without bound.
    pub(crate) fn emit(&self, events: Vec<OverlayEvent>) {
        let mut subscribers = lock(&self.subscribers);
        if subscribers.is_empty() {
            return;
        }
        for event in events {
            subscribers.retain(|tx| match tx.try_send(event.clone()) {
                Ok(()) | Err(mpsc::TrySendError::Full(_)) => true,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            });
        }
    }

    /// The window is gone: mark it, tell the subscribers once and disconnect them.
    pub(crate) fn finish(&self) {
        let subscribers = {
            let mut subscribers = lock(&self.subscribers);
            if self.closed.swap(true, Ordering::AcqRel) {
                return;
            }
            std::mem::take(&mut *subscribers)
        };
        self.hwnd.store(0, Ordering::Release);
        self.visible.store(false, Ordering::Release);
        // `try_send`: the channels are bounded, so a reader that stopped reading must not block
        // the owner thread here; it misses `Closed` but still disconnects below.
        for tx in subscribers {
            let _ = tx.try_send(OverlayEvent::Closed);
        }
        // Take the queue out and drop it after the lock is released: dropping a queued painter
        // runs user code, which must never run under the queue mutex.
        let leftover = std::mem::take(&mut *lock(&self.commands));
        drop(leftover);
    }
}

enum Merge {
    Replace,
    Drop,
    Push,
}

/// Queues `command`, merging it with the one before it when they are the same kind (the later
/// one wins) so a burst of identical requests costs one. Returns the command that was replaced or
/// dropped, if any: the caller drops it after releasing the queue lock, because a command can own
/// a painter whose `Drop` is user code.
#[must_use = "drop the displaced command after releasing the queue lock"]
fn coalesce(queue: &mut Vec<Command>, command: Command) -> Option<Command> {
    if let Some(last) = queue.last_mut() {
        use Command::*;
        let merge = match (&*last, &command) {
            (SetFrame(_), SetFrame(_))
            | (SetClamp(_), SetClamp(_))
            | (SetHitRegions(_), SetHitRegions(_))
            | (SetPainter(_), SetPainter(_))
            | (SetFocusable(_), SetFocusable(_))
            | (Present { .. }, Present { .. })
            | (Animate(_), Animate(_)) => Merge::Replace,
            (Invalidate, Invalidate)
            | (Show, Show)
            | (Hide, Hide)
            | (StopAnimation, StopAnimation)
            | (ReassertTopmost, ReassertTopmost)
            | (Close, _) => Merge::Drop,
            _ => Merge::Push,
        };
        match merge {
            Merge::Replace => return Some(std::mem::replace(last, command)),
            Merge::Drop => return Some(command),
            Merge::Push => {}
        }
    }
    queue.push(command);
    None
}

#[cfg(windows)]
fn wake_owner(hwnd: isize) -> bool {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;
    // SAFETY: PostMessageW accepts any handle value; a stale handle just fails.
    hwnd != 0 && unsafe { PostMessageW(hwnd as HWND, crate::win::WM_APP_WAKE, 0, 0) != 0 }
}

#[cfg(not(windows))]
fn wake_owner(_hwnd: isize) -> bool {
    false
}

/// A cloneable, thread-safe reference to an overlay window. Every method returns at once: the work
/// happens on the owner thread, in call order. After the window is closed the methods do nothing.
#[derive(Clone)]
pub struct OverlayHandle {
    shared: Arc<Shared>,
}

impl fmt::Debug for OverlayHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OverlayHandle")
            .field("hwnd", &self.hwnd())
            .field("visible", &self.is_visible())
            .finish()
    }
}

impl OverlayHandle {
    fn post(&self, command: Command) {
        if self.shared.closed.load(Ordering::Acquire) {
            return;
        }
        let displaced = coalesce(&mut lock(&self.shared.commands), command);
        // A replaced painter is dropped here, with no lock held.
        drop(displaced);
        if !self.shared.wake_pending.swap(true, Ordering::AcqRel) {
            let hwnd = self.shared.hwnd.load(Ordering::Acquire);
            if !wake_owner(hwnd) {
                // The wake did not go out; let the next call try again.
                self.shared.wake_pending.store(false, Ordering::Release);
            }
        }
    }

    /// Places and sizes the window.
    ///
    /// # Rect convention
    ///
    /// The frame is a `rightkit_overlay::Rect`, and its fields mix two units:
    ///
    /// * `x` and `y` are the window top-left in **physical virtual-screen pixels**, the space of
    ///   `GetCursorPos` and `rightkit_overlay::MonitorPlacement` (negative on monitors left of or
    ///   above the primary one);
    /// * `width` and `height` are **logical pixels**, multiplied by the DPI scale of the monitor
    ///   the frame lands on (the one under its centre, so 120 x 36 is 180 x 54 device pixels at
    ///   150%).
    ///
    /// Everything inside the window (painting, hit regions, event coordinates) is logical pixels
    /// relative to the window top-left. `Rect::contains` and [`crate::hit_test`] are
    /// edge-inclusive.
    ///
    /// The frame is then held inside that monitor work area by
    /// `rightkit_overlay::WorkArea::clamp` unless [`OverlayHandle::set_work_area_clamp`] turned
    /// that off. A non-finite or negative frame is ignored. When the window is visible this
    /// repaints. Changing only the size is cheap: the surface behind the window is grow-only, so
    /// a morph that resizes it every frame reallocates a handful of times, not once per frame.
    pub fn set_frame(&self, frame: Rect) {
        self.post(Command::SetFrame(frame));
    }

    /// Turns the work-area clamp applied by `set_frame` on or off (on by default).
    pub fn set_work_area_clamp(&self, clamp: bool) {
        self.post(Command::SetClamp(clamp));
    }

    /// Shows the window in the topmost band (or just above its siblings when the config is not
    /// topmost) without activating it, painting the current frame first so nothing flashes empty.
    pub fn show(&self) {
        self.shared.visible.store(true, Ordering::Release);
        self.post(Command::Show);
    }

    /// Hides the window and ends any hover. Cached surfaces are released while hidden.
    pub fn hide(&self) {
        self.shared.visible.store(false, Ordering::Release);
        self.post(Command::Hide);
    }

    /// Whether the window has been asked to be visible (the effect of the latest `show` or
    /// `hide`, available immediately rather than after the owner thread has run it).
    pub fn is_visible(&self) -> bool {
        self.shared.visible.load(Ordering::Acquire) && !self.shared.closed.load(Ordering::Acquire)
    }

    /// Installs the painter and repaints if visible. Replaces the previous painter.
    pub fn set_painter(&self, painter: Box<dyn Painter>) {
        self.post(Command::SetPainter(painter));
    }

    /// Asks for one repaint on the owner thread. Repeated calls before it runs collapse to one.
    pub fn invalidate(&self) {
        self.post(Command::Invalidate);
    }

    /// Starts (or restarts) an animation. While one is active and the window is visible the owner
    /// thread paints one frame per display refresh, paced by `DwmFlush`; a one-shot animation
    /// ends by itself after painting `t == 1.0` once. When no animation is active the thread
    /// holds no timer and blocks waiting for messages.
    pub fn animate(&self, animation: Animation) {
        self.post(Command::Animate(animation));
    }

    /// Stops the active animation, leaving the last painted frame on screen.
    pub fn stop_animation(&self) {
        self.post(Command::StopAnimation);
    }

    /// Replaces the interactive regions (logical pixels, window-relative). Hover and click events
    /// carry the id of the region under the pointer; later regions are on top of earlier ones.
    ///
    /// With `OverlayConfig::click_through` the window itself takes the shape of the union of the
    /// regions, so the pointer reaches whatever is underneath everywhere else, with no polling.
    /// Pixels painted outside every region are then not shown, and an empty list makes the whole
    /// window click-through and invisible. Without `click_through` the window is hit-tested by
    /// its own alpha (fully transparent pixels already let the pointer through) and regions only
    /// name areas for events.
    pub fn set_hit_regions(&self, regions: Vec<HitRegion>) {
        self.post(Command::SetHitRegions(regions));
    }

    /// A new event stream. Each call returns an independent receiver that sees events from that
    /// point on, so several consumers can listen. `OverlayEvent::Closed` is always last and the
    /// receiver disconnects after it.
    ///
    /// Each receiver buffers at most [`EVENT_CHANNEL_CAPACITY`] events. A reader that stops reading
    /// misses the events that arrive while its buffer is full (the window never blocks on it),
    /// which can include `Closed`; the receiver still disconnects when the window ends, so treat
    /// `RecvError` / `Disconnected` as closed too.
    pub fn events(&self) -> mpsc::Receiver<OverlayEvent> {
        let (tx, rx) = mpsc::sync_channel(EVENT_CHANNEL_CAPACITY);
        // Hold the subscriber lock across the closed check and the push: `finish` marks closed
        // under the same lock, so exactly one `Closed` is delivered, never zero or two.
        let mut subscribers = lock(&self.shared.subscribers);
        if self.shared.closed.load(Ordering::Acquire) {
            let _ = tx.try_send(OverlayEvent::Closed);
        } else {
            subscribers.push(tx);
        }
        rx
    }

    /// Repairs the topmost band: if the window lost it (another topmost window took the top), puts
    /// it back without moving, resizing or activating it. A no-op when it is already topmost, and
    /// when the config is not topmost.
    pub fn reassert_topmost(&self) {
        self.post(Command::ReassertTopmost);
    }

    /// Publishes a frame the caller drew, at the window's current position, with
    /// `UpdateLayeredWindow`. The painter does not run for it, and the next repaint (an
    /// `invalidate`, a frame change or an animation frame) replaces it.
    ///
    /// The pixels are copied at call time. They must be exactly the window's current device size
    /// (its logical size times the scale of the monitor it is on), so a canvas made for that size
    /// with `PaintCtx::from_pixels` or `PaintCtx::offscreen` works. A different size is not
    /// presented, because `UpdateLayeredWindow` would resize the window under the placement.
    /// Nothing is presented while the window is hidden.
    pub fn present(&self, ctx: &PaintCtx) {
        if self.shared.closed.load(Ordering::Acquire) {
            return;
        }
        let (width, height) = ctx.pixel_size();
        self.post(Command::Present {
            width,
            height,
            pixels: ctx.pixels().to_vec(),
        });
    }

    /// Turns edit mode on or off. On: the window stops being no-activate, is activated and takes
    /// keyboard focus (when visible), and then reports [`OverlayEvent::Char`],
    /// [`OverlayEvent::Key`] and [`OverlayEvent::Focus`]. Off (the default): the window is
    /// no-activate again, as configured, and reports none of them.
    ///
    /// Activation is best effort: Windows may refuse foreground activation to a process the user
    /// did not last interact with, in which case the window gets `Focus` only once it is
    /// activated by the user. Leaving edit mode does not move focus away from the window; it is
    /// lost when another window is activated, which reports `Focus(false)`.
    pub fn set_focusable(&self, focusable: bool) {
        self.post(Command::SetFocusable(focusable));
    }

    /// Closes the window. The owner thread ends after sending `OverlayEvent::Closed`.
    pub fn close(&self) {
        self.post(Command::Close);
    }

    /// The raw `HWND` value (0 once closed), for host code that calls other
    /// `rightkit_overlay::windows` helpers on it.
    pub fn hwnd(&self) -> isize {
        self.shared.hwnd.load(Ordering::Acquire)
    }

    /// Whether the OS accepted the configured DWM backdrop. `false` for `Backdrop::None`. When it
    /// is false for a backdrop the host asked for, fall back to `PaintCtx::glass_layer`.
    pub fn backdrop_applied(&self) -> bool {
        self.shared.backdrop_applied.load(Ordering::Acquire)
    }
}

/// Owns an overlay window and its owner thread. Dropping it closes the window and joins the
/// thread (bounded: a wedged thread is detached after 1.5 s).
pub struct OverlayWindow {
    handle: OverlayHandle,
    thread: Option<JoinHandle<()>>,
}

impl fmt::Debug for OverlayWindow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OverlayWindow")
            .field("handle", &self.handle)
            .finish()
    }
}

impl OverlayWindow {
    /// Creates the window (hidden, no content) and its owner thread, and returns once the window
    /// exists. Configure it through [`OverlayWindow::handle`]: `set_painter`, `set_frame`,
    /// `set_hit_regions`, then `show`.
    pub fn create(config: OverlayConfig) -> Result<OverlayWindow, OverlayError> {
        if config.name.is_empty() {
            return Err(OverlayError::InvalidConfig("name is empty"));
        }
        if config.name.len() > 96 || config.name.contains('\0') {
            return Err(OverlayError::InvalidConfig(
                "name must be at most 96 bytes with no NUL",
            ));
        }
        #[cfg(windows)]
        {
            let shared = Shared::new();
            let thread = crate::win::spawn_owner(config, Arc::clone(&shared))?;
            Ok(OverlayWindow {
                handle: OverlayHandle { shared },
                thread: Some(thread),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = config;
            Err(OverlayError::Unsupported)
        }
    }

    /// A handle to drive the window from any thread.
    pub fn handle(&self) -> OverlayHandle {
        self.handle.clone()
    }

    /// Closes the window and waits for its thread. Same as dropping it.
    pub fn close(self) {
        drop(self);
    }
}

impl Drop for OverlayWindow {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            #[cfg(windows)]
            crate::win::join_bounded(thread);
            #[cfg(not(windows))]
            drop(thread);
        }
    }
}
