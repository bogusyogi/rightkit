//! Typed construction (`Decimal::try_parse`, `Unit::try_parse`) and serde must
//! accept and refuse exactly the same text. One table drives both paths. The
//! typed path refuses with a stable reason code.

use rightkit_channel_memory::{Decimal, MemoryError, ReasonCode, Unit};
use serde_json::json;

/// (text, accepted). Includes the length boundary: 40 characters pass, 41 fail.
fn decimal_cases() -> Vec<(String, bool)> {
    let mut cases: Vec<(String, bool)> = [
        ("0", true),
        ("-0", true),
        ("0.0", true),
        ("1240", true),
        ("1240.00", true),
        ("-2.5", true),
        ("", false),
        ("-", false),
        (".5", false),
        ("1.", false),
        ("1,240", false),
        ("1e3", false),
        ("01", false),
        ("NaN", false),
        (" 1", false),
        ("1240.5 ", false),
        ("1_0", false),
        ("+1", false),
    ]
    .iter()
    .map(|(text, ok)| (text.to_string(), *ok))
    .collect();
    cases.push((format!("1{}", "0".repeat(39)), true));
    cases.push((format!("1{}", "0".repeat(40)), false));
    cases
}

/// (text, accepted). The 08d and 08e unit cases.
fn unit_cases() -> Vec<(String, bool)> {
    let mut cases: Vec<(String, bool)> = [
        ("USD", true),
        ("EUR", true),
        ("percent", true),
        ("count", true),
        ("days", true),
        ("seconds", true),
        ("views", true),
        ("subscribers", true),
        ("episodes", true),
        ("shorts_per_week", true),
        ("orders_per_month", true),
        ("views_per_day", true),
        ("upper_percentile", true),
        ("viewers_percent", true),
        ("per_capita_score", true),
        ("super_fans", true),
        ("Views", false),
        ("shorts per week", false),
        ("9lives", false),
        ("", false),
        ("US", false),
        ("USDX", false),
        ("_per_week", false),
        ("x_per_hour", false),
        ("a_per_week_per_day", false),
        ("a_per_per_day", false),
        ("x_per", false),
        ("views_per_", false),
        ("views_per", false),
        ("x_per_per_day", false),
    ]
    .iter()
    .map(|(text, ok)| (text.to_string(), *ok))
    .collect();
    cases.push((format!("a{}", "b".repeat(31)), true));
    cases.push((format!("a{}", "b".repeat(32)), false));
    cases
}

fn code(error: MemoryError) -> ReasonCode {
    match error {
        MemoryError::Refused { code, .. } => code,
        other => panic!("expected a typed refusal, got {other:?}"),
    }
}

#[test]
fn the_typed_and_serde_paths_accept_and_refuse_the_same_decimals() {
    for (text, accepted) in decimal_cases() {
        let typed = Decimal::try_parse(&text);
        let serde = serde_json::from_value::<Decimal>(json!(text));
        assert_eq!(typed.is_ok(), accepted, "typed path for `{text}`");
        assert_eq!(serde.is_ok(), accepted, "serde path for `{text}`");
        if let Err(error) = typed {
            assert_eq!(code(error), ReasonCode::NumberInvalid, "`{text}`");
        }
    }
}

#[test]
fn the_typed_and_serde_paths_accept_and_refuse_the_same_units() {
    for (text, accepted) in unit_cases() {
        let typed = Unit::try_parse(&text);
        let serde = serde_json::from_value::<Unit>(json!(text));
        assert_eq!(typed.is_ok(), accepted, "typed path for `{text}`");
        assert_eq!(serde.is_ok(), accepted, "serde path for `{text}`");
        if let Err(error) = typed {
            assert_eq!(code(error), ReasonCode::UnitInvalid, "`{text}`");
        }
    }
}

#[test]
fn a_typed_value_round_trips_to_the_same_text() {
    let value = Decimal::try_parse("1240.00").unwrap();
    assert_eq!(value.as_str(), "1240.00");
    let unit = Unit::try_parse("shorts_per_week").unwrap();
    assert_eq!(unit.as_str(), "shorts_per_week");
}
