# rightkit-spend

A spend reservation, allowance and compare-and-swap ledger on SQLite. It records what a paid call may spend before the call and what it actually cost after, so unattended runs stop before a call that would go over budget.

Catalog capability: `spend-ledger` (to be added by the lead). Not yet published (0.1.1, unreleased). Ported in behaviour from PostRight's `crates/engine/src/spend.rs` (`docs/SPEND-LEDGER.md`); PostRight migrates its rows to this crate. Price rows are not part of this ledger: they belong to the `rightkit-llm` price catalogue (#302).

- `Ledger::attach(conn)`: create or upgrade the `spend_*` tables in the app's own SQLite file (migrations through `rightkit-db`, schema version in the table `rightkit_spend_meta`, so the app's `user_version` is untouched). `attach_with_clock` takes a unix-seconds clock for tests.
- `set_allowance(scope, period, limit_micro, expected_revision)`: create at revision 1, or change the limit under a compare-and-swap on the revision. The limit cannot drop below reserved plus committed.
- `reserve(scope, request_id, period, route_id, price_version, estimate_micro)`: hold the estimate. Idempotent on `request_id`: a repeat returns the stored reservation and writes nothing; a repeat with a different scope, period, route, price version or estimate is `RequestMismatch`. An estimate equal to the remaining amount fits.
- `mark_in_flight(id)`, `commit(id, actual_micro)`, `release(id, reason)`: the reservation life. Retrying a `mark_in_flight`, a `commit` with the same actual, or a `release` is safe and returns the stored state.
- `expire_stale(scope, older_than_secs)`: a stale `reserved` lease expires and returns its estimate; a stale `in_flight` lease commits at its ceiling (the estimate) and is flagged, never released, because the request may have been sent.
- `acknowledge_overrun(id, note)`: an actual above the estimate (or a ceiling commit) is flagged. While any flagged reservation of an allowance has no acknowledgement, `reserve` on it fails with `OverrunUnacknowledged`.
- `report(scope, period)`: totals, counts by state, forecast, measured, variance, per-route totals, and whether the reservations reconcile with the allowance row.

Money is an integer count of micro-units of one currency. An allowance holds no currency; a reservation can carry the currency text given to `reserve_with`, fixed at reserve time. Every sum is checked; overflow is `SpendError::Overflow`. No float is used.

## Account, billed-unknown, caller transactions, strict mode

- `reserve_with(.., &ReserveOptions { account, currency })`: store an optional account key and currency on the reservation (both are part of the request-id idempotency check). `expire_stale_account(scope, account, secs)` settles one account's stale leases; `expire_stale` takes the whole scope.
- `commit_unknown(id, reason)`: commit at the ceiling in one swap, flagged `billed_unknown` and overrun, so `reserve` refuses until `acknowledge_overrun`. An expired in-flight lease is flagged the same way.
- `begin()` plus the `_in` variants (`reserve_in`, `commit_in`, `commit_unknown_in`, `release_in`, `mark_in_flight_in`, `expire_stale_in`, `expire_stale_account_in`, `acknowledge_overrun_in`, `set_allowance_in`): run ledger writes inside a transaction you own, next to your own rows; commit once, or drop it to roll everything back. A failed `_in` call undoes its own partial writes.
- `with_strict(true)`: a repeated `release`, `commit`, `commit_unknown` or `mark_in_flight` is `InvalidTransition` instead of a replay.

```rust
let tx = ledger.begin()?;
let held = ledger.reserve_in(&tx, "genright", "job-42", "2026-10", "fal-ai/flux", "v3", 120_000, &ReserveOptions::with_account("acct-1").currency("USD"))?;
tx.execute("INSERT INTO jobs (id, reservation) VALUES (?1, ?2)", ["job-42", &held.reservation.reservation_id])?;
tx.commit()?; // the reservation and the jobs row land together
```

## Allowance keys

An allowance is keyed by an opaque `scope` and an opaque `period` string, so a daily cap (`2026-10-17`), a monthly cap (`2026-10`) and a per-device offline allowance (`device:dev_abc`) are all allowances of the same kind. A call that must pass several caps reserves against each one, with one request id per allowance, and releases the earlier reservations if a later one is refused. Resetting a device allowance with an audit trail is the app's job: set the limit again with the expected revision, or start a new period string, and keep its own audit rows.

## What the app keeps

Allowance amounts, the scope mapping, the estimate arithmetic (tokens times price), qualifying the price before `reserve`, who may acknowledge an overrun, audit rows, and the single-writer policy across processes. This crate serialises writers only through SQLite (`BEGIN IMMEDIATE`).

## Dependencies

`rightkit-db` (rusqlite, SQLite) and `thiserror`. The ledger owns one connection; open a second one for the app's own queries. Enable a `rightkit-db` feature such as `bundled` or a SQLCipher feature in the app, as for any other crate on `rightkit-db`.

## Usage

```rust
use rightkit_spend::{rusqlite::Connection, Ledger};

let mut ledger = Ledger::attach(Connection::open("app.db")?)?;
ledger.set_allowance("genright", "2026-10", 50_000_000, None)?;
let held = ledger.reserve("genright", "job-42", "2026-10", "fal-ai/flux", "v3", 120_000)?;
ledger.mark_in_flight(&held.reservation.reservation_id)?;
ledger.commit(&held.reservation.reservation_id, 118_000)?;
```
