//! The seam to `rightkit_overlay_win` (Lane F): the only file that names that crate's types.
//!
//! Coordinate contract (from the crate's `geometry.rs`): `set_frame` takes the window top-left in
//! PHYSICAL virtual-screen pixels and its size in LOGICAL pixels; everything drawn or hit-tested
//! inside the window (`PaintCtx`, `HitRegion`, event coordinates) is LOGICAL. The display lists in
//! `draw.rs` are in points, which are logical pixels, so painters replay them unscaled.
//!
//! API calls used:
//!   OverlayWindow::create(OverlayConfig::new(name, OverlayStyle::PASSIVE) + click_through)
//!   OverlayHandle: set_work_area_clamp, set_frame, show, hide, set_painter, invalidate, animate,
//!                  stop_animation, set_hit_regions, events, reassert_topmost, hwnd, close
//!   `OverlayWindow::close_into_join_handle(self) -> JoinHandle<()>` posts the close and returns the
//!   window thread's handle without waiting; the thread destroys the window (`hwnd()` reads 0),
//!   sends `Closed` and returns. The driver uses it for every window and keeps the handles in
//!   `RETIRE`. Needs the overlay crate that carries it (RightKit commit `1a699008d`, unpublished).
//!   PaintCtx: fill_rounded_rect, stroke_rounded_rect, fill_path, text, clip
//!   Animation::looping, Font::new, Rgba::new, PathCmd::{MoveTo, LineTo, Close}

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Once;

use rightkit_overlay_win as ov;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::AddFontMemResourceEx;
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

use crate::draw::{Align, DrawOp, Rgba, VRect};
use crate::regions::Region;
use crate::text::{Face, ALL_FACES};

pub type Ctx = ov::PaintCtx;
pub type Handle = ov::OverlayHandle;
pub type Window = ov::OverlayWindow;
pub type OverlayEvent = ov::OverlayEvent;

/// What the surface painters implement; [`boxed`] adapts it to `rightkit_overlay_win::Painter`.
pub trait PainterBody: Send + 'static {
    fn body(&mut self, ctx: &mut Ctx);
}

/// Runs a painter body and turns a panic into a fault report. `rightkit_overlay_win` would drop a
/// panicking painter and leave a blank window that still takes clicks inside its hit regions;
/// this adapter instead reports the first panic (`on_fault`, which makes the driver destroy the
/// whole generation) and stops painting. The report carries the payload's type, never its text.
struct Adapter<T: PainterBody> {
    body: T,
    on_fault: Box<dyn Fn(&'static str) + Send>,
    faulted: bool,
}

impl<T: PainterBody> ov::Painter for Adapter<T> {
    fn paint(&mut self, ctx: &mut ov::PaintCtx, _frame: ov::FrameInfo) {
        if self.faulted {
            return;
        }
        if let Err(payload) = catch_unwind(AssertUnwindSafe(|| self.body.body(ctx))) {
            self.faulted = true;
            (self.on_fault)(crate::lifecycle::panic_type(payload.as_ref()));
        }
    }
}

pub fn boxed<T: PainterBody>(
    body: T,
    on_fault: impl Fn(&'static str) + Send + 'static,
) -> Box<dyn ov::Painter> {
    Box::new(Adapter {
        body,
        on_fault: Box::new(on_fault),
        faulted: false,
    })
}

/// One passive, topmost, non-activating layered overlay whose window shape and input follow its
/// hit regions (so the transparent envelope around the capsule never blocks the desktop). The
/// error is a fixed code: the overlay crate's own text is not forwarded.
pub fn create(name: &'static str) -> Result<Window, &'static str> {
    let mut config = ov::OverlayConfig::new(name, rightkit_overlay::OverlayStyle::PASSIVE);
    config.click_through = true;
    let window = ov::OverlayWindow::create(config).map_err(|_| "create_window")?;
    // The pill pins itself 4 pt above the work area and its envelope extends past it; the
    // default clamp would shift the capsule.
    window.handle().set_work_area_clamp(false);
    Ok(window)
}

/// Frame pump: a looping animation keeps the owner thread painting one frame per display refresh
/// until [`Handle::stop_animation`].
pub fn pump() -> ov::Animation {
    ov::Animation::looping(1000, ov::Easing::Linear)
}

/// `x`/`y` physical screen px, `w`/`h` logical px. The size is clamped to
/// `1..=bound::MAX_SURFACE_PX` and a non-finite value counts as 1, so a look cannot ask the overlay
/// for a window of any size it likes.
pub fn frame_rect(x: i32, y: i32, w: f32, h: f32) -> ov::Rect {
    let edge = |v: f32| {
        if v.is_finite() {
            v.clamp(1.0, crate::config::bound::MAX_SURFACE_PX)
        } else {
            1.0
        }
    };
    ov::Rect {
        x: f64::from(x),
        y: f64::from(y),
        width: f64::from(edge(w)),
        height: f64::from(edge(h)),
    }
}

/// The overlay hit region of one of the surface's regions. The pointer shape (`Region::cursor`)
/// is not applied: `rightkit-overlay-win` 0.1.5 has no per-region cursor ([`cursor_is_inert`]
/// tells the driver to report that once per generation).
pub fn region(region: &Region) -> ov::HitRegion {
    ov::HitRegion {
        id: region.id.0,
        rect: to_rect(&region.rect()),
    }
}

/// Whether a region's cursor cannot be applied by the linked overlay backend. Always true until
/// an overlay API for applying region cursors exists.
pub const fn cursor_is_inert() -> bool {
    true
}

/// A coordinate or length from a look: non-finite counts as 0, magnitude capped so no later
/// arithmetic overflows.
fn fin(v: f32) -> f64 {
    if v.is_finite() {
        f64::from(v).clamp(-1.0e5, 1.0e5)
    } else {
        0.0
    }
}

fn to_rect(r: &VRect) -> ov::Rect {
    ov::Rect {
        x: fin(r.x),
        y: fin(r.y),
        width: fin(r.w).max(0.0),
        height: fin(r.h).max(0.0),
    }
}

fn rgba(c: &Rgba) -> ov::Rgba {
    ov::Rgba::new(c.r, c.g, c.b, (c.a.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn font(face: Face, size: f32) -> ov::Font {
    ov::Font::new(face.gdi_family(), size, face.gdi_weight())
}

fn align(a: Align) -> ov::Align {
    match a {
        Align::Left => ov::Align::Left,
        Align::Center => ov::Align::Center,
        Align::Right => ov::Align::Right,
    }
}

/// Replay a display list (points == logical px) onto the overlay's paint context. At most
/// `bound::MAX_DRAW_OPS` operations are replayed, a polygon needs `3..=bound::MAX_POLYGON_POINTS`
/// points and a text run is cut to `bound::MAX_TEXT_RUN_BYTES`: a look cannot make one frame cost
/// more than that.
pub fn replay(ctx: &mut Ctx, ops: &[DrawOp]) {
    use crate::config::bound::{MAX_DRAW_OPS, MAX_POLYGON_POINTS, MAX_TEXT_RUN_BYTES};
    for op in ops.iter().take(MAX_DRAW_OPS) {
        match op {
            DrawOp::FillRRect {
                rect,
                radius,
                color,
            } => {
                if color.a > 0.002 {
                    ctx.fill_rounded_rect(to_rect(rect), fin(*radius) as f32, rgba(color));
                }
            }
            DrawOp::StrokeRRect {
                rect,
                radius,
                width,
                color,
            } => {
                if color.a > 0.002 {
                    ctx.stroke_rounded_rect(
                        to_rect(rect),
                        fin(*radius) as f32,
                        fin(*width) as f32,
                        rgba(color),
                    );
                }
            }
            DrawOp::Polygon { pts, color } => {
                if color.a > 0.002 && pts.len() >= 3 && pts.len() <= MAX_POLYGON_POINTS {
                    let mut cmds: Vec<ov::PathCmd> = Vec::with_capacity(pts.len() + 1);
                    for (i, (x, y)) in pts.iter().enumerate() {
                        let (x, y) = (fin(*x) as f32, fin(*y) as f32);
                        cmds.push(if i == 0 {
                            ov::PathCmd::MoveTo(x, y)
                        } else {
                            ov::PathCmd::LineTo(x, y)
                        });
                    }
                    cmds.push(ov::PathCmd::Close);
                    ctx.fill_path(&cmds, rgba(color));
                }
            }
            DrawOp::Text {
                text,
                face,
                size,
                color,
                x,
                baseline,
                w,
                align: a,
            } => {
                if color.a > 0.002 && !text.is_empty() {
                    // `PaintCtx::text` centres the rendered string vertically in its rectangle, so
                    // the rectangle is the glyph box (ascent + descent) sitting on the baseline.
                    let size = fin(*size).clamp(1.0, 512.0) as f32;
                    let rect = VRect::new(
                        *x,
                        *baseline - face.ascent_em() * size,
                        *w,
                        face.glyph_box(size),
                    );
                    ctx.text(
                        crate::text::cut_bytes(text, MAX_TEXT_RUN_BYTES),
                        font(*face, size),
                        rgba(color),
                        to_rect(&rect),
                        align(*a),
                    );
                }
            }
            DrawOp::Clip(rect) => ctx.clip(to_rect(rect)),
        }
    }
}

/// Load the host's font faces into this process only (never installed system-wide) so the
/// overlay's GDI text path can name them by family. A face with no bytes is a system font.
pub fn register_fonts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for face in ALL_FACES {
            let bytes = face.bytes();
            if bytes.is_empty() {
                continue;
            }
            // The API writes the number of fonts it added through this pointer, so the variable
            // must be mutable and passed as such.
            let mut count = 0u32;
            let Ok(len) = u32::try_from(bytes.len()) else {
                crate::config::report("font_error", &format!("face={face:?} reason=too_large"));
                continue;
            };
            let handle = unsafe {
                AddFontMemResourceEx(
                    bytes.as_ptr() as *const c_void,
                    len,
                    std::ptr::null(),
                    &mut count as *mut u32 as *const u32,
                )
            };
            if handle.is_null() {
                crate::config::report(
                    "font_error",
                    &format!("face={face:?} reason=register_failed"),
                );
            }
        }
    });
}

/// The Windows "show animations in Windows" setting (`SPI_GETCLIENTAREAANIMATION`). `true` when
/// it is on or cannot be read.
pub fn animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SystemParametersInfoW, SPI_GETCLIENTAREAANIMATION,
    };
    let mut enabled: i32 = 1;
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast::<c_void>(),
            0,
        )
    };
    read == 0 || enabled != 0
}

/// Cursor position in physical screen pixels.
pub fn cursor_pos() -> Option<(i32, i32)> {
    let mut pt = POINT { x: 0, y: 0 };
    if unsafe { GetCursorPos(&mut pt) } == 0 {
        None
    } else {
        Some((pt.x, pt.y))
    }
}

/// Play the launch sound through the default output; failures are silent. The host picks the
/// sound (`config::IntroSound`): a WAV image in memory, a WAV file, or none.
pub fn play_intro_sound() {
    use windows_sys::Win32::Media::Audio::{
        PlaySoundW, SND_ASYNC, SND_FILENAME, SND_MEMORY, SND_NODEFAULT,
    };
    let Some(config) = crate::config::try_get() else {
        return;
    };
    match &config.intro_sound {
        crate::config::IntroSound::Off => {}
        crate::config::IntroSound::Wav(wav) => {
            // The bytes live in the installed config for the whole process, so the asynchronous
            // play never reads freed memory.
            unsafe {
                PlaySoundW(
                    wav.as_ptr() as *const u16,
                    std::ptr::null_mut(),
                    SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
                );
            }
        }
        crate::config::IntroSound::File(path) => {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            unsafe {
                PlaySoundW(
                    wide.as_ptr(),
                    std::ptr::null_mut(),
                    SND_FILENAME | SND_ASYNC | SND_NODEFAULT,
                );
            }
        }
    }
}
