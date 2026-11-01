//! Wall-clock UTC stamps for logs, backup names and reports, read from the system clock.
use crate::DateTime;
use std::time::{SystemTime, UNIX_EPOCH};

fn now() -> (i64, u32) {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() as i64, d.subsec_nanos())
}

fn at(secs: i64) -> DateTime {
    DateTime::from_unix(secs, 0).expect("offset 0 is always valid")
}

/// `2026-10-20T12:00:00Z`.
pub fn rfc3339_secs() -> String {
    at(now().0).format_rfc3339()
}

/// `2026-10-20T12:00:00.123Z`: always three fraction digits, so log lines sort and align.
pub fn rfc3339_millis() -> String {
    let (secs, nanos) = now();
    let whole = at(secs).format_rfc3339();
    format!("{}.{:03}Z", whole.trim_end_matches('Z'), nanos / 1_000_000)
}

/// RFC 3339 with the clock's sub-second part (trailing zeros trimmed), for report fields.
pub fn rfc3339_precise() -> String {
    let (secs, nanos) = now();
    DateTime { nanos, ..at(secs) }.format_rfc3339()
}

/// `20261020T120000Z`: the basic form used in file names.
pub fn compact() -> String {
    rfc3339_secs().replace(['-', ':'], "")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(s: &str, pattern: &str) -> bool {
        s.len() == pattern.len()
            && s.chars().zip(pattern.chars()).all(|(c, p)| match p {
                '9' => c.is_ascii_digit(),
                _ => c == p,
            })
    }

    #[test]
    fn stamps_keep_their_shapes() {
        assert!(shape(&rfc3339_secs(), "9999-99-99T99:99:99Z"));
        assert!(shape(&rfc3339_millis(), "9999-99-99T99:99:99.999Z"));
        assert!(shape(&compact(), "99999999T999999Z"));
    }

    #[test]
    fn the_millis_stamp_pads_a_short_fraction() {
        let whole = at(1_000_000_000).format_rfc3339();
        assert_eq!(whole, "2001-09-09T01:46:40Z");
        assert_eq!(
            format!("{}.{:03}Z", whole.trim_end_matches('Z'), 7),
            "2001-09-09T01:46:40.007Z"
        );
    }
}
