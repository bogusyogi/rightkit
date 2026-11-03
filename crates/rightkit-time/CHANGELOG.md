# Changelog

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- `zones`: local wall time to UTC for UTC, Asia/Kolkata, Europe/London and America/New_York (DST tables for 2026 and 2027), with fold handling for ambiguous times and `parse_instant` / `format_instant` / `now_secs`. Moved from PostRight (engine calendar_time.rs).
- `stamp`: system-clock UTC stamps (`rfc3339_secs`, `rfc3339_millis`, `rfc3339_precise`, `compact`). Moved from MailRight (utc_stamp.rs).
- Existing API unchanged.

## 0.1.0 (2026-10-17)

- New crate 0.1.0 (Orthic-Labs/rightkit#367, #369). `days_from_civil` / `civil_from_days` (Hinnant), `is_leap_year`, `days_in_month`, `weekday`, `Date::add_days`, `DateTime` with `parse_rfc3339` / `format_rfc3339` / `unix_seconds` / `from_unix`, and `format_http_date` / `parse_http_date` (RFC 7231 IMF-fixdate). std only.
- Not covered: a named-zone tz database (#367 mentions jiff, blocked on the allowlist); callers pass fixed offsets.
- Not compiled or run when written; verification is the CI run.
