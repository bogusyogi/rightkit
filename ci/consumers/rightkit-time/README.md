# rightkit-time registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-time` to the exact version in its
`Cargo.toml` with default features (the crate has none and no dependencies). It is built from the
sealed archive through the offline directory registry. Nothing in it is platform-gated, so Windows,
macOS and Linux run the same assertions.

It proves the whole public surface with fixed values and no clock:

- Civil dates: `days_from_civil` and `civil_from_days` (epoch, pre-epoch, a leap day, and a round
  trip over a wide sweep), `is_leap_year`, `days_in_month` (0 for an invalid month), `weekday`, and
  the `Date` type (`new` refuses impossible dates, `add_days` crosses year boundaries, `from_days`,
  `days_since_epoch`, ordering).
- RFC 3339: `DateTime::parse_rfc3339` (fractions, numeric offsets, lowercase `t` and `z`, second 60
  accepted as written, digits beyond nanoseconds dropped), `format_rfc3339` (fraction trimming, `Z`
  for offset 0, an inexpressible offset normalised to the same instant), `unix_seconds`,
  `from_unix`, `valid_offset_secs`, a parse and format round trip over an instant and offset grid
  covering years 1 to 9999, and a list of malformed inputs that must be refused.
- HTTP dates: `format_http_date` and `parse_http_date` for the IMF-fixdate, including a wrong
  weekday, an impossible day, an out-of-range hour and the obsolete forms, which return `None`.

It does not prove named time zones or daylight saving (the crate has none and callers pass a fixed
offset), the system clock (the crate never reads it), or anything outside the public API.

State: in-memory values only. No network, no files, no process spawn, no clock, no user data. Run
only by the CI consumer lane.
