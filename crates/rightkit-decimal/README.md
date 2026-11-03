# rightkit-decimal

Exact decimal values and comparison with no floats, std only. Catalog capability: `decimal-compare` (declaration-only).

- `Decimal::parse(&str)`: optional sign, ASCII digits, at most one point (`-12.50`, `.5`, `7.`). No exponent, no thousands separators, no currency sign; read those first and pass the canonical string (`rightkit-content-proof` does that for claim numbers). Input is capped at `MAX_DIGITS` (1024).
- `Decimal` is normalised (no leading or trailing zeros, zero never negative), `Ord`, and `Display`s its canonical text, so `1.0 == 1` and `-0 == 0`.
- `Comparison` (`Lt`, `Le`, `Eq`, `Ge`, `Gt`; `Comparison::parse` reads `lt`, `<=`, `==` and so on) and `compare(left, op, right)` / `ordering(left, right)` over decimal strings: the metric-threshold check engagement triggers need (PostRight #439, rightkit#1498).

## Not here

Arithmetic, rounding, currency and locale parsing, exponent notation.
