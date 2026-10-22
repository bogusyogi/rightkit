//! Native Win32 layered-window overlays for Right Suite apps: no WebView, no GPU stack, no
//! framework. One overlay is one window, one owner thread and one software painter.
//!
//! ```no_run
//! use rightkit_overlay_win::{
//!     Animation, Easing, HitRegion, OverlayConfig, OverlayEvent, OverlayWindow, PaintCtx, Rect, Rgba,
//! };
//!
//! # fn main() -> Result<(), rightkit_overlay_win::OverlayError> {
//! let window = OverlayWindow::create(OverlayConfig::passive("pill"))?;
//! let overlay = window.handle();
//!
//! // A painter is any `FnMut(&mut PaintCtx, FrameInfo) + Send`, or a type implementing `Painter`.
//! overlay.set_painter(Box::new(|ctx: &mut PaintCtx, frame: rightkit_overlay_win::FrameInfo| {
//!     let size = ctx.logical_size();
//!     let body = Rect { x: 0.0, y: 0.0, width: f64::from(size.width), height: f64::from(size.height) };
//!     ctx.glass_layer(body, 18.0, 6.0, Rgba::new(33, 29, 26, 235));
//!     let _ = frame.t; // 0..1 while an animation runs
//! }));
//!
//! // Position in physical screen pixels, size in logical pixels (scaled per monitor).
//! overlay.set_frame(Rect { x: 800.0, y: 960.0, width: 120.0, height: 36.0 });
//! overlay.set_hit_regions(vec![HitRegion {
//!     id: 1,
//!     rect: Rect { x: 0.0, y: 0.0, width: 120.0, height: 36.0 },
//! }]);
//! let events = overlay.events();
//! overlay.show();
//!
//! while let Ok(event) = events.recv() {
//!     match event {
//!         OverlayEvent::HoverEnter(_) => overlay.animate(Animation::once(150, Easing::EaseInOut)),
//!         OverlayEvent::Closed => break,
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Model
//!
//! * **Policy comes from `rightkit-overlay`**: `OverlayStyle` (topmost, never-activating, click
//!   through), `HoverGate` (the hover enter and leave debounce), `WorkArea::clamp` (placement),
//!   `show_noactivate` and the topmost-band repair. This crate adds the window, the painter and
//!   the loop.
//! * **Coordinates**: a frame is a [`Rect`] whose `x`/`y` are the physical-pixel top-left and whose
//!   `width`/`height` are a logical size; everything drawn or hit-tested inside the window is in
//!   logical pixels (see [`place_frame`]). Hit testing is edge-inclusive, like
//!   `rightkit_overlay::Rect::contains`. Each monitor DPI is honoured, and the owner thread is
//!   per-monitor-v2 DPI aware regardless of the process.
//! * **Idle costs nothing**: with no animation and the pointer away, the owner thread blocks in
//!   `GetMessage`. Frames are paced by `DwmFlush` only while an [`Animation`] runs.
//! * **Painting**: [`PaintCtx`] is a premultiplied-BGRA software canvas (rounded rectangles, paths,
//!   arcs, text, level bars, squircles, image blits, cached parameterised glass), copied into a DIB
//!   and presented with `UpdateLayeredWindow`.
//!
//! On non-Windows hosts the crate compiles and [`OverlayWindow::create`] returns
//! [`OverlayError::Unsupported`]; the pure parts (geometry, animation maths, painting into an
//! offscreen [`PaintCtx`]) work everywhere.

// Off Windows the owner thread, hover tracker, event queue and GDI text backend have no caller, so
// their items read as dead code; the workspace gate runs clippy with `-D warnings` on every host.
#![cfg_attr(not(windows), allow(dead_code))]

mod anim;
mod color;
mod events;
mod geometry;
mod hover;
mod paint;
mod path;
mod squircle;
mod text;
mod window;

#[cfg(windows)]
mod win;

pub mod icons;

pub use anim::{Animation, Easing, FrameInfo};
pub use color::Rgba;
pub use events::OverlayEvent;
pub use geometry::{
    client_to_logical, device_regions, device_size, hit_test, place_frame, scale_for_dpi,
    DeviceRect, HitRegion, Placement, Size,
};
pub use hover::HOVER_TICK_MS;
pub use paint::{
    bar_rects, Align, BarStyle, Font, GlassBorder, GlassHighlight, GlassStyle, PaintCtx,
};
pub use path::{parse_svg_path, rounded_rect_path, transform as transform_path, PathCmd};
pub use window::{
    Backdrop, OverlayConfig, OverlayError, OverlayHandle, OverlayWindow, Painter,
    EVENT_CHANNEL_CAPACITY,
};

/// Re-exported from `rightkit-overlay` so a host names one crate: the overlay style presets, the
/// hover debounce and the rectangle type used by frames and hit regions.
pub use rightkit_overlay::{HoverGate, OverlayStyle, Rect};
