# rightkit-roots 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-roots` by exact registry pin (`=0.1.0`)
with default features (there are none). It has no path, Git, workspace or patch dependency, no committed
`Cargo.lock` and no other dependency.

Exercises the root surface an app uses without a daemon (moved from `rightkit-daemon` 0.7.0): `mode::decide`
(always `Release`, and it reads nothing from its input) and `mode::roots_with` against a fake `ModeInput` whose
per-user default is a per-run suite named `Orthic Labs/shared`, so the app directory is the sibling
`Orthic Labs/<dir_name>`; `Suite::per_user_default` is absolute and ends in `Orthic Labs/shared` (resolved, never
created or written); `Suite::at` refuses a relative root; `Suite::per_run_temp`, `service_dir` and `data_dir`;
`valid_app_name`; `Suite::app_root` (inside the per-run suite, under `apps`), `lock_writer`
held once; `AppRoot::migrate_from` with `Migrated`, `AlreadyMigrated` and `NothingToMigrate`, and
`migration_state`; `lock::try_lock_exclusive` taken once and refused while held; `RootCutover::status` before any
cutover and `CUTOVER_RECORD`; and that the `app_dirs` and `StateDir` re-exports name the `rightkit-fs` items.

Not covered: the root cutover run, recovery and retirement (the `rightkit-daemon` recipe, PK07, runs them through
the daemon's re-exports), migration options (links, specials, reparse points), Windows reparse points, and the
`app_dirs` resolver itself.

State: per-run private temp suites under the OS temp directory, removed on drop. The real per-user root is only
resolved, never created, read or written. No network, no real user data.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check panics and exits
non-zero.
