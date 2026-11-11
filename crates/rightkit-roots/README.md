# rightkit-roots

Where a Right Suite app keeps its data: the one run-mode decision (`mode`), the per-user suite root (`Suite`, `.../Orthic Labs/shared`), an app's own root (`AppRoot`) with the one-time migration of a legacy data folder into it, and the crash-safe move of an app's data root (`RootCutover`).

**Use it** for every path an app's stores, logs, credentials and services come through: call `mode::init_or_exit(&APP)` first in `main`, then `mode::roots(&APP, mode)`, and open stores under the `AppRoots` it returns. Use `Suite` for suite-shared state (`Suite::resolve`, `Suite::data_dir`), `Suite::app_root` for an app's folder with its writer lock and migration, and `RootCutover` to move a data root an install already uses.

**Do not use it** for local IPC: hosting or calling a local service is `rightkit-daemon`, which takes the `Suite` built here. Do not use it for atomic writes, locks or owner-only files: those are `rightkit-fs`, which this crate builds on.

## Origin

The code moved here unchanged from `rightkit-daemon` 0.7.0 (`mode.rs`, `suite.rs`, `app_root.rs`, `root_cutover.rs`, and the exclusive lock file from `discovery.rs`, now `lock`). The 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-foundation-runtime.md, rightkit-daemon section) found that this is path and run-mode policy that apps with no daemon (ViewRight, ScrapeRight) and `rightkit-models` linked the whole local-service crate for. `rightkit-daemon` 0.8.0 depends on this crate and re-exports every moved item under its old path for one release. Every path resolves exactly as it did in `rightkit-daemon` 0.7.0.

## Roots

`Suite::resolve()` is `Suite::per_user_default()`: the account database, never the environment, in every build. It fails closed when no absolute app-data root can be determined and never falls back to the working directory; `Suite::per_run_temp()` is the explicit alternative (a private directory under the OS temp directory, removed when the last clone drops).

| OS | Suite root | App root (`mode::roots`, `Suite::app_root`) |
|---|---|---|
| macOS | `<home>/Library/Application Support/Orthic Labs/shared`, `<home>` from `getpwuid_r(geteuid())` | `<home>/Library/Application Support/Orthic Labs/<dir_name>` |
| Linux | `<home>/.local/share/Orthic Labs/shared` (`XDG_DATA_HOME` ignored) | `<home>/.local/share/Orthic Labs/<dir_name>` |
| Windows | `<profile>\AppData\Local\Orthic Labs\shared`, `<profile>` from `GetUserProfileDirectoryW` on the process token | `<profile>\AppData\Local\Orthic Labs\<dir_name>` |
| other | refused (`SuiteError::NoRoot`) | refused |

`<dir_name>` is `mode::AppId::dir_name` for `mode::roots`, and the name passed to `Suite::app_root`. A suite root that is not the canonical `.../Orthic Labs/shared` (an explicit `Suite::at` or a per-run temp suite) keeps its app roots inside itself, under `<root>/apps`.

## No test mode

There is no test mode, no `test-mode` feature, no `Mode::Test`, no `test_selector!` and no override selector (since rightkit-daemon 0.5.3, where this code lived; rightkit#1437). Adrian, 2026-11-03: "dev mode, is mine. im the fucking owner, my data. no dummy data, no fucking made up shit. real data. snapshot and restore." Every build resolves the real per-user roots from the account database and reads no environment. QA runs against the real data under the snapshot and restore of `rightkit-qa`.

## App-root migration (`AppRoot::migrate_from`)

`AppRoot::migrate_from(src)` copies an old data root into the app root (empty directories included), verifies every byte and writes the marker `.rightkit-migrated.json` last. It is strict: a symlink, junction, reparse point or special file in `src` returns `UnsupportedSource`. `migrate_from_with(src, MigrateOptions)` opts in:

* `preserve_links(true)` copies each symlink as a link and never follows it. A relative link that stays inside the tree keeps its text; one that leaves the tree is rewritten to an absolute path to its original target; an absolute or dangling link is copied as it is. If the destination cannot create a link (Windows without the symlink privilege or Developer Mode) the call fails with a retryable `AppRootError::Io` that carries a typed `LinkNotCreated { link, target, resolved, reason }`, read with `err.link_not_created()` (`err.is_retryable()` is true for it), before any marker is written; the old tree stays usable and the app decides whether to start from it. `optional_links(|path| ...)` lets the caller mark links that may be omitted instead; they are reported as `omitted_optional_links`.
* `skip_ephemeral_specials(true)` skips sockets and FIFOs (never opened; on Windows an `AF_UNIX` socket file counts as a socket) and reports them. Devices and other specials still refuse.
* `skip_reparse_points(true)` (Windows) skips a junction or any other reparse point that is not a symlink or socket, and reports it. Such an entry is never followed, opened, copied, recreated or recursed into; without this option it refuses with `UnsupportedSource` before anything is written.

`migrate_from_with` returns a `MigrationReport`: `outcome()` (the same `MigrationOutcome` that `migrate_from` returns), `state()`, `migrated_links()`, `skipped_specials()`, `omitted_optional_links()`, `skipped_reparse_points()` and `complete()`. The old tree is never modified.

**Completion states.** `.rightkit-migration-state.json` (version 2) records `Ready`, `ReadyWithOmissions` or `Incomplete`; read it with `AppRoot::migration_state(src)`. `Incomplete` is written before the destination is first touched and stays after any interruption or failure: the old root stays the authority and a rerun finishes the copy. `ReadyWithOmissions` means every required entry is in place and only entries the caller declared disposable (optional links, skipped reparse points) are missing, each listed; a rerun retries the omitted links only. The old-shape marker `.rightkit-migrated.json` is written only for `Ready`, so a reader of an earlier version never takes an incomplete or partial destination for a finished move.

## Root cutover (`RootCutover`)

`RootCutover::new(app_root, old_root)` moves an app's data root with the writers stopped and the old root left complete. The app registers participants (`CutoverParticipant`: `prepare`, `quiesce`, `relocate`, `reopen`, `settle`; the database relocation of `rightkit-db` is one, wrapped by the app) and picks how files travel (`FsMove::Tree` or `FsMove::Companions`). The persisted record `.rightkit-root-cutover.json` selects the root: `copying` and `aborted` the old one, `switched` and `completed` the new one. Read it with `RootCutover::status` or `selected_root`, and run `recover()` at startup so a switched cutover finishes before any root is opened. Cleanup is separate (`retire_old_root`): identity-checked, and it only renames the old root aside or sends it to the trash. Detail and the protocol table are in the module docs.

## Three root implementations

Root resolution exists three times. This crate holds one and re-exports the other two from `rightkit-fs` (`rightkit_roots::app_dirs`, `rightkit_roots::StateDir`) so they sit side by side; they are not merged yet, and an app that moves from one to another today can land in a different folder. Where they overlap, for the change that unifies them:

1. **Per-user base folder.** `Suite::per_user_default` uses `<profile>\AppData\Local` on Windows, with the profile from `GetUserProfileDirectoryW` on the process token. `AppDirs::resolve` uses the Roaming AppData known folder for `data` and `config` and Local AppData for `local` and `cache`, both from `SHGetKnownFolderPath`, which this crate deliberately avoids because it expands `%USERPROFILE%` from the environment (see `suite.rs`). On Windows `mode::roots(..).app_dir` and `AppDirs::data` are therefore different folders (Local and Roaming). macOS agrees (`~/Library/Application Support`); Linux agrees for data (`~/.local/share`, both ignoring `XDG_DATA_HOME` in the process-reading resolvers).
2. **Account home lookup.** Two `getpwuid_r(geteuid())` implementations: `suite::passwd_home_dir` (hand-declared `struct passwd`, no libc dependency) and `rightkit_fs::app_dirs::account_home` (through `libc`). Windows: `GetUserProfileDirectoryW` here, the Profile known folder there. `rightkit_fs::profile_home` is a fourth, environment-honouring variant for secondary-profile shells.
3. **Organisation folder.** `SUITE_DIR_NAME` here and `rightkit_fs::app_dirs::DEFAULT_ORG` there are both `"Orthic Labs"`, defined twice.
4. **App identity and folder.** `mode::AppId { id, dir_name }` gives `<base>/Orthic Labs/<dir_name>`; `rightkit_fs::app_dirs::AppId` gives `<base>/<org>/<namespace>` with an optional `Dev` suffix (`Options::dev`), legacy names (`with_legacy_names`), a flat layout (`without_org`) and a separate cache folder. This crate has no dev namespace, no legacy-name lookup and no cache folder; `AppRoot::migrate_from` and `RootCutover` move data instead of looking for it under an old name.
5. **Owner-only directories.** `make_private_dir` (Suite and app roots: `rightkit_fs::private::ensure_private_dir` then `verify_owner_only`, with a foreign owner refused as `Insecure`) and `StateDir::create` (descriptor-pinned, `O_NOFOLLOW` walk, owner and mode checked by `fstat`) both create and verify a private directory, by different means. `StateDir` resolves no root itself: it opens a path the caller resolved with one of the other two.
6. **No test mode, fail closed.** Both resolvers refuse a relative or missing base and read no override variable (`AppDirs::resolve_with` takes an explicit environment for tests; `Suite::at` and `Suite::per_run_temp` are this crate's explicit alternatives).
7. **Exclusive lock files.** `lock::try_lock_exclusive` (a `flock`, or a no-share handle on Windows; the app-root writer lock, the cutover lock, and `rightkit-daemon`'s election and spawn claim) beside `rightkit_fs::lock` (POSIX record locks). Not a root resolver, but the same kind of duplication.

## Platform support

macOS and Linux resolve roots from the account database; Windows from the process token. Other targets compile and refuse to resolve a per-user root.

## License

MIT OR Apache-2.0.
