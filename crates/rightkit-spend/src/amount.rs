//! Money and identifier text helpers for apps that take amounts and ids from people and tools:
//! decimal amounts to and from micro-units, id and currency checks, and the length-prefixed
//! encoding a seal covers.
//!
//! Amounts are integer micro-units (one millionth of a unit), never a float, so nothing drifts.
//! Each check returns a plain `Option` or `bool`: the app words its own refusal.
//!
//! Ported from Pulse `core/src/budget.rs` (Orthic-Labs/pulse#141).

const MICROS: i64 = 1_000_000;

/// The largest whole-unit amount [`parse_decimal_micro`] accepts (1e9), so sums stay far inside
/// `i64`.
pub const MAX_WHOLE_UNITS: i64 = 1_000_000_000;

/// A decimal amount ("25", "25.50", up to 6 decimals, at most [`MAX_WHOLE_UNITS`]) as micro-units.
/// `allow_zero` accepts 0 (a report of no spend). `None` for anything else: a sign, an exponent,
/// an empty part, a trailing `.`, too many digits.
pub fn parse_decimal_micro(text: &str, allow_zero: bool) -> Option<i64> {
    let (whole, frac) = match text.split_once('.') {
        Some((whole, frac)) if !frac.is_empty() => (whole, frac),
        Some(_) => return None,
        None => (text, ""),
    };
    if whole.is_empty()
        || whole.len() > 10
        || frac.len() > 6
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !frac.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let whole: i64 = whole.parse().ok()?;
    if whole > MAX_WHOLE_UNITS {
        return None;
    }
    let fraction: i64 = if frac.is_empty() {
        0
    } else {
        format!("{frac:0<6}").parse().ok()?
    };
    let micros = whole * MICROS + fraction;
    if micros > MAX_WHOLE_UNITS * MICROS || (micros == 0 && !allow_zero) {
        return None;
    }
    Some(micros)
}

/// Micro-units as a decimal string with at least two decimals ("25.50").
pub fn format_decimal_micro(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let micros = micros.unsigned_abs();
    let unit = MICROS.unsigned_abs();
    let mut fraction = format!("{:06}", micros % unit);
    while fraction.len() > 2 && fraction.ends_with('0') {
        fraction.pop();
    }
    format!("{sign}{}.{fraction}", micros / unit)
}

/// Whether `id` is a scope or project id: 1 to 64 ASCII letters, digits, `.`, `_` or `-`.
pub fn valid_scope_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A currency code (three ASCII letters) upper-cased, or `None`.
pub fn normalize_currency(currency: &str) -> Option<String> {
    (currency.len() == 3 && currency.bytes().all(|b| b.is_ascii_alphabetic()))
        .then(|| currency.to_ascii_uppercase())
}

/// Whether `id` is a spend report id: 1 to 128 visible ASCII characters without spaces.
pub fn valid_report_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_graphic())
}

/// The bytes a seal covers: `domain`, then each field preceded by its length as eight big-endian
/// bytes, so two different splits of the same text never produce the same message.
pub fn length_prefixed_message(domain: &[u8], fields: &[&[u8]]) -> Vec<u8> {
    let mut message = domain.to_vec();
    for field in fields {
        message.extend_from_slice(&(field.len() as u64).to_be_bytes());
        message.extend_from_slice(field);
    }
    message
}
