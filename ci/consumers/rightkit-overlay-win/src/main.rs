// rightkit-overlay-win 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves the pure, window-free API on every host (so it builds and runs on the macOS runner): Easing and
// Animation sampling, Rgba, DPI and hit-test geometry, path construction and parsing, bar layout, and
// painting into an offscreen PaintCtx. OverlayWindow::create is checked only for its config refusal and,
// off Windows, its Unsupported refusal; on Windows no window is created.
// Does not prove: a live window, hover, presentation, text, glass, per-monitor DPI.
// State: none. No network, no files, no real user data.
use rightkit_overlay_win::{
    bar_rects, client_to_logical, device_regions, device_size, hit_test, parse_svg_path,
    rounded_rect_path, scale_for_dpi, transform_path, Animation, BarStyle, Easing, HitRegion,
    OverlayConfig, OverlayError, OverlayStyle, OverlayWindow, PaintCtx, PathCmd, Rect, Rgba,
};

fn near(left: f32, right: f32) -> bool {
    (left - right).abs() < 1e-4
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect { x, y, width, height }
}

fn main() -> Result<(), String> {
    // Easing: every curve is anchored at 0 and 1; out-back overshoots in between.
    for easing in [Easing::Linear, Easing::EaseIn, Easing::EaseOut, Easing::EaseInOut, Easing::EaseOutBack] {
        assert_eq!(easing.apply(0.0), 0.0, "{easing:?}");
        assert_eq!(easing.apply(1.0), 1.0, "{easing:?}");
        assert_eq!(easing.apply(-3.0), 0.0, "{easing:?} clamps below");
        assert_eq!(easing.apply(9.0), 1.0, "{easing:?} clamps above");
    }
    assert!(near(Easing::Linear.apply(0.25), 0.25));
    assert!(near(Easing::EaseIn.apply(0.5), 0.125));
    assert!(Easing::EaseOutBack.apply(0.7) > 1.0);

    // Animation sampling: a one-shot finishes at exactly 1.0, a loop wraps and never finishes.
    let once = Animation::once(100, Easing::Linear);
    assert_eq!(once.sample(0), (0.0, false));
    assert!(near(once.sample(50).0, 0.5) && !once.sample(50).1);
    assert_eq!(once.sample(100), (1.0, true));
    let looping = Animation::looping(100, Easing::Linear);
    assert!(near(looping.sample(150).0, 0.5) && !looping.sample(150).1);
    assert_eq!(Animation::once(0, Easing::Linear).sample(1), (1.0, true), "zero duration reads as 1 ms");

    // Colour.
    assert_eq!(Rgba::from_hex(0x336699), Rgba::rgb(0x33, 0x66, 0x99));
    assert_eq!(Rgba::WHITE.with_alpha(0.5).a, 128);
    assert_eq!(Rgba::new(1, 2, 3, 200).scaled_alpha(0.5).a, 100);
    assert_eq!(Rgba::BLACK.lerp(Rgba::WHITE, 0.5), Rgba::new(128, 128, 128, 255));
    assert_eq!(Rgba::BLACK.lerp(Rgba::WHITE, 5.0), Rgba::WHITE, "t is clamped");

    // DPI and geometry.
    assert!(near(scale_for_dpi(96), 1.0) && near(scale_for_dpi(144), 1.5) && near(scale_for_dpi(0), 1.0));
    assert_eq!(device_size(36.0, 1.5), 54);
    assert_eq!(device_size(0.0, 1.0), 1, "at least one device pixel");
    assert_eq!(device_size(1.0e9, 1.0), 8192, "at most the surface side limit");
    assert!((client_to_logical(150, 1.5) - 100.0).abs() < 1e-9);

    let regions = [
        HitRegion { id: 1, rect: rect(0.0, 0.0, 100.0, 40.0) },
        HitRegion { id: 2, rect: rect(100.0, 0.0, 50.0, 40.0) },
    ];
    assert_eq!(hit_test(&regions, 10.0, 10.0), Some(1));
    assert_eq!(hit_test(&regions, 100.0, 10.0), Some(2), "touching edge: the later region wins");
    assert_eq!(hit_test(&regions, 150.0, 40.0), Some(2), "edges are inclusive");
    assert_eq!(hit_test(&regions, 151.0, 10.0), None);
    assert_eq!(hit_test(&regions, f64::NAN, 10.0), None);
    let degenerate = [HitRegion { id: 3, rect: rect(1.2, 1.2, 0.0, 5.0) }, regions[0]];
    assert_eq!(device_regions(&degenerate, 1.5), vec![[0, 0, 150, 60]]);
    assert_eq!(device_regions(&[HitRegion { id: 4, rect: rect(0.4, 0.4, 1.0, 1.0) }], 1.0), vec![[0, 0, 2, 2]]);

    // Paths: a rounded rectangle closes; SVG data parses; transform scales then moves.
    let rounded = rounded_rect_path(0.0, 0.0, 20.0, 10.0, 100.0);
    assert!(matches!(rounded.first(), Some(PathCmd::MoveTo(..))) && matches!(rounded.last(), Some(PathCmd::Close)));
    let parsed = parse_svg_path("M0 0 L10 0 L10 10 Z");
    assert_eq!(parsed.len(), 4);
    assert!(matches!(parsed[0], PathCmd::MoveTo(x, y) if x == 0.0 && y == 0.0));
    let moved = transform_path(&parsed, 2.0, 5.0, 1.0);
    assert!(matches!(moved[1], PathCmd::LineTo(x, y) if near(x, 25.0) && near(y, 1.0)));
    assert!(parse_svg_path("").is_empty());

    // Level bars: one rectangle per level, centred, silent bars at the minimum height.
    let style = BarStyle::new(Rgba::WHITE);
    let bars = bar_rects(&[0.0, 1.0, f32::NAN], rect(0.0, 0.0, 40.0, 20.0), &style);
    assert_eq!(bars.len(), 3);
    assert!((bars[0].height - 3.0).abs() < 1e-9 && (bars[1].height - 20.0).abs() < 1e-9);
    assert!((bars[2].height - 3.0).abs() < 1e-9, "non-finite reads as silence");
    assert!(bar_rects(&[], rect(0.0, 0.0, 40.0, 20.0), &style).is_empty());

    // Offscreen painting: a filled rounded rectangle is opaque at its centre and clear outside.
    let mut canvas = PaintCtx::offscreen(60, 30, 96);
    assert_eq!(canvas.pixel_size(), (60, 30));
    assert!(near(canvas.logical_size().width, 60.0));
    canvas.fill_rounded_rect(rect(10.0, 5.0, 40.0, 20.0), 6.0, Rgba::rgb(200, 100, 50));
    assert_eq!(canvas.pixel(30, 15), 0xFF_C8_64_32);
    assert_eq!(canvas.pixel(1, 1), 0);
    assert_eq!(canvas.pixel(9_999, 9_999), 0, "outside the surface reads as 0");
    canvas.clear();
    assert_eq!(canvas.pixel(30, 15), 0);
    let hidpi = PaintCtx::offscreen(120, 60, 192);
    assert!(near(hidpi.scale(), 2.0) && near(hidpi.logical_size().width, 60.0));

    // Window creation: the config is checked first on every host; off Windows no backend exists.
    let unnamed = OverlayWindow::create(OverlayConfig::new("", OverlayStyle::PASSIVE));
    assert!(matches!(unnamed, Err(OverlayError::InvalidConfig(_))));
    #[cfg(not(windows))]
    assert!(matches!(
        OverlayWindow::create(OverlayConfig::passive("consumer")),
        Err(OverlayError::Unsupported)
    ));

    println!("OK rightkit-overlay-win 0.1.0 (registry consumer)");
    Ok(())
}
