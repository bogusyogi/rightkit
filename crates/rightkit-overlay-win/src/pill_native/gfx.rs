//! The seam to `rightkit_overlay_win` (Lane F): the only file that names that crate's types.
//!
//! Coordinate contract (from the crate's `geometry.rs`): `set_frame` takes the window top-left in
//! PHYSICAL virtual-screen pixels and its size in LOGICAL pixels; everything drawn or hit-tested
//! inside the window (`PaintCtx`, `HitRegion`, event coordinates) is LOGICAL. The display lists in
//! `visuals.rs` are in points, which are logical pixels, so painters replay them unscaled.
//!
//! API calls used:
//!   OverlayWindow::create(OverlayConfig::new(name, OverlayStyle::PASSIVE) + click_through)
//!   OverlayHandle: set_work_area_clamp, set_frame, show, hide, set_painter, invalidate, animate,
//!                  stop_animation, set_hit_regions, events, reassert_topmost, hwnd
//!   PaintCtx: fill_rounded_rect, stroke_rounded_rect, fill_path, text, clip
//!   Animation::looping, Font::new, Rgba::new, PathCmd::{MoveTo, LineTo, Close}

use std::ffi::c_void;
use std::sync::Once;

use crate as ov;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::AddFontMemResourceEx;
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

use super::visuals::{Align, DrawOp, Face, Rgba, VRect, ALL_FACES};

pub type Ctx = ov::PaintCtx;
pub type Handle = ov::OverlayHandle;
pub type Window = ov::OverlayWindow;
pub type OverlayEvent = ov::OverlayEvent;

/// What the surface painters implement; [`boxed`] adapts it to `rightkit_overlay_win::Painter`.
pub trait PainterBody: Send + 'static {
    fn body(&mut self, ctx: &mut Ctx);
}

struct Adapter<T: PainterBody>(T);

impl<T: PainterBody> ov::Painter for Adapter<T> {
    fn paint(&mut self, ctx: &mut ov::PaintCtx, _frame: ov::FrameInfo) {
        self.0.body(ctx);
    }
}

pub fn boxed<T: PainterBody>(body: T) -> Box<dyn ov::Painter> {
    Box::new(Adapter(body))
}

/// One passive, topmost, non-activating layered overlay whose window shape and input follow its
/// hit regions (so the transparent envelope around the capsule never blocks the desktop).
pub fn create(name: &'static str) -> Result<Window, String> {
    let mut config = ov::OverlayConfig::new(name, rightkit_overlay::OverlayStyle::PASSIVE);
    config.click_through = true;
    let window = ov::OverlayWindow::create(config).map_err(|error| error.to_string())?;
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

/// `x`/`y` physical screen px, `w`/`h` logical px.
pub fn frame_rect(x: i32, y: i32, w: f32, h: f32) -> ov::Rect {
    ov::Rect {
        x: f64::from(x),
        y: f64::from(y),
        width: f64::from(w),
        height: f64::from(h),
    }
}

pub fn region(id: u32, rect: VRect) -> ov::HitRegion {
    ov::HitRegion {
        id,
        rect: to_rect(&rect),
    }
}

fn to_rect(r: &VRect) -> ov::Rect {
    ov::Rect {
        x: f64::from(r.x),
        y: f64::from(r.y),
        width: f64::from(r.w),
        height: f64::from(r.h),
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

/// Replay a display list (points == logical px) onto the overlay's paint context.
pub fn replay(ctx: &mut Ctx, ops: &[DrawOp]) {
    for op in ops {
        match op {
            DrawOp::FillRRect {
                rect,
                radius,
                color,
            } => {
                if color.a > 0.002 {
                    ctx.fill_rounded_rect(to_rect(rect), *radius, rgba(color));
                }
            }
            DrawOp::StrokeRRect {
                rect,
                radius,
                width,
                color,
            } => {
                if color.a > 0.002 {
                    ctx.stroke_rounded_rect(to_rect(rect), *radius, *width, rgba(color));
                }
            }
            DrawOp::Polygon { pts, color } => {
                if color.a > 0.002 && pts.len() >= 3 {
                    let mut cmds: Vec<ov::PathCmd> = Vec::with_capacity(pts.len() + 1);
                    for (i, (x, y)) in pts.iter().enumerate() {
                        cmds.push(if i == 0 {
                            ov::PathCmd::MoveTo(*x, *y)
                        } else {
                            ov::PathCmd::LineTo(*x, *y)
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
                    let rect = VRect::new(
                        *x,
                        *baseline - face.ascent_em() * *size,
                        *w,
                        face.glyph_box(*size),
                    );
                    ctx.text(
                        text,
                        font(*face, *size),
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

/// Load the five bundled faces into this process only (never installed system-wide) so the
/// overlay's GDI text path can name them by family.
pub fn register_fonts() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for face in ALL_FACES {
            let bytes = face.bytes();
            let count = 0u32;
            let handle = unsafe {
                AddFontMemResourceEx(
                    bytes.as_ptr() as *const c_void,
                    bytes.len() as u32,
                    std::ptr::null(),
                    &count,
                )
            };
            if handle.is_null() {
                super::actions::log_message(
                    "pill_native_font_error",
                    &format!("face={face:?} AddFontMemResourceEx failed"),
                );
            }
        }
    });
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

/// Play the launch swell (`BloomSoundPlayer`) through the default output; failures are silent.
pub fn play_intro_sound() {
    use std::sync::OnceLock;
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    let wav = WAV.get_or_init(super::visuals::bloom_wav);
    unsafe {
        PlaySoundW(
            wav.as_ptr() as *const u16,
            std::ptr::null_mut(),
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}
