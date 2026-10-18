# rightkit-spend

A spend reservation, allowance and compare-and-swap ledger on SQLite. It records what a paid call may spend before the call and what it actually cost after, so unattended runs stop before a call that would go over budget.

Catalog capability: `spend-ledger` (to be added by the lead). Not yet published (0.2.0, unreleased). Ported in behaviour from PostRight's `crates/engine/src/spend.rs` (`docs/SPEND-LEDGER.md`); PostRight migrates its rows to this crate. Price rows are not part of this ledger: they belong to the `rightkit-llm` price catalogue (#302).

- `Ledger::attach(conn)`: create or upgrade the `spend_*` tables in the app's own SQLite file (migrations through `rightkit-db`, schema version in the table `rightkit_spend_meta`, so the app's `user_version` is untouched). `attach_with_clock` takes a unix-seconds clock for tests.
- `set_allowance(scope, period, limit_micro, expected_revision)`: create at revision 1, or change the limit under a compare-and-swap on the revision. The limit cannot drop below reserved plus committed.
- `reserve(scope, request_id, period, route_id, price_version, estimate_micro)`: hold the estimate. Idempotent on `request_id`: a repeat returns the stored reservation and writes nothing; a repeat with a different scope, period, route, price version or estimate is `RequestMismatch`. An estimate equal to the remaining amount fits.
- `mark_in_flight(id)`, `commit(id, actual_micro)`, `release(id, reason)`: the reservation life. Retrying a `mark_in_flight`, a `commit` with the same actual, or a `release` is safe and returns the stored state.
- `expire_stale(scope, older_than_secs)`: a stale `reserved` lease expires and returns its estimate; a stale `in_flight` lease commits at its ceiling (the estimate) and is flagged, never released, because the request may have been sent.
- `acknowledge_overrun(id, note)`: an actual above the estimate (or a ceiling commit) is flagged. While any flagged reservation of an allowance has no acknowledgement, `reserve` on it fails with `OverrunUnacknowledged`.
- `report(scope, period)`: totals, counts by state, forecast, measured, variance, per-route totals, and whether the reservations reconcile with the allowance row.

Money is an integer count of micro-units of one currency. An allowance holds no currency; a reservation can carry the currency text given to `reserve_with`, fixed at reserve time. Every sum is checked; overflow is `SpendError::Overflow`. No float is used.

## Account, billed-unknown, caller transactions, strict mode

- `reserve_with(.., &ReserveOptions::with_account(..).currency(..))`: store an optional account key and currency on the reservation (both are part of the request-id idempotency check). `expire_stale_account(scope, account, secs)` settles one account's stale leases; `expire_stale` takes the whole scope.
- `commit_unknown(id, reason)`: commit at the ceiling in one swap, flagged `billed_unknown` and overrun, so `reserve` refuses until `acknowledge_overrun`. An expired in-flight lease is flagged the same way.
- `begin()` plus the `_in` variants (`reserve_in`, `commit_in`, `commit_unknown_in`, `release_in`, `mark_in_flight_in`, `expire_stale_in`, `expire_stale_account_in`, `acknowledge_overrun_in`, `set_allowance_in`, and the ones below). A caller that holds only a `&Connection` (a job-queue store hook, for example) uses the `_on` variants on a connection that already has a transaction open; in autocommit mode they return `NoActiveTransaction`: run ledger writes inside a transaction you own, next to your own rows; commit once, or drop it to roll everything back. A failed `_in` call undoes its own partial writes.
- `with_strict(true)`: a repeated `release`, `commit`, `commit_unknown` or `mark_in_flight` is `InvalidTransition` instead of a replay.

```rust
let tx = ledger.begin()?;
let held = ledger.reserve_in(&tx, "genright", "job-42", "2026-10", "fal-ai/flux", "v3", 120_000, &ReserveOptions::with_account("acct-1").currency("USD"))?;
tx.execute("INSERT INTO jobs (id, reservation) VALUES (?1, ?2)", ["job-42", &held.reservation.reservation_id])?;
tx.commit()?; // the reservation and the jobs row land together
```

## Allowance keys

An allowance is keyed by an opaque `scope` and an opaque `period` string, so a daily cap (`2026-10-17`), a monthly cap (`2026-10`) and a per-device offline allowance (`device:dev_abc:1`) are all allowances of the same kind.

## Several caps, carry-over, device allowance

- `reserve_multi(request_id, &[Cap], route, price_version, estimate, &opts)`: one estimate against a daily, a monthly and a device cap under one request id, all or nothing. A refusing cap is `CapRefused { scope, period, cause }` and nothing is written on any cap. Legs are ordinary reservations (`<request_id>#0`, `#1`, ...); `mark_in_flight_group`, `commit_group`, `release_group` settle them together; `group(request_id)` reads them back. A repeat of the same request is a replay.
- `ReserveOptions::carry_over(true)` / `Cap::carry_over(true)`: open reservations of earlier periods of the scope count against this period's cap (a reservation that outlives midnight keeps counting against the daily cap until settled). Periods compare as text and only against periods of the same length, so keep one kind of period fixed-width and chronologically sortable.
- `set_device_allowance(scope, device_id, Some(limit) | None, note)`: every set or clear starts a new epoch. A set creates the allowance `device:<id>:<epoch>` with nothing counted, so spend before the reset stops counting; the old epoch keeps its own totals and its open reservations settle there. `Cap::device(scope, device_id)` follows the current epoch; a device with no allowance set takes no leg.

```rust
ledger.set_allowance("genright", "2026-10-17", 5_000_000, None)?;
ledger.set_allowance("genright", "2026-10", 50_000_000, None)?;
ledger.set_device_allowance("genright", "dev_abc", Some(2_000_000), Some("refill"))?;
let caps = [
    Cap::allowance("genright", "2026-10-17").carry_over(true),
    Cap::allowance("genright", "2026-10"),
    Cap::device("genright", "dev_abc"),
];
let held = ledger.reserve_multi("job-42", &caps, "fal-ai/flux", "v3", 120_000, &ReserveOptions::default())?;
ledger.commit_group("job-42", 118_000)?;
```

## Budgets

`Budget::new(key, limit_micro)` bounds the spend counted against a key: open estimates plus committed actuals of the reservations tagged with it. `ReserveOptions::caller_budget(budget)` is one request's budget (the key is usually the job); `Ledger::set_process_budget(Some(budget))` bounds every reservation made through that ledger value, so the jobs of one run share it. A budget is checked after the caps and can only refuse more (`BudgetExceeded`). A multi-cap reservation counts once. `budget_spend(kind, key)` and `check_budget(..)` read it.

## Provider cost, invoices, adjustment

- `record_provider_reported(id, micro, source)`: the provider's stated cost, stored beside the ledger; no cap moves.
- `adjust_actual(id, actual, source, note)`: correct a committed actual and the allowance total with it (an adjustment row is the trail; `billed_unknown` clears, the overrun flag follows the new actual).
- `reconcile_invoice(&InvoiceInput)`: record an invoice (idempotent on its reference) and match lines to reservations of the provider in the period; unmatched lines and reservations are reported, never guessed. `adjust_committed: true` also sets matched committed actuals to their invoice lines; otherwise the invoice moves no cap.

## Audit, fail-closed, overrun gate

- `cap_audit(scope, limit)`: every allowance create or limit change and device-allowance set or clear (old and new limit, revision or epoch, note, `who` from `with_actor`), written in the same transaction as the change.
- A stored limit that is not a finite non-negative number reads as 0 (the allowance refuses everything non-zero until reset through `set_allowance`). `micro_from_units(f64)` rejects `NaN`, infinities and negatives.
- `with_overrun_gate(false)`: overruns are still flagged and counted but never block `reserve`.

## Differences from GenRight's ledger

GenRight's ledger counts USD floats in a `ledger` table keyed by job and attempt, with caps in a `settings` table. This crate keeps integer micro-units and its own allowance and reservation rows, so: a cap is an allowance row (a cap change is a revisioned compare-and-swap, not a settings write); the "attempt" is a reservation; a monthly or device total is its own allowance rather than a `LIKE` over a day column; carry-over is opt-in per cap instead of built into the daily cap; GenRight's process budget was a global static and is a field of the `Ledger`; GenRight's invoice reconciliation never changed a settled amount and still does not unless `adjust_committed` or `adjust_actual` is used; GenRight's `1e-9` float tolerance is gone (integers compare exactly); an invoice period is a unix-seconds range over `created_at`, not UTC days; the device id is the caller's, not generated here.

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
