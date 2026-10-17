# Changelog

## 0.1.0 (2026-10-17)

- New crate 0.1.0 (Orthic-Labs/rightkit#367, #369). `days_from_civil` / `civil_from_days` (Hinnant), `is_leap_year`, `days_in_month`, `weekday`, `Date::add_days`, `DateTime` with `parse_rfc3339` / `format_rfc3339` / `unix_seconds` / `from_unix`, and `format_http_date` / `parse_http_date` (RFC 7231 IMF-fixdate). std only.
- Not covered: a named-zone tz database (#367 mentions jiff, blocked on the allowlist); callers pass fixed offsets.
- Not compiled or run when written; verification is the CI run.
