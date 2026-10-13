//! `WindowSpec` per-window options (M1-G1): the pure parts, without a window.

use rightkit_shell::window_state::MonitorSnapshot;
use rightkit_shell::windows::{correct_offscreen, spec_min_size, WindowSpec};

fn mon(x: f64, y: f64, w: f64, h: f64) -> MonitorSnapshot {
    MonitorSnapshot {
        x,
        y,
        width: w,
        height: h,
        scale: 1.0,
    }
}

#[test]
fn default_minimum_is_unchanged() {
    assert_eq!(spec_min_size(&WindowSpec::default()), (320.0, 240.0));
}

#[test]
fn minimum_is_configurable_and_bad_values_fall_back() {
    let s = WindowSpec {
        min_width: Some(600.0),
        min_height: Some(400.0),
        ..Default::default()
    };
    assert_eq!(spec_min_size(&s), (600.0, 400.0));
    let bad = WindowSpec {
        min_width: Some(f64::NAN),
        min_height: Some(-5.0),
        ..Default::default()
    };
    assert_eq!(spec_min_size(&bad), (320.0, 240.0));
    let low = WindowSpec {
        min_width: Some(200.0),
        ..Default::default()
    };
    assert_eq!(spec_min_size(&low).0, 200.0);
}

#[test]
fn spec_fields_round_trip_camel_case_and_default_to_none() {
    let s: WindowSpec = serde_json::from_str(
        r#"{"label":"a","minWidth":800,"minHeight":560,"decorations":false,"center":true,"fullscreen":false,"skipTaskbar":true,"x":5,"y":6}"#,
    )
    .unwrap();
    assert_eq!((s.min_width, s.min_height), (Some(800.0), Some(560.0)));
    assert_eq!(
        (s.decorations, s.center, s.fullscreen, s.skip_taskbar),
        (Some(false), Some(true), Some(false), Some(true))
    );
    let d: WindowSpec = serde_json::from_str(r#"{"label":"a"}"#).unwrap();
    assert!(
        d.min_width.is_none() && d.decorations.is_none() && d.center.is_none() && d.x.is_none()
    );
}

#[test]
fn on_screen_position_is_unchanged() {
    let m = [mon(0.0, 0.0, 1920.0, 1080.0)];
    assert_eq!(
        correct_offscreen(100.0, 80.0, 900.0, 640.0, &m),
        (100.0, 80.0)
    );
    // Mostly off the left edge but 200px of it visible: reachable, untouched.
    assert_eq!(
        correct_offscreen(-700.0, 10.0, 900.0, 640.0, &m),
        (-700.0, 10.0)
    );
}

#[test]
fn off_screen_position_is_clamped_onto_nearest_monitor() {
    let m = [mon(0.0, 0.0, 1920.0, 1080.0)];
    assert_eq!(
        correct_offscreen(5000.0, 100.0, 900.0, 640.0, &m),
        (1840.0, 100.0)
    );
    assert_eq!(
        correct_offscreen(100.0, 4000.0, 900.0, 640.0, &m),
        (100.0, 1040.0)
    );
    assert_eq!(
        correct_offscreen(-3000.0, -500.0, 900.0, 640.0, &m),
        (-820.0, 0.0)
    );
}

#[test]
fn unplugged_second_monitor_falls_to_remaining_one() {
    let m = [mon(0.0, 0.0, 1440.0, 900.0)];
    let (x, y) = correct_offscreen(2500.0, 200.0, 800.0, 600.0, &m);
    assert_eq!((x, y), (1360.0, 200.0));
}

#[test]
fn no_monitors_or_non_finite_input_is_safe() {
    assert_eq!(
        correct_offscreen(10.0, 20.0, 800.0, 600.0, &[]),
        (10.0, 20.0)
    );
    let m = [mon(0.0, 0.0, 1920.0, 1080.0)];
    assert_eq!(
        correct_offscreen(f64::NAN, 5.0, 800.0, 600.0, &m),
        (0.0, 0.0)
    );
}
