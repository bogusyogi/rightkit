//! Manual demo: a glass pill that expands on hover and animates once on click.
//!
//! Run it on a Windows desktop (not in CI; it needs a real display and pointer):
//!
//! ```text
//! cargo run -p rightkit-overlay-win --example overlay_demo
//! cargo run -p rightkit-overlay-win --example overlay_demo -- --blur --seconds 60
//! ```
//!
//! What to check:
//! 1. A small dark glass pill appears at the bottom centre of the monitor under the cursor. It
//!    takes no focus: type into another window while it is up and the caret stays there.
//! 2. Move the pointer onto the pill. After the hover enter it expands over 180 ms into a wider
//!    capsule with a mic, level bars, a timer and a close button.
//! 3. Move the pointer off. After the hover debounce (three ticks of 40 ms) it collapses.
//! 4. Click the pill body: one 700 ms pulse (a ring grows and fades, the bars dance). Frames are
//!    paced by the display refresh; when it ends the overlay thread goes back to sleeping in
//!    GetMessage (Task Manager: 0% CPU, and no timer exists while the pointer is away).
//! 5. Click the X (or run with `--seconds N`): the demo closes.
//! 6. On a multi-monitor setup with mixed scaling, start the demo with the cursor on each monitor
//!    in turn: the pill must be the same logical size and equally sharp on all of them.
//!
//! Flags: `--blur` or `--acrylic` ask DWM for a backdrop. Windows may ignore it for a
//! per-pixel-alpha window; the demo prints whether it was accepted, and the glass layer looks right
//! either way. `--seconds N` exits by itself after N seconds (default 120).

#[cfg(not(windows))]
fn main() {
    eprintln!("overlay_demo needs Windows: the overlay backend is Win32.");
}

#[cfg(windows)]
fn main() {
    demo::run();
}

#[cfg(windows)]
mod demo {
    use std::f32::consts::TAU;
    use std::sync::mpsc::RecvTimeoutError;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use rightkit_overlay::windows::monitor_at;
    use rightkit_overlay_win::{
        icons, Align, Animation, Backdrop, BarStyle, Easing, Font, FrameInfo, HitRegion,
        OverlayConfig, OverlayEvent, OverlayWindow, PaintCtx, Painter, Rect, Rgba,
    };
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    const EMBER: Rgba = Rgba::rgb(0xFF, 0x56, 0x30);
    const FOREGROUND: Rgba = Rgba::rgb(0xF3, 0xEE, 0xEA);
    const WINDOW_W: f64 = 280.0;
    const WINDOW_H: f64 = 96.0;
    const REGION_PILL: u32 = 1;
    const REGION_CLOSE: u32 = 2;
    const MORPH_MS: u32 = 180;
    const PULSE_MS: u32 = 700;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Kind {
        /// `t` drives the pill from `from` to `to`.
        Morph,
        /// `t` drives the click pulse; the pill rests at `to`.
        Pulse,
    }

    /// What the main thread tells the painter. The painter runs on the overlay thread.
    struct Demo {
        from: f32,
        to: f32,
        kind: Kind,
        hot: Option<u32>,
        /// Written by the painter each frame: where the pill actually is.
        current: f32,
        glass_alpha: u8,
    }

    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }

    fn pill_rect(expand: f32) -> Rect {
        let (w, h) = (lerp(120.0, 220.0, expand), lerp(36.0, 44.0, expand));
        Rect {
            x: (WINDOW_W - f64::from(w)) / 2.0,
            y: (WINDOW_H - f64::from(h)) / 2.0,
            width: f64::from(w),
            height: f64::from(h),
        }
    }

    fn close_rect() -> Rect {
        let body = pill_rect(1.0);
        Rect {
            x: body.x + body.width - 14.0 - 24.0,
            y: body.y + (body.height - 24.0) / 2.0,
            width: 24.0,
            height: 24.0,
        }
    }

    /// The pill is one region while collapsed; expanded it is the body plus the close button,
    /// side by side so the pointer crosses from one to the other.
    fn regions(expanded: bool) -> Vec<HitRegion> {
        if !expanded {
            return vec![HitRegion {
                id: REGION_PILL,
                rect: pill_rect(0.0),
            }];
        }
        let body = pill_rect(1.0);
        let close = close_rect();
        vec![
            HitRegion {
                id: REGION_PILL,
                rect: Rect {
                    width: close.x - body.x,
                    ..body
                },
            },
            HitRegion {
                id: REGION_CLOSE,
                rect: close,
            },
        ]
    }

    struct PillPainter {
        state: Arc<Mutex<Demo>>,
    }

    impl Painter for PillPainter {
        fn paint(&mut self, ctx: &mut PaintCtx, frame: FrameInfo) {
            let (expand, hot, pulsing, glass_alpha) = {
                let mut demo = self.state.lock().unwrap_or_else(|p| p.into_inner());
                let expand = match demo.kind {
                    Kind::Morph => lerp(demo.from, demo.to, frame.t),
                    Kind::Pulse => demo.to,
                };
                demo.current = expand;
                (expand, demo.hot, demo.kind == Kind::Pulse, demo.glass_alpha)
            };
            let body = pill_rect(expand);
            let radius = (body.height / 2.0) as f32;
            ctx.glass_layer(body, radius, 8.0, Rgba::new(33, 29, 26, glass_alpha));

            let (cx, cy) = (body.x + body.width / 2.0, body.y + body.height / 2.0);
            let stroke = |size: f32| icons::STROKE_WIDTH * size / 24.0;

            // Collapsed face: a mic, fading out as the pill opens.
            let rest = 1.0 - expand;
            if rest > 0.02 {
                let mic = icons::build(icons::MIC, 20.0, cx as f32 - 10.0, cy as f32 - 10.0);
                ctx.stroke_path(&mic, stroke(20.0), FOREGROUND.scaled_alpha(rest));
            }

            // Expanded face: mic, level bars, timer and the close button, fading in.
            if expand > 0.02 {
                let fade = |c: Rgba| c.scaled_alpha(expand);
                let mic = icons::build(icons::MIC, 20.0, body.x as f32 + 14.0, cy as f32 - 10.0);
                ctx.stroke_path(&mic, stroke(20.0), fade(FOREGROUND));

                let levels: [f32; 5] = if pulsing {
                    let phase = frame.t * TAU * 3.0;
                    let calm = 1.0 - frame.t;
                    std::array::from_fn(|i| calm * (0.5 + 0.5 * (phase + i as f32 * 0.9).sin()))
                } else {
                    [0.35, 0.6, 0.9, 0.6, 0.35]
                };
                let bars = Rect {
                    x: body.x + 44.0,
                    y: body.y + 6.0,
                    width: 25.0,
                    height: body.height - 12.0,
                };
                ctx.bars(&levels, bars, BarStyle::new(fade(EMBER)));

                let label = Rect {
                    x: body.x + 79.0,
                    y: body.y,
                    width: 60.0,
                    height: body.height,
                };
                ctx.text(
                    "00:07",
                    Font::mono(13.0),
                    fade(FOREGROUND),
                    label,
                    Align::Left,
                );

                let close = close_rect();
                if hot == Some(REGION_CLOSE) {
                    ctx.fill_rounded_rect(close, 12.0, fade(Rgba::WHITE.with_alpha(0.18)));
                }
                let cross =
                    icons::build(icons::X, 14.0, close.x as f32 + 5.0, close.y as f32 + 5.0);
                ctx.stroke_path(&cross, stroke(14.0), fade(FOREGROUND));
            }

            // The click pulse: a ring that grows off the pill and fades.
            if pulsing && frame.t < 1.0 {
                let grow = 10.0 * f64::from(frame.t);
                let ring = Rect {
                    x: body.x - grow,
                    y: body.y - grow,
                    width: body.width + 2.0 * grow,
                    height: body.height + 2.0 * grow,
                };
                ctx.stroke_rounded_rect(
                    ring,
                    radius + grow as f32,
                    2.0,
                    EMBER.scaled_alpha(1.0 - frame.t),
                );
            }
        }
    }

    fn parse_args() -> (Backdrop, u64) {
        let mut backdrop = Backdrop::None;
        let mut seconds = 120;
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--blur" => backdrop = Backdrop::Blur,
                "--acrylic" => {
                    backdrop = Backdrop::Acrylic {
                        tint: Rgba::new(33, 29, 26, 150),
                    }
                }
                "--seconds" => {
                    seconds = args.next().and_then(|v| v.parse().ok()).unwrap_or(seconds);
                }
                other => eprintln!("ignoring unknown argument {other}"),
            }
        }
        (backdrop, seconds)
    }

    /// Bottom centre of the work area of the monitor under the cursor, in physical pixels.
    fn start_frame() -> Rect {
        let mut cursor = POINT { x: 0, y: 0 };
        // SAFETY: GetCursorPos writes one POINT.
        unsafe { GetCursorPos(&mut cursor) };
        let (work, scale) = monitor_at(cursor.x, cursor.y)
            .map(|m| (m.work, m.scale()))
            .unwrap_or((
                rightkit_overlay::WorkArea {
                    left: 0.0,
                    top: 0.0,
                    right: 1920.0,
                    bottom: 1040.0,
                },
                1.0,
            ));
        let width_px = WINDOW_W * scale;
        let height_px = WINDOW_H * scale;
        Rect {
            // Position is physical pixels; size is logical and scaled per monitor by the overlay.
            x: work.left + ((work.right - work.left) - width_px) / 2.0,
            y: work.bottom - height_px - 4.0 * scale,
            width: WINDOW_W,
            height: WINDOW_H,
        }
    }

    pub fn run() {
        // The overlay thread sets its own DPI awareness; this thread asks for physical pixels so
        // the monitor numbers above are real pixels.
        // SAFETY: process-wide setting made once at start-up.
        unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        let (backdrop, seconds) = parse_args();
        let window = match OverlayWindow::create(OverlayConfig {
            backdrop,
            ..OverlayConfig::passive("rk-overlay-demo")
        }) {
            Ok(window) => window,
            Err(error) => {
                eprintln!("could not create the overlay: {error}");
                return;
            }
        };
        let overlay = window.handle();
        if backdrop != Backdrop::None {
            println!(
                "DWM backdrop {:?}: accepted by Windows = {}",
                backdrop,
                overlay.backdrop_applied()
            );
        }

        let state = Arc::new(Mutex::new(Demo {
            from: 0.0,
            to: 0.0,
            kind: Kind::Morph,
            hot: None,
            current: 0.0,
            glass_alpha: if overlay.backdrop_applied() { 130 } else { 228 },
        }));
        overlay.set_painter(Box::new(PillPainter {
            state: Arc::clone(&state),
        }));
        overlay.set_frame(start_frame());
        overlay.set_hit_regions(regions(false));
        let events = overlay.events();
        overlay.show();

        println!("overlay_demo: hover the pill to expand it, click it to pulse, click X to quit.");
        println!("It exits by itself after {seconds} s.");

        let set_target = |to: f32, kind: Kind| {
            let mut demo = state.lock().unwrap_or_else(|p| p.into_inner());
            demo.from = demo.current;
            demo.to = to;
            demo.kind = kind;
        };

        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut expanded = false;
        let mut hovered: Option<u32> = None;
        let mut collapse_at: Option<Instant> = None;
        let mut moves = 0u32;
        loop {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let wake = collapse_at.map_or(deadline, |at| at.min(deadline));
            match events.recv_timeout(wake.saturating_duration_since(now)) {
                Ok(OverlayEvent::Closed) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(OverlayEvent::HoverEnter(id)) => {
                    println!("hover enter region {id}");
                    hovered = Some(id);
                    collapse_at = None;
                    if !expanded {
                        expanded = true;
                        set_target(1.0, Kind::Morph);
                        overlay.set_hit_regions(regions(true));
                        overlay.animate(Animation::once(MORPH_MS, Easing::EaseInOut));
                    }
                    if id == REGION_CLOSE {
                        state.lock().unwrap_or_else(|p| p.into_inner()).hot = Some(id);
                        overlay.invalidate();
                    }
                }
                Ok(OverlayEvent::HoverMove(..)) => moves += 1,
                Ok(OverlayEvent::HoverLeave(id)) => {
                    println!("hover leave region {id} ({moves} moves coalesced into events)");
                    moves = 0;
                    if hovered == Some(id) {
                        hovered = None;
                    }
                    {
                        let mut demo = state.lock().unwrap_or_else(|p| p.into_inner());
                        if demo.hot == Some(id) {
                            demo.hot = None;
                            drop(demo);
                            overlay.invalidate();
                        }
                    }
                    // The pointer may be crossing from the body to the button: wait a beat for
                    // the enter that follows before collapsing.
                    collapse_at = Some(Instant::now() + Duration::from_millis(25));
                }
                Ok(OverlayEvent::Click(REGION_CLOSE)) => {
                    println!("close clicked");
                    overlay.close();
                }
                Ok(OverlayEvent::Click(id)) => {
                    println!("click region {id}: one {PULSE_MS} ms pulse");
                    let to = state.lock().unwrap_or_else(|p| p.into_inner()).to;
                    set_target(to, Kind::Pulse);
                    overlay.animate(Animation::once(PULSE_MS, Easing::EaseOut));
                }
                Ok(OverlayEvent::DpiChanged(scale)) => println!("dpi changed: scale {scale}"),
                Ok(OverlayEvent::MonitorChanged) => println!("monitor changed"),
                Err(RecvTimeoutError::Timeout) => {
                    if collapse_at.is_some_and(|at| Instant::now() >= at) {
                        collapse_at = None;
                        if hovered.is_none() && expanded {
                            expanded = false;
                            println!("collapse");
                            set_target(0.0, Kind::Morph);
                            overlay.set_hit_regions(regions(false));
                            overlay.animate(Animation::once(MORPH_MS, Easing::EaseInOut));
                        }
                    }
                }
            }
        }
        overlay.close();
        window.close();
    }
}
