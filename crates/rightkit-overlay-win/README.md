# rightkit-overlay-win

Native Win32 layered-window overlays for Right Suite apps: a notch, a pill, a hover card. One overlay is one window, one owner thread and one software painter. No WebView, no GPU stack, no UI framework, no async runtime. The only dependencies are `rightkit-overlay` and `windows-sys`.

`rightkit-overlay` owns the policy (`OverlayStyle`, `HoverGate`, `WorkArea::clamp`, `show_noactivate`, topmost repair, `monitor_at`). This crate adds the window, the painter and the loop, and calls that policy rather than copying it.

## What you get

- **A layered window that never takes focus.** Created through `rightkit_overlay::windows`, styled with `OverlayStyle`, shown with `show_noactivate`, topmost band repaired on request.
- **A software painter** (`PaintCtx`): premultiplied BGRA, so a frame is one copy into a DIB and one `UpdateLayeredWindow`. Rounded rectangles (per-corner radii), continuous-curvature squircles, strokes, discs, arcs, SVG-style paths (fill, even-odd fill, stroke), bilinear image blits, text through GDI with a string cache, level-meter bars, and a cached, parameterised frosted-glass layer (`GlassStyle`).
- **Per-monitor-v2 DPI.** You paint in logical pixels; the context scales. Moving to a monitor with another scale repaints at the new scale and sends `DpiChanged`.
- **Hover and click by region.** `HitRegion { id, rect }`; events carry the id. Hit testing is edge-inclusive, the same rule as `rightkit_overlay::Rect::contains`. Hover goes through `rightkit_overlay::HoverGate`, so enter and leave debounce the way the rest of the suite does.
- **Animation without timers.** `animate` paints one frame per display refresh, paced by `DwmFlush`, and only while an animation is active.
- **Idle costs nothing.** With no animation and the pointer away, the owner thread blocks in `GetMessage`. No timer exists. The only wake-ups that do not come from a message are the animation frames and, briefly after the pointer leaves a region, the 40 ms hover leave-debounce timer, which is killed as soon as the gate settles. Animation frames also stop while the session is locked or disconnected (`WM_WTSSESSION_CHANGE`) and resume, with one repaint, when it is back.
- **Bounded resources.** Fonts (32), glass layers (4, each capped), rendered strings, event channels (256 events per receiver) and the DIB (grow-only with hysteresis) are all bounded, and every GDI object is released on every path. The window class is unregistered when the last window of that name closes.

## Example

```rust
use rightkit_overlay_win::{
    icons, Animation, Easing, FrameInfo, HitRegion, OverlayConfig, OverlayEvent, OverlayWindow,
    PaintCtx, Rect, Rgba,
};

fn main() -> Result<(), rightkit_overlay_win::OverlayError> {
    let window = OverlayWindow::create(OverlayConfig::passive("pill"))?;
    let overlay = window.handle();

    overlay.set_painter(Box::new(|ctx: &mut PaintCtx, frame: FrameInfo| {
        let size = ctx.logical_size();
        let body = Rect { x: 0.0, y: 0.0, width: f64::from(size.width), height: f64::from(size.height) };
        ctx.glass_layer(body, 18.0, 6.0, Rgba::new(33, 29, 26, 235));
        let mic = icons::build(icons::MIC, 20.0, 50.0, 8.0);
        ctx.stroke_path(&mic, icons::STROKE_WIDTH * 20.0 / 24.0, Rgba::WHITE.scaled_alpha(0.6 + 0.4 * frame.t));
    }));

    // Position: physical screen pixels. Size: logical pixels, scaled for the monitor it lands on.
    overlay.set_frame(Rect { x: 800.0, y: 960.0, width: 120.0, height: 36.0 });
    overlay.set_hit_regions(vec![HitRegion {
        id: 1,
        rect: Rect { x: 0.0, y: 0.0, width: 120.0, height: 36.0 },
    }]);
    let events = overlay.events();
    overlay.show();

    while let Ok(event) = events.recv() {
        match event {
            OverlayEvent::HoverEnter(_) => overlay.animate(Animation::once(150, Easing::EaseInOut)),
            OverlayEvent::Click(1) => overlay.close(),
            OverlayEvent::Closed => break,
            _ => {}
        }
    }
    Ok(())
}
```

## API

| Item | Purpose |
| --- | --- |
| `OverlayConfig { name, style, topmost, noactivate, click_through, backdrop }` | How to build the window. `OverlayConfig::new(name, style)` and `::passive(name)` fill the booleans from an `OverlayStyle`. |
| `OverlayWindow::create(cfg)` | Creates the window (hidden) and its owner thread. Dropping it closes the window and joins the thread (bounded). |
| `OverlayWindow::handle()` -> `OverlayHandle` | Cloneable, `Send + Sync`. Every method returns at once; the owner thread applies calls in order. |
| `set_frame(Rect)` | Top-left in **physical** virtual-screen pixels, size in **logical** pixels (see [Rect convention](#rect-convention)). Held inside the monitor work area by `WorkArea::clamp` (`set_work_area_clamp(false)` turns that off). Resizing every frame is cheap: the backing surface is grow-only. |
| `show` / `hide` / `is_visible` | Show without activating (paints first, so nothing flashes empty). Hide releases surfaces and ends any hover. |
| `set_painter(Box<dyn Painter>)` | The frame source. A closure `FnMut(&mut PaintCtx, FrameInfo) + Send` works. |
| `invalidate()` | One repaint on the owner thread. Bursts collapse to one. |
| `animate(Animation)` / `stop_animation()` | `Animation { duration_ms, easing, repeat }`. `FrameInfo.t` is the eased `0..1` progress. |
| `set_hit_regions(Vec<HitRegion>)` | Interactive areas, logical pixels, window-relative, edges inclusive; later regions are on top. |
| `events()` -> `Receiver<OverlayEvent>` | `HoverEnter(id)`, `HoverMove(id, x, y)`, `HoverLeave(id)`, `Click(id)`, `DpiChanged(scale)`, `MonitorChanged`, `Closed`. Each call returns an independent stream of at most `EVENT_CHANNEL_CAPACITY` (256) buffered events; a reader that stops reading misses new events but never blocks the window, and the receiver still disconnects at close. |
| `reassert_topmost()` | Repairs the topmost band; a no-op when the window is already topmost. |
| `close()` | Closes the window; the stream ends with `Closed`. |
| `hwnd()`, `backdrop_applied()` | The raw handle for other `rightkit_overlay::windows` helpers; whether Windows accepted the DWM backdrop. |

`Painter::paint(&mut self, ctx: &mut PaintCtx, frame: FrameInfo)`; `FrameInfo { t, elapsed_ms, dpi, size }`, `size` in logical pixels.

`PaintCtx` (all coordinates logical): `clear`, `clip` / `clear_clip`, `fill_rounded_rect`, `fill_rounded_rect_corners`, `stroke_rounded_rect`, `fill_squircle`, `stroke_squircle`, `fill_circle`, `stroke_arc`, `fill_path`, `fill_path_even_odd`, `stroke_path`, `blit_rgba`, `blit_premultiplied`, `text`, `measure_text`, `bars`, `glass_layer`, `glass_layer_styled`, `invalidate_glass`. `PaintCtx::offscreen(w, h, dpi)` builds a canvas without a window, to unit-test a painter or render a preview. Paths come from `parse_svg_path` (M L H V C Q A Z, absolute and relative; arcs become cubics) or are built from `PathCmd`; `icons` has a small line-icon set on a 24-unit grid.

### Rect convention

`set_frame`, `HitRegion::rect` and every `PaintCtx` rectangle are the suite's `rightkit_overlay::Rect { x, y, width, height }` (this crate re-exports it), but the units differ by use:

| Where | `x`, `y` | `width`, `height` |
| --- | --- | --- |
| `set_frame` | **physical** virtual-screen pixels (the space of `GetCursorPos` and `MonitorPlacement`; negative left of or above the primary monitor) | **logical** pixels, multiplied by the DPI scale of the monitor under the frame centre |
| `HitRegion::rect`, `PaintCtx` rectangles, event coordinates | logical pixels from the window top-left | logical pixels |

So `Rect { x: 800.0, y: 960.0, width: 120.0, height: 36.0 }` is a 180 x 54 pixel window at 150%, whose top-left sits at screen pixel (800, 960). `Rect::contains` is edge-inclusive (all four edges count as inside), and so is `hit_test`: where two regions touch, the one listed later wins.

### Coordinates and events

A frame's monitor is the one under the frame centre; its DPI scales the size. Inside the window everything is logical pixels relative to the top-left, and `FrameInfo.size` is the window size in those units (device size divided by the scale, so it can differ from the requested size by a fraction of a pixel on fractional scales).

Hover: the first pointer sample over a region sends `HoverEnter`. Leaving takes three consecutive outside ticks (`HoverGate`), 40 ms apart, so a pointer crossing a seam between regions or briefly outside does not flicker. Consecutive `HoverMove` events in one region are merged into the latest. A click is a left press and release on the same region.

### Click-through

`OverlayStyle::click_through` keeps the suite meaning: the whole window is invisible to the pointer (a drawing surface that sends no events). `OverlayConfig::click_through` is different: the window takes the shape of the union of its hit regions, so the pointer reaches whatever is underneath everywhere else, with no polling. Pixels painted outside every region are then not shown, and an empty region list makes the window click-through and invisible. Without it, the window is hit-tested by its own alpha: fully transparent pixels already let the pointer through, and regions only name areas for events.

### Backdrop

`Backdrop::Blur` and `Backdrop::Acrylic { tint }` ask DWM for a blur behind the window (`DwmEnableBlurBehindWindow`, and the acrylic accent policy). The window is a per-pixel-alpha layered window, and whether DWM paints a backdrop under such a window depends on the Windows build and how it is composed, so this is best effort. `backdrop_applied()` reports whether the call was accepted, which is not proof that pixels changed. `PaintCtx::glass_layer` is the look that always works; use it with the backdrop off, or at lower alpha over an accepted backdrop.

### Glass

`glass_layer(rect, radius, blur_px, tint)` is `glass_layer_styled(rect, radius, GlassStyle::new(blur_px, tint))`. A `GlassStyle { blur_px, tint, border, highlight }` names the whole material: the shadow falloff (`0` for none), the body fill (its alpha is the see-through amount), a `GlassBorder { color, width, bottom_fade }` rim and a `GlassHighlight { color, extent }` sheen. `GlassBorder::NONE` and `GlassHighlight::NONE` switch them off; `GlassStyle::new` gives the default rim and sheen. A host with its own look (a flatter notch, a coloured rim) builds the style it needs.

Each distinct size, radius and style is drawn once at the current scale and copied after that. It stays cached (up to four layers) until the parameters change, `invalidate_glass()` is called, or the DPI changes. A morph that resizes the glass every frame rebuilds it every frame.

Memory is capped whatever the request: the blur is clamped to 64 device pixels, a layer (body plus shadow margin) is at most 4096 pixels on a side and 2,000,000 pixels of padded area, 32 MB of build memory (16 bytes per pixel while it is built), and a body that does not fit with its full shadow gets a narrower shadow until it does. A body too large even without a shadow, or a non-finite rectangle, draws nothing. Nothing panics.

### Squircles and images

`fill_squircle(rect, radius, color)` and `stroke_squircle(rect, radius, width, color)` draw the continuous-curvature corner of an app icon (the Pulse notch shape): the curve leaves each side 1.53 radii from the vertex and eases in, softer than a circular corner. `blit_rgba(rgba, width, height, dest)` scales a straight-alpha RGBA8 image bilinearly into `dest`, and `blit_premultiplied(pixels, width, height, dest)` does the same for a premultiplied `0xAARRGGBB` buffer. Both composite source-over, honour the clip, fade partly covered edge pixels, and draw nothing for an empty or short image or an empty, non-finite or off-surface destination.

### Surfaces and resizing

The DIB behind a window is grow-only with hysteresis: a size change that still fits it (and leaves it less than twice too large) reuses it, and the canvas is resized in place, so text and glass caches survive. Animating the window size therefore allocates a handful of times, not once per frame. Hiding the window releases the surface and canvas; a DPI change starts a new canvas.

## Manual demo

```text
cargo run -p rightkit-overlay-win --example overlay_demo
cargo run -p rightkit-overlay-win --example overlay_demo -- --blur --seconds 60
```

A dark glass pill appears at the bottom centre of the monitor under the cursor. It takes no focus. Hover it: it expands over 180 ms (mic, level bars, timer, close button). Move away: it collapses after the hover debounce. Click the body: one 700 ms pulse, painted at display-refresh rate, after which the thread idles. Click the X to quit. On a mixed-DPI setup start it with the cursor on each monitor and compare size and sharpness. `--blur` and `--acrylic` print whether Windows accepted the backdrop.

## Tests

Pure-logic unit tests cover animation frame maths, hit-region lookup, DPI scaling and frame placement across monitors, the surface growth policy, event coalescing and the bounded event channels, the hover model on top of `HoverGate`, path parsing and flattening, bar layout, the rasteriser (shapes, squircles, blits and their bounds, clip, fill rules, arcs, glass cache, glass caps and styles) and, on Windows, GDI text, the font and surface bounds, the window class registry and the session-lock pause.

Two integration tests create real windows and are ignored by default because they need an interactive desktop:

```text
cargo test -p rightkit-overlay-win --test windows_smoke -- --ignored
cargo test -p rightkit-overlay-win --test windows_gdi_handles -- --ignored --nocapture
```

`windows_gdi_handles` creates, shows, animates, hides and closes 30 overlays, then three overlays of one name together, then one window resized 400 times, and asserts that `GetGuiResources(GR_GDIOBJECTS)` and the process thread count return to their baseline and that the window class is unregistered after the last close.

Source presence is not native qualification: the integration owner records the unit run, the smoke run, the handle-count run and a manual demo pass on a two-monitor mixed-DPI host.

## API contract and deviations

The consumer is the HeardRight native pill (`src-tauri/src/pill_native_win/gfx.rs`, the only file that names this crate), which coded against this surface. Names and signatures below are stable; changing one is a breaking change for it.

**Contract coded against**

- `OverlayWindow::create(OverlayConfig::new(name, OverlayStyle::PASSIVE))`, with `config.click_through = true` set by the host.
- `OverlayHandle`: `set_work_area_clamp`, `set_frame`, `show`, `hide`, `set_painter`, `invalidate`, `animate`, `stop_animation`, `set_hit_regions`, `events`, `reassert_topmost`, `hwnd`.
- `PaintCtx`: `fill_rounded_rect`, `stroke_rounded_rect`, `fill_path`, `text`, `clip`.
- `Animation::looping`, `Font::new`, `Rgba::new`, `Align`, `PathCmd::{MoveTo, LineTo, Close}`, `HitRegion`, `Rect`, `Painter` and `FrameInfo`.

**Shipped deviations and behaviours to know**

- **`set_frame` mixes units.** `x`/`y` are physical virtual-screen pixels; `width`/`height` are logical pixels scaled by the monitor under the frame centre. It is not a single-unit rectangle (see [Rect convention](#rect-convention)).
- **The work-area clamp is on by default.** `set_frame` holds the frame inside the monitor work area with `WorkArea::clamp`; a host that places outside it (the pill pins itself above the work area) calls `set_work_area_clamp(false)`.
- **`OverlayConfig::click_through` is window shaping, not a style bit.** It makes the window take the shape of the union of its hit regions with `SetWindowRgn`, so the pointer reaches the desktop everywhere else with no polling; pixels painted outside every region are not shown, and an empty region list makes the window invisible and click-through. It is distinct from `OverlayStyle::click_through` (the suite meaning: the whole window ignores the pointer).
- **`events()` returns a fresh receiver on every call.** Each sees events from the moment of the call; it is not a single shared queue. Each receiver is bounded at 256 events and drops new ones while full.
- **`is_visible()` is the last request, not the window state.** It flips at the `show`/`hide` call, before the owner thread has run it, and is false once the window is closed.
- **The DWM backdrop is best effort.** `backdrop_applied()` reports whether Windows accepted the call, not that pixels changed; `glass_layer` is the look that always works.
- **No drag.** There is no move-by-drag, drag region or drag event; a host that wants one moves the window itself with `set_frame` from its own pointer logic.
- **Hit testing is edge-inclusive** (changed from half-open to match `Rect::contains`): a region owns its right and bottom edge, and later regions win where regions touch.
- **`OverlayEvent` is not `rightkit_overlay::OverlayEvent`.** The suite type is the pill state feed that drives an overlay; this one is what the window reports back (pointer, clicks, display changes). They share no variant, so one type cannot carry both; import this one by path or alias it.
- **Animation pauses while the session is locked**, and the glass look gained `GlassStyle` parameters; `glass_layer` keeps its signature and default look.

## Platforms

Windows 10 1703 or later for per-monitor-v2 awareness. On other targets the crate compiles; `OverlayWindow::create` returns `OverlayError::Unsupported`, and the pure parts (geometry, animation maths, painting into an offscreen `PaintCtx`) still work.

## License

MIT OR Apache-2.0. See `NOTICE` for the provenance of the rasteriser.
