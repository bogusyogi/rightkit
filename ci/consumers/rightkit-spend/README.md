# rightkit-spend 0.2.0 registry consumer (draft, PK20)

Standalone consumer, outside the workspace, pinning `rightkit-spend =0.2.0` exactly. It exercises
the families GenRight's ledger uses: `reserve_on` inside a caller transaction with a caller budget,
`commit_on`, `record_provider_reported`, `reconcile_invoice`, and `report`. A `reserve_with` call
checks the caller budget refusal.

State: in-memory SQLite only, with `rightkit-db` `bundled` so no system SQLite is needed. The clock
is fixed. No network, no files, no real user data. Run only by the CI consumer lane.

Not covered: `reserve_multi`, device allowances, the `_in` variants, overrun acknowledgement, stale
expiry, and the file-backed ledger. PK20-2 may remove families that no consumer names, so this
recipe must be revisited if the list changes.
