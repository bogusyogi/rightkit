//! rightkit-time: civil date math, RFC 3339 and HTTP-date, checked against published vectors (RFC 3339 section 5.8
//! examples, RFC 7231 IMF-fixdate example) and round trips.
use rightkit_time::{
    civil_from_days, days_from_civil, days_in_month, format_http_date, is_leap_year, parse_http_date, weekday, Date,
    DateTime,
};
use serde_json::{json, Value};

fn ensure(cond: bool, what: &str) -> Result<(), String> {
    if cond {
        Ok(())
    } else {
        Err(format!("rightkit-time: {what}"))
    }
}

pub fn probe(_ctx: &super::Ctx) -> Result<Value, String> {
    ensure(days_from_civil(1970, 1, 1) == 0, "1970-01-01 is not day 0")?;
    ensure(days_from_civil(2000, 3, 1) == 11_017, "2000-03-01 is not day 11017")?;
    ensure(civil_from_days(-1) == (1969, 12, 31), "day -1 is not 1969-12-31")?;
    let mut z = -800_000i64;
    while z < 800_000 {
        let (y, m, d) = civil_from_days(z);
        ensure(days_from_civil(y, m, d) == z, &format!("civil round trip failed at day {z}"))?;
        z += 997;
    }
    ensure(is_leap_year(2000) && is_leap_year(2024) && !is_leap_year(1900) && !is_leap_year(2026), "leap years")?;
    ensure(days_in_month(2024, 2) == 29 && days_in_month(2026, 2) == 28, "February length")?;
    ensure(Date::new(2026, 2, 29).is_none(), "2026-02-29 accepted")?;
    let d = Date::new(2026, 12, 31).ok_or("Date::new(2026,12,31) rejected")?;
    ensure(Some(d.add_days(1)) == Date::new(2027, 1, 1), "add_days across year end")?;
    ensure(weekday(1970, 1, 1) == 4 && weekday(1994, 11, 6) == 0, "weekday known dates")?;

    let t = DateTime::parse_rfc3339("1996-12-19T16:39:57-08:00").ok_or("RFC 3339 example did not parse")?;
    ensure(t.offset_secs == -8 * 3600, "offset of the RFC 3339 example")?;
    ensure(t.unix_seconds() == 851_042_397, "unix seconds of the RFC 3339 example")?;
    ensure(t.format_rfc3339() == "1996-12-19T16:39:57-08:00", "RFC 3339 re-format")?;
    let frac = DateTime::parse_rfc3339("1985-04-12T23:20:50.52Z").ok_or("fractional example did not parse")?;
    ensure(frac.nanos == 520_000_000 && frac.format_rfc3339() == "1985-04-12T23:20:50.52Z", "fractional seconds")?;
    for bad in ["", "2026-02-29T00:00:00Z", "2026-13-01T00:00:00Z", "2026-01-01T24:00:00Z", "2026-01-01T00:00:00"] {
        ensure(DateTime::parse_rfc3339(bad).is_none(), &format!("accepted invalid RFC 3339 {bad:?}"))?;
    }
    let from = DateTime::from_unix(851_042_397, -8 * 3600).ok_or("from_unix rejected a valid offset")?;
    ensure(from.format_rfc3339() == "1996-12-19T16:39:57-08:00", "from_unix format")?;

    let http = format_http_date(784_111_777);
    ensure(http == "Sun, 06 Nov 1994 08:49:37 GMT", &format!("http date was {http:?}"))?;
    ensure(parse_http_date(&http) == Some(784_111_777), "http date parse")?;
    ensure(parse_http_date("Mon, 06 Nov 1994 08:49:37 GMT").is_none(), "inconsistent weekday accepted")?;

    // The wall clock reads back through the crate to a plausible year.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock before 1970: {e}"))?
        .as_secs() as i64;
    let n = DateTime::from_unix(now, 0).ok_or("from_unix(now) rejected")?;
    ensure(n.year >= 2026, &format!("system clock reads year {}", n.year))?;
    let n_back = DateTime::parse_rfc3339(&n.format_rfc3339()).ok_or("now did not round trip")?;
    ensure(n_back.unix_seconds() == now, "now round trip changed the instant")?;

    Ok(json!({ "rfc3339": t.format_rfc3339(), "http_date": http, "now_rfc3339": n.format_rfc3339() }))
}
