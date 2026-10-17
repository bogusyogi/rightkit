# Changelog

## 0.1.0 - unreleased

- New crate (Orthic-Labs/rightkit#301, PostRight RK-C1; second consumer: GenRight, which reserves against daily and monthly caps and a per-device offline allowance, `crates/genright-e2e/tests/paid_spend_allowance.rs`). `Ledger` (`attach`, `attach_with_clock`, `set_allowance`, `allowance`, `reserve`, `mark_in_flight`, `commit`, `release`, `expire_stale`, `acknowledge_overrun`, `report`, `reservation`), `Allowance`, `Reservation`, `ReservationState`, `Outcome`, `ExpireReport`, `Report`, `RouteTotal`, `SpendError`.
- Behaviour follows PostRight's engine spend ledger (`docs/SPEND-LEDGER.md`) apart from these differences: allowances are keyed by an opaque `(scope, period)` instead of venture, channel and month; the ledger holds no price rows and stores the route and price version it is given as opaque text; no currency is stored; `commit` with the same actual, `release` of a released reservation and a repeated `mark_in_flight` return the stored state instead of `invalid_transition`; `report().open_micro` counts `reserved` and `in_flight` estimates.
- Tables are `spend_allowances`, `spend_reservations`, `spend_overrun_acks`; the schema version is in `rightkit_spend_meta`.
- Not compiled or run when written; verification is the CI run.
