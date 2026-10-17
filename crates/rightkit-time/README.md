# rightkit-time

Civil date math, RFC 3339 and HTTP-date formatting with no dependencies. Not yet published (0.1.0). Catalog capability: `time-civil` (to be added by the lead).

- `days_from_civil(y, m, d) -> i64` and `civil_from_days(z) -> (i64, u32, u32)`: days since 1970-01-01 (Howard Hinnant's algorithms), proleptic Gregorian.
- `is_leap_year`, `days_in_month`, `weekday` (0 = Sunday), `Date::add_days`.
- `DateTime { year, month, day, hour, minute, second, nanos, offset_secs }`: `parse_rfc3339(&str)`, `format_rfc3339()`, `unix_seconds()`, `from_unix(secs, offset_secs)`. Offsets are fixed (`Z` or `+hh:mm`); second 60 is accepted when written and not normalised.
- `format_http_date(unix_secs) -> String` and `parse_http_date(&str) -> Option<i64>`: IMF-fixdate (`Sun, 06 Nov 1994 08:49:37 GMT`), for Retry-After and mock responses.

## Not here

Named time zones and DST rules (no tz database): callers pass a fixed offset. Adding a tz owner needs an allowlisted dependency decision.
