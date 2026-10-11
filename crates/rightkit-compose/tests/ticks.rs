//! Tick conversions on the CutRight clock (705,600,000 ticks per second).

use rightkit_compose::*;

const NTSC_30: Rational = Rational { num: 30000, den: 1001 };
const NTSC_24: Rational = Rational { num: 24000, den: 1001 };
const NTSC_60: Rational = Rational { num: 60000, den: 1001 };

#[test]
fn clock_constants_match_cutright() {
    assert_eq!(TICKS_PER_SECOND, 705_600_000);
    assert_eq!(TICKS_PER_MS, 705_600);
    assert_eq!(TICKS_PER_SECOND / 1000, TICKS_PER_MS);
}

#[test]
fn milliseconds_convert_exactly() {
    assert_eq!(Ticks::from_ms(0), Ticks(0));
    assert_eq!(Ticks::from_ms(1), Ticks(705_600));
    assert_eq!(Ticks::from_ms(1000), Ticks(705_600_000));
    assert_eq!(Ticks::from_ms(1).to_ms_f64(), 1.0);
    assert_eq!(Ticks::from_ms(12_345).to_ms_f64(), 12_345.0);
    // Saturates instead of wrapping for absurd inputs.
    assert_eq!(Ticks::from_ms(u64::MAX), Ticks::MAX);
}

#[test]
fn seconds_as_rationals_are_floored_and_exact_when_whole() {
    assert_eq!(Ticks::from_seconds_rational(3, 2), Some(Ticks(1_058_400_000)));
    assert_eq!(Ticks::from_seconds_rational(1, 3), Some(Ticks(235_200_000)));
    // 1/7 s is exactly 100,800,000 ticks; 1/11 s is 64,145,454.5 and floors.
    assert_eq!(Ticks::from_seconds_rational(1, 7), Some(Ticks(100_800_000)));
    assert_eq!(Ticks::from_seconds_rational(1, 11), Some(Ticks(64_145_454)));
    assert_eq!(Ticks::from_seconds_rational(1, 0), None);
}

#[test]
fn frame_duration_at_ntsc_rates_is_an_integer_tick_count() {
    assert_eq!(NTSC_30.frame_ticks(), Some(23_543_520));
    assert_eq!(NTSC_24.frame_ticks(), Some(29_429_400));
    assert_eq!(NTSC_60.frame_ticks(), Some(11_771_760));
    assert_eq!(Rational::new(30, 1).frame_ticks(), Some(23_520_000));
    assert_eq!(Rational::new(7, 1).frame_ticks(), Some(100_800_000));
    // 11 fps does not divide the clock, so there is no whole frame duration.
    assert_eq!(Rational::new(11, 1).frame_ticks(), None);
}

#[test]
fn ntsc_frame_boundaries_are_exact() {
    let step = NTSC_30.frame_ticks().unwrap();
    for n in [0_u64, 1, 2, 29, 30, 59, 1_000, 107_892, 3_600_000] {
        let boundary = Ticks::from_frames(n, NTSC_30).expect("in range");
        assert_eq!(boundary, Ticks(n * step), "frame {n}");
        // Frame boundaries are not integer milliseconds at 29.97 fps, yet
        // each one round-trips through the tick clock with no error.
        assert_eq!(boundary.0 % step, 0);
    }
    // One NTSC frame is 1001/30000 s, which is 1001 * 705,600,000 / 30000 ticks.
    assert_eq!(Ticks::from_frames(1, NTSC_30), Some(Ticks(23_543_520)));
    assert_eq!(
        Ticks::from_frames(30_000, NTSC_30),
        Some(Ticks::from_seconds_rational(1001, 1).unwrap())
    );
}

#[test]
fn integer_frame_rates_match_their_seconds() {
    let fps25 = Rational::new(25, 1);
    assert_eq!(Ticks::from_frames(25, fps25), Some(Ticks::from_ms(1000)));
    assert_eq!(Ticks::from_frames(1, fps25), Some(Ticks(28_224_000)));
    assert_eq!(Ticks::from_frames(3, Rational::new(0, 1)), None);
}

#[test]
fn ticks_serialise_as_bare_integers() {
    let t = Ticks::from_ms(250);
    assert_eq!(serde_json::to_string(&t).unwrap(), "176400000");
    let back: Ticks = serde_json::from_str("176400000").unwrap();
    assert_eq!(back, t);
}

#[test]
fn checked_and_saturating_arithmetic_do_not_wrap() {
    assert_eq!(Ticks::MAX.checked_add(Ticks(1)), None);
    assert_eq!(Ticks::MAX.saturating_add(Ticks(1)), Ticks::MAX);
    assert_eq!(Ticks(5).checked_sub(Ticks(6)), None);
    assert_eq!(Ticks(6).checked_sub(Ticks(5)), Some(Ticks(1)));
}
