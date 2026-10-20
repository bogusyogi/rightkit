# 01 — RightKit package implementation, qualification and publication packet

Date: 2026-10-20. Author: RK Fable (Dell). Owner of this file only; `README.md` (index), `review.md` and `02..07-<app>.md` are Astra's.
Canonical spec: `docs/plans/2026-10-20-rightkit-spec-adjudication.md` at commit `5ce6e2c32843edd28b003162f31f50f6cce0d9b5` (SHA-256 `f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49`), with Astra r1 K01–K16 (`docs/plans/2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md`) as the normative target contracts and Fable r1 rows (`docs/plans/2026-10-20-rightkit-spec-lock/fable-r1.md`, with its errata in the adjudication record) as structure. Nothing here reopens design. This packet is a plan; it changes no code, pin, build, publication or commit by itself.

Executor: one RK integration agent (Dell, with the Mac for npm/Swift steps) plus bounded edit-only child workers. App packets `02..07` depend on the handoff manifest in §6; each app task starts when the receipts for its declared PK subset exist (no global barrier).

Baseline facts used (working tree at HEAD `5a5892a9`, dirty with the staged candidates; registry = `packages/release/rightkit-versions.json` `registry` section; archives under `D:/scratch/rk-pub/crates-m` and `D:/scratch/rk-spec-adjudication-20261020`):

| Package | Published | Source | Status of source delta |
|---|---|---|---|
| rightkit-fs | 0.4.4 | 0.4.5 | staged: access-denied retry, publish_dir policy, lease guard fix (7 new pub items) |
| rightkit-child-process | 0.5.2 | 0.5.3 | staged: run_command, run_streaming, breakaway builder, capture fix (22 items) |
| rightkit-logs / @rightkit/logs | 0.3.3 / 0.1.5 | 0.3.4 / 0.1.6 | staged: DetachedLog, KnownSecrets public / recorder, crash formatter |
| rightkit-crash | 0.3.3 | 0.3.4 | staged: KnownSecrets redaction; floor logs 0.3.4 |
| rightkit-http | 0.3.6 | 0.3.7 | staged: fractional Retry-Reset parsing (0 new pub items) |
| rightkit-credentials | 0.3.0 | 0.3.1 | staged: rotation_status, HelperTrust/new_trusted, resolve_bundled_login_item (11 items) |
| rightkit-license / @rightkit/license | 0.2.3 / 0.2.1 | 0.2.4 / 0.3.0 | staged: `client`, `update-auth`, `client-testing` (165 items) / native view client (BREAKING) |
| rightkit-device | 0.3.1 | 0.3.1 | published; no delta |
| rightkit-daemon | 0.3.0 | 0.3.1 | staged: migrate_from_with + report types (22 items); HELD on known defects |
| rightkit-job-queue | 0.4.0 | 0.4.1 | staged: handshake, activity registry, cross-process FIFO, JobQuery (52 items) |
| rightkit-tauri-shell / @rightkit/tauri-shell | 0.4.0 / 0.1.1 | 0.5.0 / 0.1.2 | staged: 9 new modules (associations, caller_gate, logs_commands, open_paths, os_settings, preview, print, webview2_env, webview2_menu) + menu/WindowSpec additions (207 items) / adapter repair |
| @rightkit/tauri | 0.2.3 | 0.3.0 | staged: licence client removal (BREAKING), dialog-qa |
| @rightkit/app-update | 0.4.0 | 0.4.1 | staged: `./native`, createPersistedCheckCache |
| @rightkit/settings-ui | 0.6.1 | 0.7.0 | staged: LicenseView migration (BREAKING) |
| @rightkit/react-shell | 0.7.1 | 0.7.2 | staged: FuseCurves orientation |
| rightkit-sandbox | 0.5.0 | 0.5.1 | staged: Linux roots choice, ConPTY knobs |
| rightkit-models | 0.4.4 | 0.5.0 | staged: suite tests isolated; floors daemon 0.3.0, qa 0.7.0 |
| rightkit-browser | 0.3.4 | 0.3.5 | staged: cookies::parse_netscape, import_cookies, ManagedRoot re-export |
| rightkit-automation | 0.5.0 | 0.5.1 | staged: busy UIA lane refusal |
| rightkit-agent-tools | 0.2.0 | 0.2.1 | staged: camelCase exposure hardening |
| @rightkit/qa-chrome / @rightkit/qa | 0.1.8 / 0.5.1 | 0.1.9 / 0.5.1 | staged: close budget / none (dep bump needed when qa-chrome publishes) |
| @rightkit/release | 0.4.0 | 0.4.1 | staged: production-mode frontend build, dev-condition refusal, license-testing markers, Windows `publish cargo --local`; unreleased: iOS recipe, #778 updater signing, disk hygiene |
| rightkit-qa | 0.7.1 | 0.7.1 (tree) | tree carries unpublished `dialog`, `fake_cli`, `ui_dialog_*` additions under the published number (`src/lib.rs:48,56`); PK14-1 bumps the tree version before any publish |
| rightkit-oauth | 0.9.0 | 0.9.1 | staged delta not owned by any PK: held (RV08) unless the daemon `remote`/loopback rows (PK06-5) or PK10-3 name a consumer, then qualified with those rows |
| rightkit-db, rightkit-app-settings, rightkit-sidecar, @rightkit/platform-ui, @rightkit/theme, @rightkit/legal, @rightkit/legal-ui, @rightkit/onboarding, rightkit-overlay | = source | = source | published; CHANGELOG "staging" labels stale (relabel only) |
| RightKitSwift | tag 0.8.1 | 0.9.0 | held; `swift test` 835 pass 2026-10-18 on macOS |

## 1. Program rules (apply to every PK)

R1. **Source qualification before publication.** For each crate: the crate's `ci/gate.config.json` entry (every `featureSets` and `checkFeatureSets` row) runs green on the publishing host with zero failures and no skipped evidence; `cargo fmt --check` per crate; `cargo check` of the crate's affected dependency and consumer closure (the crate, every workspace crate that depends on it, and every app feature the PK names); the exact `featureSets`/`checkFeatureSets` combinations from the gate config plus every new consumer feature the PK adds; secret scan. A failure outside that closure does not fail this crate's qualification. (AH05) For npm: the package's own test script plus `publication-gate`. For Swift: `swift test` on macOS plus the generic iOS/watchOS builds already recorded.
R2. **Standalone consumer qualification, twice.** (a) Before upload: `cargo package` (or `npm pack`) produces the exact artifact; a scratch consumer outside the workspace (no `path`, no `git`, no `[patch]`, no `workspace:*`) resolves that packed artifact through a local directory registry / packed tarball and compiles and runs the consumer-facing example named in the PK, so portability defects surface before the irreversible upload. (b) After `right-release publish` readback: the same scratch consumer resolves the exact published version from the registry and repeats the example. A failure in (b) means the number is spent: publish the fix as the next number, never yank-and-reuse. For an existing published release that already satisfies a capability (e.g. device 0.3.1), there is no upload: (a) is `not-applicable` with the reason "unchanged published bytes", and (b) runs against the fetched archive in a `qualify-existing` event (§6). (AH03)
R3. **No path or Git dependencies in Cargo or npm manifests** leave the repository (RightKitSwift is the exception by design: SwiftPM consumers bind the repository URL plus the exact published tag, never a branch, revision or local path): every intra-repo `rightkit-*` dependency must carry a `version` (they do today) and the published manifest is checked by `cargo package --list` + unpacked Cargo.toml inspection in R2.
R4. **Stop on affected failure only.** A failing gate blocks that crate and its dependents in the DAG (§2); unrelated lanes continue. No "qualify all staged" step: each PK lists its own tasks and tests.
R5. **No republish, no downgrade.** A number present in the registry is final (device 0.3.1, qa 0.7.1, legal-ui 0.4.1, onboarding 0.6.2, db 0.5.0, app-settings 0.2.4, platform-ui 0.4.0, theme 0.3.1, legal 0.6.1, sidecar 0.3.0, job-queue 0.4.0, daemon 0.3.0, credentials 0.3.0). Their CHANGELOG "unpublished staging" headings are relabelled to the published date in the next publish of that crate; any defect found gets the next number. Final version numbers are not chosen here. Every number this packet attaches to a candidate or publication (staged versions in the baseline table, "0.5.3", "tag 0.9.0", "logs 0.3.4" and the like) is a baseline label for the staged content, never a reserved release number. At publish time the executor selects the number by this rule: the semver category comes from the actual change set being published (all current packages are pre-1.0, so compatible fixes and additions take the next patch and breaking changes take the next minor with a BREAKING note; at 1.0 or above, standard semver), then the live lookup in §2 collision handling confirms the number is free, otherwise the next free number in that category is taken. Where a PK says "patch" or "minor" it states the expected category only; the actual change set decides. The readback value is what the handoff manifest (§6) records.
R6. **Publication host and command.** Crates: `right-release publish cargo --crate <name> --local` on the Mac under the committed `.rightkit-local-build.json` grant (packages/release/README.md:167-169). Windows `publish cargo --local` is in the @rightkit/release tree (`local-publish.mjs:3,32-35`, "crates only", win32-only receipt) but the Dell's installed `right-release --version` reads 0.2.165 on 2026-10-20 (pnpm global), which predates 0.4.0 and every gate this packet relies on; the executor first installs the published 0.4.0 (or the next published version) on both hosts and records the version in the handoff. If the installed version lacks Windows local publish, either publish from the Mac or run the source-qualified tree copy of `@rightkit/release` for RightKit's own publishing (RV02: that is distinct from the published-only dependency apps use) and record which in the handoff. npm: `right-release publish npm --package @rightkit/<name> --local` on the Mac only (Keychain token; README:169). Swift: `right-release publish swift` tags from the Mac. Registry readback (`rightkit-versions.json` registry section) is written by the publisher and is the handoff value.
R7. **Runtime budget.** `ci/runtime-budget-deferred.json` (approved 2026-10-14) covers only the P-g renamed-dependency republish; it is not extended to any behaviour this packet adds. Every PK that publishes a budgeted crate (daemon, job-queue, tauri-shell, logs, crash, qa, agent-tools; `continuous = "held-resource"` for browser, sandbox, agent-tools) measures the published features on the row's required hosts (W and M unless the crate's gate `os` list says otherwise) and records it in `crates/<name>/runtime-budget.json` before the PK contract is marked satisfied. Existing records (daemon 0.2.7, job-queue 0.3.2, tauri-shell 0.3.2, logs 0.3.2, qa 0.6.4) do not carry to a new version. `continuous = false` crates need none. (RV03)
R8. **Independent review.** Each PK's diff gets the second reviewer's (Astra) sign-off under the 2026-10-19 Fable+Astra process before its publish step; no additional Oracle pass per diff (D14).
R9. **Forbidden in every PK** (locked): a general host/adapter framework or codegen without duplicated-schema evidence; any renderer secret getter/export; a second UI watchdog loop; a forced native API-key entry redesign (D6 keeps write-only slots in settings; OS-authentication UI required by PK08-7 is not affected); `releaseSwitches.remoteHost: true` on a desktop app; silent job breakaway; DevTools in any build except ViewRight content-only; a test trust loader; Cargo/npm path, git or patch dependencies (Swift: exact remote tag only).
R10. **Edit discipline.** Workers edit only the files named in their PK task; the integration agent stages named files only, never directories; every commit lists the PK id; pushes only after R1 on both hosts for the touched crates. **Push gate (Adrian's standing rule, unchanged; authority: his 2026-10-17 instruction, recorded on the Dell at `C:/Users/adrds/.claude/projects/D--Claude-rightkit/memory/local-gate-before-every-push.md`; not yet in the repository):** before every push of a coding batch (docs-only pushes are not covered), on the Dell through the broker: `node scripts/check-syntax.mjs`; `cargo fmt -p <crate>` for every changed crate; `cargo check --workspace --tests --locked`; `cargo check -p <crate> --all-features --tests --locked` for each changed crate (rightkit-db excluded from `--all-features` because its sqlcipher features are mutually exclusive; use its gate rows). An unrelated workspace failure blocks the push, not the crate's qualification: record the failure with its owning crate, tell that crate's owner, and wait for an authorized resolution; the candidate keeps its qualification record and only its push waits. Staging changes do not change the working-tree check, so never unstage or edit unrelated work to get past it. (AH05)

## 2. Dependency DAG and publishing order

Edges from `crates/*/Cargo.toml` `[dependencies]` and target dependencies, read 2026-10-20 (`A ─> B` means B depends on A; opt = optional; dev-deps excluded, e.g. daemon's `rightkit-http` is a dev-dependency only). Published floors are at or below current registry except the two marked ↑.

```
fs ─> db ─(opt)─> app-settings ─(opt)─> credentials ─(opt)─> license ─> device
fs ─> app-settings;  credentials ─(opt)─> device, oauth;  http ─(opt)─> license
fs ─> logs ─> crash          (crash floor logs 0.3.4 ↑; license floor credentials 0.3.1 ↑, opt)
fs ─(opt)─> http ─> oauth ─(opt)─> daemon
fs, child-process, sidecar ─> daemon;  contracts ─(opt)─> daemon
fs, db, sidecar, child-process, crash, daemon ─(all opt)─> job-queue
fs ─(opt)─> pdf;  fs, http, daemon ─> models
child-process ─> logs, sandbox, browser, tauri-shell, qa;  child-process ─(opt)─> oauth, job-queue
daemon ─> tauri-shell, qa, agent-tools;  daemon ─(opt)─> automation
job-queue, logs ─(opt)─> tauri-shell
sandbox ─> browser;  fs, models ─(opt)─> browser;  browser ─(opt)─> qa, agent-tools
automation ─> qa;  automation ─(opt)─> agent-tools;  fs, sidecar ─> qa
```

npm: `@rightkit/logs` → `@rightkit/tauri` (dep `^0.1.4`); `@rightkit/app-update` → `@rightkit/tauri` (peer `<0.5`); `@rightkit/platform-ui` → `@rightkit/react-shell` (peer); `@rightkit/react-shell`, `@rightkit/license` (peer `>=0.3.0 <0.4`), `@rightkit/theme`, `@rightkit/logs`, `@rightkit/app-update` → `@rightkit/settings-ui`; `@rightkit/react-shell`, `@rightkit/legal-ui` (peer `>=0.4.1`) → `@rightkit/onboarding`; `@rightkit/qa-chrome` → `@rightkit/qa` (exact dep `0.1.8`).

**What the edges prove.** A floor at or below the published number proves only that the resolver can satisfy it, not that the new contract is available. When a PK's new code consumes a new API of a dependency, that PK raises the floor to the version carrying it (today only crash→logs 0.3.4 and license→credentials 0.3.1 are raised). The planned post-implementation graph is recomputed from the manifests after each PK's code lands, not assumed here. (RV04)

**Planned new edges (provisional; each created only by the PK named, optional and feature-gated; any other new edge needs a review note):** db ─(opt)─> credentials adapter feature (PK03; credentials implements db's `ProtectedKeyStore`); app-settings, license, crash ─(opt)─> tauri-shell host services (PK04-2, PK05-3, PK11-4); license `update-auth` ─(opt)─> tauri-shell `update` (PK13-2); overlay ─(opt)─> tauri-shell `OverlayHost` (PK08-4); credentials OS-auth ─(opt)─> tauri-shell app lock (PK08-7); rightkit-pdf ─(opt)─> rightkit-tauri-shell through an optional `pdf-host` feature (tauri-shell owns caller/window authorization, pdf owns the view lifecycle; rightkit-pdf never depends on tauri-shell) (PK08-6, FH23). Forbidden: db depending on credentials (cycle), daemon depending on db, db depending on daemon (PK03/PK07 compose through opaque participants). (RV10)

**Lanes (ordered by their internal edges; lanes run in parallel; a crossing waits for the upstream readback only when new API is consumed):**
- Leaves: fs, child-process, sidecar, contracts, overlay, ownership; sandbox above child-process only.
- Storage and secrets: fs → db → app-settings → credentials → license → device; http and credentials → oauth.
- Diagnostics: fs + child-process → logs → crash.
- Services: fs + child-process + sidecar + oauth (opt) → daemon → automation (opt), models (with http), job-queue (opt deps incl. db, crash), tauri-shell (with job-queue, logs opt).
- Tools: models + sandbox → browser; automation + browser + daemon → qa, agent-tools.
- npm: {logs, license, app-update, platform-ui, theme, legal, legal-ui, qa-chrome} leaves; {tauri, tauri-shell, react-shell} next; {settings-ui, onboarding, qa} last; release independent of all (its own qualification only). (RV10)

**Cycle guard.** rightkit-credentials → rightkit-app-settings (opt) → rightkit-db (opt) exists today; rightkit-db must therefore never depend on rightkit-credentials (Cargo rejects package cycles regardless of features). PK03 keeps the encrypted lifecycle in db behind a `ProtectedKeyStore` trait that db defines; rightkit-credentials implements it in an adapter behind its own feature (credentials depends on db optionally; db never depends on credentials); db stays the single workflow owner. Relocation: db owns the DB/key relocation primitive; daemon `RootCutover` composes filesystem moves with opaque lifecycle participants the app registers (the db primitive is one); db never depends on daemon and daemon never depends on db. Sequence: participant/trait interfaces first, then db (PK03-5), then root composition (PK07-3). (RV04, RV09)

**Readiness groups instead of waves (RV01).** There is no "publish all staged" step. Each PK publishes its owner once the PK's candidate and required work are coded and qualified together, as the next semver step; where a staged candidate is incompatible with the required work it is reshaped before publishing, never published as-is to be fixed later. An intermediate bugfix release is allowed only when it independently unblocks a named consumer and the PK says which (PK01-1 lease fix: every app's `Lease` user; PK02-1 capture fix: ScrapeRight/CodeRight runners; PK03-1 credentials 0.3.1: HeardRight helper trust; PK05-1/2 licence client + renderer view client: all apps' licence migration, published with PK04-2 host registration). App packets start when the fetched receipts (§6) of their declared PK subset exist; unrelated package work never blocks an app task.

**Publication sequence within a group** follows the edges only: a crate publishes after every dependency whose new API it consumes has its readback. http 0.3.7 has no consumer of its change and no dependent floor; it publishes with the next http change or when a consumer names it. `@rightkit/release`: `publish-cargo.mjs` runs the crate gate + `publication-gate.mjs`; the production-mode/dev-condition refusals live in `release.mjs` (app build path), so crate and npm publishes do not depend on a new release version; `@rightkit/release` ships after its own qualification, with the unreleased iOS/#778/disk-hygiene content excluded by this packet's scope (RV02). sandbox 0.5.1 and automation 0.5.1 are held unless an app packet or rightkit#821 names a consumer of the staged change (RV05).

**Collision handling.** Before choosing a version: a live registry lookup (crates.io sparse index fetch for the crate, `npm view <pkg> versions --json`, `git ls-remote --tags` for RightKitSwift) and the `rightkit-versions.json` registry section must both show the target number absent; the lookup output is stored with the handoff evidence. If present, take the next number and re-run R1. A publisher dry-run checks packaging only, never availability. No yank, no overwrite, no numbers reserved in advance. (RV10)

## 3. Package tasklist PK01–PK17

Format per PK: owner; **Published** (what exists at the registry version); **Candidate** (staged in tree; reshaped and qualified together with the PK's required work); **Required** (not coded; ships in the same readiness group unless the PK names an earlier consumer-driven release); source anchors; tasks `[ ]`; qualification (gate rows, hosts, scenarios from the K contract); publication; deletion consumers (apps, per their packet). Hosts: W = Windows (Dell), M = macOS (Mac mini), L = Linux PR check only where the crate has one, A = Android compile.

### PK01 — rightkit-fs (K01; rows M8, M6 part)

Published 0.4.4: atomic/append/state/private/lock/lease/migrate/publish_dir/replaced/handle/file_key/identity/clone/swap/trash/verified/digest/app_dirs/test_mode (archive-confirmed).
Candidate 0.4.5: `atomic_write_with_access_denied_retry`, `AtomicWriter::create_with_access_denied_retry`, `publish_file_with_access_denied_retry`, `AccessDeniedRetry`, `publish_dir_with` + `DirPublishPolicy`, lease guard rewrite (`lease.rs`, publish_guard via hard link; working-tree only). CHANGELOG 0.4.5 says its tests were not compiled or run.
Required (not coded; publishes with its PK group): `KeyedWriteLocks::with_destination(path, operation)`; `FileTransaction::prepare(root, entries, options) -> PreparedTransaction`, `PreparedTransaction::commit() -> TransactionReport {Committed|RolledBack|RecoveryRequired}`, `recover_transactions(root) -> RecoveryReport` (K01 semantics: journal before mutation, retain rollback material, no all-file atomic visibility, cross-volume stage-then-publish). Names provisional.
Source: `crates/rightkit-fs/src/{atomic.rs, lease.rs, publish_dir.rs, lock.rs, state.rs}`; tests `tests/atomic_*.rs`, `tests/lease*.rs`, `tests/publish_dir*.rs`, `tests/access_denied_retry_tests.rs` (36 files).

- [ ] PK01-1 Lease guard: verify `lease.rs` publishes a complete guard file through strict no-replace publication before acquisition (no `rename_exclusive` O_EXCL placeholder fallback, `atomic.rs:429-433` path not used); keep one stable lock inode; refuse unknown/live legacy owner; keep original OS error in the error chain. Add a deterministic two-contender test and a kill-between-write-and-publish test.
- [ ] PK01-2 Access-denied retry: keep opt-in only, one shared finite budget for Windows codes 5/32/33, no ACL change, no unbounded retry; run `tests/access_denied_retry_tests.rs` on W.
- [ ] PK01-3 `publish_dir_with` / `DirPublishPolicy` (unpublished): keep only if an app packet names a consumer and the staged tests run on W and M; otherwise remove the addition from the candidate before publishing (locked §2: unnecessary unpublished additions are removed; published APIs are never removed).
- [ ] PK01-4 Relabel stale README version text (README:11) and the 0.4.5 CHANGELOG heading at publish.
- [ ] PK01-5 (required) Implement `KeyedWriteLocks` over the existing `FileLock`/`AtomicWriter`; consumer ViewRight `documents.rs:511-520` save (replaces `destination_write_lock` + local `write_atomic`).
- [ ] PK01-6 (required) Implement `FileTransaction`/`recover_transactions` with journal under the root, identity checks on journal/staged/backup, deterministic resume; consumer MailRight `data_relocation.rs` companions via PK03/PK07.
- [ ] PK01-7 (required) Fault-injection tests before/after each rename and journal flush (reuse `atomic::seams`); private-mode preservation; collision refusal; external-writer conflict.

Qualification: gate `--all-features` + alsoSerial on W and M (R1); FAT/exFAT exclusive-publish on a real volume on W once (README:60 gap) or record as still-open; `cargo doc` for the new types; R2 consumer example: `AtomicWriter::create(..).commit_checked(..)` and (required) a two-file `FileTransaction`.
Publication: next semver step once PK01-1..4 and the required items with named consumers are qualified together; the lease fix alone may ship first as a patch (named consumer: every app's `Lease` user).
Deletion consumers: ViewRight save lock/atomic helpers; ScrapeRight Save-As publication; MailRight relocation staging/journal (after PK03/PK07); every app's hand-rolled temp-and-rename, lock files, containment checks (named per app packet).

### PK02 — rightkit-child-process (K02; rows M3, M5)

Published 0.5.2 (archive-confirmed). Candidate 0.5.3: `run_command(OwnedCommand, input, &RunOptions)`, `run_streaming` + `StreamOptions`/`StreamOutput`, `WindowsJobBuilder::allow_breakaway/allow_silent_breakaway`, `WindowsJob::contain_current_process_with`, capture propagation fix. Known: `run()` builds through `command_mut()` which clears the Windows auto config, so the native-stdio fix is unreachable from `run()`; `native_capture/windows.rs:199,222` maps broken pipe to `Ok(0)`.
Source: `crates/rightkit-child-process/src/{owned.rs, run.rs, native_capture/*, windows_job.rs}`; tests `tests/native_capture.rs`, `tests/output_capture.rs`, `tests/windows_process_tree.rs`, `tests/handle_inheritance.rs` (20 files; several `.rs*` Windows-only).

- [ ] PK02-1 Capture propagation, per runner: inspect the spawn path each of `run`, `run_with_stdin` (`run.rs:131`, uses `command_mut()` → default `Command` spawn) and `run_command` (`run.rs:204`, native Windows spawn with configured stdio) actually selects; the configured path must carry env, cwd, raw args, handle allowlist and containment. A closed output pipe is EOF, not an error (`native_capture/windows.rs:199,222` map `ERROR_BROKEN_PIPE`/`PIPE_NOT_CONNECTED` to `Ok(0)` by design; an early-closed stdin is a normal child exit per `run.rs:113,183`). The defect to fix is a non-EOF I/O error on a stream being swallowed: such errors propagate after both streams are collected, and `Ok` never carries incomplete output. One stdio regression test per runner on W and M. (RV07)
- [ ] PK02-2 Remove the unpublished *silent* breakaway path (`allow_silent_breakaway` on caller-built/current-process jobs); keep explicit `allow_breakaway(true)` + `contain_current_process_with` for CodeRight's replacement process; default containment unchanged.
- [ ] PK02-3 `run_streaming`/`StreamOptions` (unpublished): keep only with the ScrapeRight consumer named in `06-scraperight.md` (bounded tails, cancel, deadline, tests on W and M); otherwise remove the addition from the candidate before publishing. Same rule for `run_command` (CodeRight/ScrapeRight env/cwd consumer) and the explicit breakaway builder (CodeRight replacement process).
- [ ] PK02-4 Run the Windows suites on the Dell (README "Nothing here was run on Windows" closes): `native_capture`, `windows_process_tree`, `handle_inheritance` (ignored helper allowed per gate), `drop_bounded`, `parent_death`; Unix suites on M in both thread modes.
- [ ] PK02-5 Document `kill_on_parent_exit` as a Windows no-op in the row README so apps stop writing parent-death watchdogs.

Qualification: gate default + alsoSerial on W and M; scenarios Q4: stdout closed early, stderr flood with cap, large stdin with surviving descendant, timeout then kill, nonzero exit, explicit breakaway only when requested, ordinary descendants contained.
Publication: next patch once PK02-1..5 are qualified; the capture fix may ship alone (named consumers: ScrapeRight and CodeRight runners). No further item.
Deletion consumers: ViewRight raw `Command::new` at `lib.rs:133,152,165`; CodeRight `lib.rs:942` spawn + `daemon_lifecycle.rs` ownership, `lib.rs:288,299` (git); HeardRight `src-tauri/src/commands/system.rs:175,187,217`, `commands/updates.rs:114` (`/bin/sh -c` relaunch: replace with a typed launch), `commands/files.rs:50`, `commands/diagnostics.rs:279,291,605,619`, `commands/microphone.rs:39`, `commands/catalog.rs:24`, `heardright-command-host/src/command_dispatch/power.rs`, `app_launch.rs:481`, `keys.rs:1100`; MailRight `src-tauri/src/startup_error.rs:53`; Pulse `core/src/worker.rs:261`, `core/src/context_cmd.rs:30`, `core/src/bridge/deliver_codex.rs:1259`, `core/src/main.rs:1574`; every app's kill-tree/process-table/reveal helpers. Most need only the published 0.5.2 `run`/`run_with_stdin`/`OwnedCommand` (`PK02.capture` covers its qualification). (FH20)

### PK03 — rightkit-db + rightkit-credentials (K03; rows M9, M11, D15)

Published: db 0.5.0 (DbOptions, read pool, online backup, restore_database, migrate, RecoveryPolicy, rekey, WalCheckpointJob); credentials 0.3.0 (Store, rotation begin/pending/commit_of/abort_of, migrate_plaintext_file, AppleHelper, Android bridge).
Candidate: credentials 0.3.1 (`rotation_status`, `HelperTrust`/`new_trusted`, `resolve_bundled_login_item`); db none.
Required (db minor; db defines `ProtectedKeyStore` and owns the workflow; rightkit-credentials implements it in an adapter feature; no db-to-credentials edge, see §2 cycle guard): `EncryptedStore::open(config)`, `rotate_key(request) -> RotationReport {Committed|CommittedCleanupPending|NotCommitted|Indeterminate}`, `recover_rotation() -> RecoveryReport`, backup/restore delegation with quiescence, `relocate(plan) -> RelocationReport` (db-owned DB/key relocation primitive over the PK01 transaction; the app registers it as a PK07 `RootCutover` participant; db never calls daemon); one cross-process `StoreTransition` guard (bounded acquire, drain readers/writers/pools, `Busy` before mutation, held until effects settle, old/new root identities journaled; lock order guard → credentials rotation lock → DB op). Legacy HeardRight staging account `SQLCipher/DBKey.staging` read/migrated before switching to `<account>.rotation-pending`; old key retained durably until readback proves consistency.
Source: `crates/rightkit-db/src/{lib.rs, backup.rs, restore.rs, recover.rs, migrate.rs}`; `crates/rightkit-credentials/src/{store.rs, rotation.rs:339-340, migrate.rs}`; tests db 14 files (`restore`, `unreadable_recovery`, `integrity_recovery`, `read_pool`), credentials 8 files (`rotation_process`, `windows_*`, `linux_secret_service`, `guard_release`).

- [ ] PK03-1 credentials: (a) protected store: run `windows_lock`, `windows_target`, `windows_keyring_compat` on W (README:51 says never run), the store suites on M, Linux via the PR check only; satisfiable by a `qualify-existing` event on 0.3.0 if no store change ships, else with the next publish. (b) helper trust: `HelperTrust`/`new_trusted`/`resolve_bundled_login_item` tests and `rotation_process` on W and M; publish 0.3.1.
- [ ] PK03-2 db: reconcile the 0.5.0 CHANGELOG label. The runtime-WAL-refusal retention of the requested main file and its `-wal`/`-shm`/`-journal` siblings is deliberate (CHANGELOG 0.5.0: another connection may have committed rollback-journal data; any check-then-remove could destroy it) and is NOT a defect: add only a regression test that the files are retained and the writer lands on the per-host sibling; no cleanup code. (RV06)
- [ ] PK03-3 (required) Implement `StoreTransition` guard and `EncryptedStore::open` key selection (primary, pending, mapped legacy staging) with classified failures; no automatic empty DB.
- [ ] PK03-4 (required) Implement `rotate_key`/`recover_rotation` over `rekey` + the `ProtectedKeyStore` trait (the credentials adapter maps it to the rotation API); classify outcome from readback; retain both viable keys; cleanup-only failure never rolls back.
- [ ] PK03-5 (required) Implement backup/restore quiescence and the db-owned `relocate` primitive over PK01-6 only; db takes no daemon dependency. Order: PK01-6 and the PK07 participant interface first, then PK03-5, then PK07-3 composes it. (RV09)
- [ ] PK03-6 (required) Tests: kill after each phase, primary unavailable, staging unreadable, rekey fail, promotion with cleanup failure, indeterminate readback, pool with active readers, wrong destination key, legacy staging as sole viable key; W and M, SQLCipher provider feature set per host.

Qualification: db gate `bundled,statement-cache` run + `hooks` check, plus `sqlcipher` and `sqlcipher-system` rows added to `ci/gate.config.json` for this crate (PK17-4); credentials gate default + `isolated-test-keychain` (M only for the isolated keychain) + the new db-adapter feature row (PK17-4); R2 example: open writer, `rotate_key`, reopen.
Publication: credentials next patch once PK03-1 is qualified (named consumer: HeardRight helper trust); db next minor with PK03-3..6.
Deletion consumers: HeardRight `history/key_rotation.rs::rotate_with`, `credential_store.rs` wrappers/cache; MailRight `db/data_relocation.rs` mechanics; CodeRight `token_store.rs` (incl. the "plain" fallback at :76,:96); (ViewRight raw rusqlite at `logs.rs:33` moves to the plain shared DB open of `PK03.sqlite-baseline` with root ownership from PK07, not to the encrypted lifecycle.)

### PK04 — rightkit-app-settings + @rightkit/settings-ui + rightkit-tauri-shell host registration (K04; rows M10, M1 part, D6)

Published: app-settings 0.2.4 (Store, Document, revision CAS, poll_external, SqlSettings; `atomic_write` wrapper delegating to fs, stays); settings-ui 0.6.1; tauri-shell 0.4.0 `handler!` (lib.rs:1272-1325 in tree).
Candidate: settings-ui 0.7.0 (LicenseView migration, BREAKING; peer license >=0.3.0); tauri-shell 0.5.0 `logs_commands` (not in `handler!`) and `caller_gate`.
Required (tauri-shell minor + settings-ui minor): `HostServices` registration of typed settings/logs/licence/update/models/crash services once under `HostCallerPolicy` (trusted window labels/origins per service; caller identity from native invocation context); settings bridge `describe/get/patch(expectedRevision)/subscribe/reset(keys)` over `Document`; `rightkit.secrets.presence/clear` over `Store::exists/delete`; `rightkit.secrets.set` kept as write-only for app-declared `ApiKeySlot` refs with explicit `WriteIntent`, trusted settings caller only; `StoreOptions::write_security(profile)` mapping to fs privacy; no generic credential get/export. Names provisional.
Source: `crates/rightkit-app-settings/src/{lib.rs, document.rs, atomic.rs}`; `crates/rightkit-tauri-shell/src/{lib.rs, commands.rs, caller_gate.rs, logs_commands.rs}`; `packages/settings-ui/src/{shell-adapter.ts, types.ts:61-66, sections.tsx}`; tests app-settings 13 files, tauri-shell `tests/logs_commands.rs`, settings-ui e2e `server.ts`.

- [ ] PK04-1 settings-ui candidate (0.7.0 in tree): run its tests + `/core` import-graph test; publishes only in the PK04/PK05 group.
- [ ] PK04-2 (required) Implement `HostServices`/`HostCallerPolicy` in tauri-shell over `caller_gate`; register through `handler!`; fold `logs_commands` in; register the licence (PK05), update (PK13), models and crash (`rightkit_crash_window_error`) commands through it.
- [ ] PK04-3 (required) Implement the settings bridge over `Document` with CAS, subscribers notified after commit, typed conflicts; `reset(keys)`.
- [ ] PK04-4 (required) Secrets: presence/clear/set(slot-restricted) with `WriteIntent`; negative test: content-origin caller refused; no command returns a secret.
- [ ] PK04-5 (required) `StoreOptions::write_security(profile)` → fs `Privacy`; typed failure when protection unachievable; no DACL code in settings.
- [ ] PK04-6 (required) settings-ui `/shell-adapter` METHODS target the registered names; drop any adapter method that has no host registration.

Qualification: app-settings gate (W, M, A compile); tauri-shell gate rows `[]`, `engine`, `logs` + check rows (W with `wv2-0_38` and `wv2-0_39`, M); scenarios Q2/Q5: stale revision, failed validation, newer schema, oversized, external edit, read-only backend, malicious content window, no secret response path.
Publication: settings-ui with @rightkit/license 0.3.0 and the tauri-shell host-registration minor as one group (no app can adopt either alone); app-settings next minor with PK04-5.
Deletion consumers: all six apps' settings command maps, get/save wrappers, polling loops; Pulse keeps read-only/BOM/size policy as config; MailRight keeps `SqlSettings` adoption; ScrapeRight `commands.rs:14` save/snapshot sequencing.

### PK05 — rightkit-license + rightkit-device + @rightkit/license (K05; rows M12, M13)

Published: license 0.2.3 (verify, clock, lifecycle, refresh, lease envelope, EngineEntitlements, subject-entitlement, engine-gate); device 0.3.1; @rightkit/license 0.2.1 (renderer token client, retiring).
Candidate: license 0.2.4 `client` (LicenseClient status/activate/refresh/deactivate/request_trial/lease_*/import_legacy, LicenseView, LicenseError), `update-auth` (UpdateAuthProvider, AuthorizedHeaders), `client-testing`; @rightkit/license 0.3.0 (`createLicenseClient({invoke, listen})`, LicenseView, LICENSE_MESSAGES, `./testing` dev-only). CHANGELOG 0.2.4: "not compiled when written".
Required (license minor): `AccountSessionClient` (request_sign_in/complete_sign_in/status/sign_out/list_devices/rename_device/remove_device/request_link/redeem_link), Pro-bridge exchange and lease transport as native typed operations, account deletion as typed destructive action with server-refusal preservation; native entitlement invalidation to live engine generations. Names provisional; HeardRight is the named consumer.
Source: `crates/rightkit-license/src/{client/*, engine.rs, refresh.rs, lifecycle.rs}`; tests `client_engine`, `client_flow`, `client_lease`, `client_race`, `engine_gate`, `subject_entitlement`, `public_vectors`, `dalek_parity` (15 files); `packages/license/src/{index.ts, testing.ts, native-parity.test.ts}`.

- [ ] PK05-1 license 0.2.4: compile and run the client tests on W and M; test vectors (license-v2, lease-envelope-v1, dalek-parity-v1) unchanged; negative tests: renderer device id refused, token > 16 KiB refused unread, rolled-back clock denies, `peek_unverified_claims` never grants.
- [ ] PK05-2 @rightkit/license 0.3.0: run `native-parity.test.ts`; verify `./testing` resolves only under `development`; publish (BREAKING note). An app task that adopts the @rightkit/license 0.3.0 renderer client (and so its `./testing` subpath) also requires `PK17.release` (@rightkit/release 0.4.1 with the `development`-condition preflight and `license-testing` markers) published and pinned; native-only licence work does not (FH08).
- [ ] PK05-7 @rightkit/tauri 0.3.0 (staged, BREAKING: renderer licence client removed; `dialog-qa`): run its tests; keep `dialog-qa` only if the rightkit-qa `dialog` module ships with a named consumer (PK14-1), otherwise remove it before publish; publish in the same group as @rightkit/license 0.3.0 with a BREAKING note. Consumers: every app's licence task (they all pin @rightkit/tauri 0.2.3); `@rightkit/logs` dep and `@rightkit/app-update` peer ranges checked against it. (FH04)
- [ ] PK05-3 Command registration: the five `rightkit_license_*` commands and `rightkit://license-changed` are registered by PK04-2 (tauri-shell), not by apps; until PK04-2 lands, @rightkit/license 0.3.0 is publishable but apps may not adopt it, because no native command exists to serve it.
- [ ] PK05-4 device: no republish; add `#[non_exhaustive]` to `AuthorizationDenial` and README feature text only in the next minor when another change needs it.
- [ ] PK05-5 (required) Implement `AccountSessionClient`, Pro bridge, lease transport, link codes inside `client`; keep existing service paths/payloads as compatibility vectors; no invented endpoints.
- [ ] PK05-6 (required) Tests: wrong app/device, replayed clock, revoked, offline policies, interrupted deactivation, concurrent refresh/sign-out, old token after tombstone, legacy import, secret-store denial, updater headers never serialised.

Qualification: license gate default (allowIgnored dalek generator) with `--features client,update-auth,subject-entitlement,engine-gate` rows added (PK17-4); W and M; R2 example: `LicenseClient::new(..).status()` from a scratch consumer.
Publication: license next step with PK05-1 as part of the PK04/PK05 group; @rightkit/license 0.3.0 in the same group; licence extensions (PK05-5/6) join that same group unless they independently unblock a named AP task, which the publication note then names. (RV14)
Deletion consumers: ViewRight `commands/license.rs:124` lifecycle + `state/updateLicense.ts:190,387`; CodeRight `license.rs:35` + `updateLicense.ts:103`; MailRight `commands/license.rs:27` raw get/set/delete; HeardRight `license/lease.rs:245`, `account_session.rs:221`; ScrapeRight `license.rs:58`, `pro_bridge.rs:328`; HeardRight engine `is_pro()` replaced by `EngineEntitlements` (settings.rs:318-327).

### PK06 — rightkit-job-queue + rightkit-sidecar + engine composition (K06; rows M3, M4, D5)

Published: job-queue 0.4.0 (EngineSupervisor, EngineClient, Dispatcher, serve/run_cli, JobStore/Runner/StagedPool, ffi); sidecar 0.3.0.
Candidate: job-queue 0.4.1 (`with_handshake`/`HandshakeCall`, `InlineCancelRegistry` activity options, `FifoScope::CrossProcess`, `JobTable::query/delete_terminal`, `Dispatcher::event_emitter`, serve drain); CHANGELOG: tests "written without running Cargo".
Required (not coded; publishes with its PK group): engine host composition = tauri-shell `EngineHost`/`EngineSpec`/`EnginePolicy` (+ Swift counterpart) registering startup, ready/health, allowlist, deadlines, event forwarding, snapshots, shutdown once; daemon client lifecycle typed `Owned`/`Attached` provenance with `attach_or_launch`, `acquire_session`, `release_session`, `shutdown_owned` (attach never implies kill; identity/generation revalidated); CodeRight loopback HTTP+bearer as a **separable daemon feature** (e.g. `remote-loopback`) reusing the `remote` router/auth internals with loopback-only bind, explicit origin/CSRF policy, capability verification; `releaseSwitches.remoteHost` stays false and the scanner (`release-switch-scan.mjs:84-99`) gets its own rule for the loopback feature (PK17-2). Typed dispatch from a method enum and typed events with replay-by-seq only with HeardRight/Pulse as deletion consumers.
Source: `crates/rightkit-job-queue/src/{supervisor.rs, client.rs, contract.rs, inline.rs, store/*}`, tests 47 files (`handshake`, `inline_activity`, `event_emitter`, `job_lock_fifo`, `table_query`, `engine_contract`, `ffi`); `crates/rightkit-daemon/src/{client.rs, remote/*, server.rs}`; `crates/rightkit-tauri-shell/src/engine.rs`.

- [ ] PK06-1 job-queue 0.4.1: run the five gate rows on W and M; the staged tests actually execute; JobCursor long-key test runs.
- [ ] PK06-2 sidecar: relabel CHANGELOG/README (0.3.0 published; README says 0.1.0); no code change.
- [ ] PK06-3 (required) Engine host composition in tauri-shell `engine` feature: presence/refcount idle exit, restart generation latch, ordered shutdown, snapshot replay; one executable README example run by a test on W and M.
- [ ] PK06-4 (required) daemon `Owned`/`Attached` client lifecycle; `supervise_adopted` stays for `PrivateChild`; attached clients release sessions without killing the shared service.
- [ ] PK06-5 (required) daemon loopback feature: bounded parser, loopback bind, bearer/capability verification inside the owner, CodeRight codecs; scanner rule in PK17-2; negative test: desktop app with `remote` still refused.
- [ ] PK06-6 (required) Tests: wrong endpoint identity, recycled PID, two shells attach, owner exits, detach, replacement starts, stale-generation reply cannot complete a new request, exhausted restart budget, graceful stop then kill.

Qualification: job-queue gate rows (W, M) with runtime budget measured for each published version (R7); daemon rows `[]`, `test-mode`, plus `remote` and the new loopback feature rows (PK17-4); tauri-shell `engine` rows.
Publication: job-queue next step with PK06-1 (named consumers: HeardRight handshake, ScrapeRight activity list); daemon and tauri-shell minors with PK06-3..6.
Deletion consumers: HeardRight `engine_supervisor/supervisor.rs:412` host transport/replay; ScrapeRight `engine_supervisor.rs:230,420` host/snapshot glue; MailRight `shell_mode.rs:58` start/stop orchestration; CodeRight `lib.rs:661,942`, `daemon_lifecycle.rs`; Pulse `engine/src/resident.rs` glue; every app's hello/ready parsers, restart timers, stall watchdogs, framers.

### PK07 — rightkit-daemon roots, services, approvals (K07; rows M6, M7, M22 part)

Published 0.3.0 (Suite, AppRoot, migrate_from, discovery, host/Service, MethodTable, chunk, private child, supervisor, link/relay/remote, mode).
Candidate 0.3.1 (`migrate_from_with`, `MigrateOptions`, `MigrationReport`, `LinkNotCreated`, `is_retryable`): HELD; known defects: empty directories not migrated (dangling link to empty dir), Windows sockets and junction targets not special-cased, marker semantics for incomplete migrations.
Required (not coded; publishes with its PK group): completion states `Ready | ReadyWithOmissions | Incomplete` in a versioned marker (old marker shape only for complete moves); retry of unresolved entries on rerun; junction/reparse explicit policy; `RootCutover` composing PK01 filesystem moves with opaque lifecycle participants the app registers (the PK03 db relocation is one; stop writers, copy/verify, persist generation, switch readers/services, reopen, mark; no daemon/db edge); `approval` module (`ApprovalStore`, `ApprovalCoordinator::{request,decide,claim,settle,cancel}`, `ApprovalVerifier`; expiry, revocation, request binding, one execution claim, audit-before-success, `OutcomeUnknown` after crash); `ERROR_RATE_LIMITED` exported. Pulse P-256/Secure Enclave profile and Mail account-scoped profile preserved as declared inputs.
Source: `crates/rightkit-daemon/src/{app_root.rs, suite.rs, server.rs, contract/*, remote/core/*, mode.rs, error.rs:45}`; tests 21 files (`mode`, `supervisor`, `private_service`, `remote_*`, `e2e`).

- [ ] PK07-1 Fix the 0.3.1 migration defects (empty dirs, sockets/junctions, marker for incomplete) with tests on W and M before any daemon publish; until then daemon stays 0.3.0 and dependents keep floor 0.3.0 (no edge is forced).
- [ ] PK07-2 Export `ERROR_RATE_LIMITED`; measure runtime budget on W and M (README "not measured").
- [ ] PK07-3 (required) `RootCutover` over a participant trait (prepare, quiesce, relocate, reopen, settle), after PK03-5; Pulse `migrated_app_root()` deleted only when every root consumer is switched; old roots recoverable, cleanup a separate identity-checked step.
- [ ] PK07-4 (required) `approval` module with verifier trait; Pulse signing vectors byte-compatible; Mail decision/audit profile as data.
- [ ] PK07-5 (required) Service registration ergonomics: app provides identity, method descriptors, handlers; no public listener to satisfy M7/M22.
- [ ] PK07-6 (required) Tests: linked empty dir, required/optional link creation denied, interrupted marker, old reader on incomplete candidate, collision, path substitution; approval replay across actions/accounts, revoke, expiry, double claim, failed audit, uncertain effect.

Qualification: gate rows `[]`, `test-mode` (+ `remote`, loopback per PK17-4); peer-check negative test on W and M; R2 example: `Service::builder()..local(..)` + `Client::connect_expecting`.
Publication: next step after PK07-1/2 (named consumers: HeardRight and ScrapeRight data-folder move); next minor with PK07-3..6.
Deletion consumers: Pulse `core/src/store.rs:128,269`, `remote/approval_sig.rs` mechanics, `devicelink_host.rs` transport; MailRight `agent/gate.rs:145,229` pending/decision/audit, `data_relocation.rs` choreography; HeardRight/ScrapeRight data-folder move code; every app's `dirs`/env root resolution.

### PK08 — rightkit-tauri-shell native shell, rightkit-overlay, preview, PDF host, app lock (K08; rows M1, M16 part)

Published: tauri-shell 0.4.0 (archive list in `docs/plans/2026-10-20-rightkit-spec-lock/archive-surface-diff.md`); overlay 0.1.2 (state/placement + Windows styling; no host); rightkit-pdf 0.3.2 (not a mandatory row; owner for PDF).
Candidate: tauri-shell 0.5.0 nine modules + menu Check/Submenu/overrides + WindowSpec fields + `webview2_env`; @rightkit/tauri-shell 0.1.2. CHANGELOG 0.5.0: "Not built or run locally".
Required (not coded; publishes with its PK group): `SurfaceSpec { id, size, minimum, placement, activation, close_behavior }` + `set_surface(id)` (ScrapeRight `window_surface.rs`); `OverlayHost::{show,update,hide,dispose}` composing rightkit-overlay (work-area clamp, mixed-DPI, no-activate, topmost/fullscreen, click-through, timers, cleanup; Windows z-order walk for `covering_window`); `PreviewStore` qualified from the 0.5.0 candidate; optional `PdfHost` backend in rightkit-pdf (macOS PDFKit open/set_frame/state/find/print/suspend/close) with tauri-shell owning caller/window/resource authorization; app-lock coordinator (`lock`, `begin_unlock`, `complete_unlock`, `status`) in the shell over the PK03 credentials OS-auth primitive. ViewRight iframe inspector stays app code (EX1). Names provisional.
Source: `crates/rightkit-tauri-shell/src/{windows.rs, preview.rs, devtools.rs, menu.rs, webview2_env.rs, associations.rs, open_paths.rs, print.rs, os_settings.rs, watchdog.rs}`, tests 26 files (`associations`, `devtools_policy`, `menu_entries`, `open_paths`, `logs_commands`, `popup_co*`); `crates/rightkit-overlay/src/lib.rs` (no tests dir); `packages/tauri-shell/src/index.ts`.

- [ ] PK08-1 tauri-shell staged candidate (0.5.0 in tree): build and run gate rows on W (`wv2-0_38`, `wv2-0_39`) and M; the devtools scanner negative test (release build with `content-devtools` refused by right-release); Windows clipboard/hardening/idle executed on W (README:45); `webview2_env` refuses debugging-port/no-sandbox flags (test); README feature table lists `logs`, `qa-probes`; dev-inspect text removed; CHANGELOG relabel.
- [ ] PK08-2 @rightkit/tauri-shell candidate: wrap `set_menu_item_checked`, `set_menu_state`, `print`, `open_paths_ready`, `associations_*`, `open_default_apps`; `MenuEntry` Check/Submenu; full `WindowSpec` fields; README version line; publish after the tauri-shell minor that carries PK04-2.
- [ ] PK08-3 (required) `SurfaceSpec`/`set_surface`; consumer ScrapeRight `window_surface.rs:13` (960×640 setup, 1200×760 main as config).
- [ ] PK08-4 (required) `OverlayHost` + overlay z-order walk; consumers HeardRight `hub_window.rs:420` pill/hub, Pulse notch (Swift via PK16); overlay product names (`OverlayKind::HeardRightPill`, `HR_STATUS_WINDOW_LEVEL`) replaced by app-supplied kinds in the same minor.
- [ ] PK08-5 (required) `PreviewStore` + scheme qualification (limits, expiry, revoke, no privileged IPC); consumer ViewRight `preview_protocol.rs:75`.
- [ ] PK08-6 (required) `PdfHost` (rightkit-pdf, feature-gated macOS) + shell authorization through tauri-shell's optional `pdf-host` feature depending on rightkit-pdf (never the reverse); consumers ViewRight `pdf_adapter.rs:106`, `mac_pdf_view.rs:103`; other platforms typed Unsupported.
- [ ] PK08-7 (required) App lock coordinator; credentials OS-auth primitive first (PK03 scope, credentials minor: LocalAuthentication / Windows Hello / Android biometric, typed Unsupported elsewhere); consumer Pulse only (local PIN vs remote approval kept distinct; HeardRight has no app-lock code). Pulse anchors: `core/src/remote/unlock.rs:518` `verify_pin`, `windows/src/unlock_auth.rs`, `windows/src/remote_alerts.rs:326-334,404` (`Way::Pin`/`Way::Hello`), `mac/Notch/Sources/Agent/ChatUnlockPanel.swift:30` (LAContext at :66,:142,:386,:454), `ios/Sources/Owners/AppLock.swift:8`; PIN authority and action policy stay Pulse data. (FH14)
- [ ] PK08-8 (required) Tests: focus stays with previous app, off-screen recovery on monitor removal, setup geometry transition, background launch, dispose leak checks, PDF owner isolation, preview revoke/expiry, lock then restart, inspector confinement.

Qualification: tauri-shell gate rows + check rows on W/M; overlay unit tests + a Windows smoke for `create_overlay_window` recorded; rightkit-pdf gate rows `[]`, `extract` + new macOS feature row (PK17-4).
Publication: tauri-shell staged modules publish only inside the PK04 host-registration minor (RV01); @rightkit/tauri-shell follows it; overlay, pdf and credentials minors with PK08-4/6/7.
Deletion consumers: HeardRight hub/pill window lifecycle; ScrapeRight `window_surface.rs`; ViewRight preview store and native PDF hosting; Pulse native lock orchestration; every app's context-menu install (`webview2_menu::install_context_menu`, ViewRight `context_menu.rs`).

### PK09 — @rightkit/react-shell, @rightkit/platform-ui, @rightkit/theme, settings-ui presentation (K09; row M2)

Published: react-shell 0.7.1, platform-ui 0.4.0, theme 0.3.1 (archives). Candidate: react-shell 0.7.2 (`FuseCurves orientation="horizontal"`; CHANGELOG "unpublished staging"). Required (minor of react-shell only when a named app deletion exists): primitives the app packets name (path-picker row, progress/status row, confirmation surface, native action section, Textarea/Table/Tooltip, tab strip, sidebar rail, popover placement policy); no global redesign, no new UI package.
Source: `packages/react-shell/src/{layout/*, ui/*, overlays/*, provider.tsx}`, `packages/platform-ui/src/{index.ts, qa.ts, headless/*}`, `packages/theme/src/*`; tests: each package's vitest suite, react-shell Chrome e2e via qa-chrome.

- [ ] PK09-1 react-shell 0.7.2 `orientation` (unpublished): keep only if an app packet names a consumer; otherwise remove it from the candidate before the next react-shell publish (locked §2). Run tests + e2e. (RV14)
- [ ] PK09-2 README/CHANGELOG drift: platform-ui QA exports moved to `./qa` (README:16,63-70,204), react-shell "0.6.9 unreleased" heading, theme `@rightkit/app-shell` references; fixed in the same publish.
- [ ] PK09-3 (required) Implement only primitives with a named deletion in `02..07`: MailRight three raw `<table>` → Table; others per packet. Each primitive: props typed, keyboard/focus/disabled/pending/error states, `prefers-reduced-motion` honoured, no app-specific styling.
- [ ] PK09-4 Keep React peer `<21`; no WebKit project added by this packet (gap stays named).

Qualification: `pnpm -F <pkg> test` + e2e on M (Chrome for Testing) and W; contrast report via `rightkit-theme generate --check` for each app in its packet; R2: scratch Vite app importing `./react` and `shell.css` builds.
Publication: next minor of react-shell when PK09-3 lands; platform-ui/theme only if touched.
Deletion consumers: MailRight raw tables; app shortcut registries, palettes, toast systems, dialog shells, context menus, titlebars, theme stores, platform sniffing (named per packet).

### PK10 — @rightkit/legal, @rightkit/legal-ui, @rightkit/onboarding (K10; row M14)

Published: legal 0.6.1, legal-ui 0.4.1 (`layout="contained"` in bytes, archive `rk-spec-adjudication-20261020/rightkit-legal-ui-0.4.1`), onboarding 0.6.2 (`layout`, `legalReceipt`, `permissionsStep` at steps.tsx:52, `modelsStep` at :136, `licenseStep`, `signInStep`). No candidate deltas.
Required (onboarding minor + legal patch): native receipt creation path: `LegalReceiptPort.create` (native timestamp, declared document set/digest, eligibility) replacing the renderer-built record at `Onboarding.tsx:224` (kept as legacy fallback with a deprecation note); versioned re-acceptance/reopen/reset over the receipt; `LicenseBackend` adapter over `@rightkit/license` 0.3.0 `LicenseView` shipped by onboarding; `ctx.signIn` reachable or removed; shared step adapters stay the existing `permissionsStep`/`modelsStep` (no synonyms). Host side: tauri-shell registers a legal-receipt service through PK04-2 (native time, store under app root via PK01/PK04 store).
Source: `packages/onboarding/src/{Onboarding.tsx, steps.tsx, types.ts}`, `packages/legal/src/{core.ts, tooling.ts}`, `packages/legal-ui/src/LegalGate.tsx`; tests: package vitest suites.

- [ ] PK10-1 Relabel the three stale "unpublished staging" headings; README peer table (onboarding legal-ui `>=0.4.1`), `layout` prop documented, `LicenseStepCopy` exported from index.
- [ ] PK10-2 (required) `LegalReceiptPort.create` + native service registration (PK04-2); renderer record path marked legacy; persistence error keeps the gate closed; bare legacy `legalAccepted` never passes.
- [ ] PK10-3 (required) `LicenseBackend` adapter over `LicenseView`; `signInStep` wired to the app's rightkit-oauth command via a typed port.
- [ ] PK10-4 (required) `rightkit-legal rust-licenses` unblocked by choosing `cargo-license-notices.mjs` as source (PK17-5) or the command removed; README "(unreleased)" text fixed.
- [ ] PK10-5 Tests: first run / re-acceptance on digest change, failed assent save, cancel/deny/retry permissions, offline model failure, skip/back/finish, relaunch at incomplete step, both layouts.

Qualification: vitest suites on M and W; `legal-snapshot --final` dry run on one app fixture; R2: scratch React app mounts `Onboarding` with memory backends.
Publication: onboarding next minor after PK04-2 host service exists; legal/legal-ui patch only if touched.
Deletion consumers: HeardRight legal pages and onboarding wizard (moves onto built-in steps); ScrapeRight `commands_models.rs:253` onboarding persistence/navigation; every app's acceptance flags and permission/model/licence step UIs.

### PK11 — rightkit-logs, @rightkit/logs, rightkit-crash, shell watchdog (K11; rows M15, M16, D4)

Published: logs 0.3.3 / @rightkit/logs 0.1.5 / crash 0.3.3; tauri-shell 0.4.0 `watchdog` (WatchdogConfig :46, on_stall :59, StallDetector :86, should_fire :124, install :197; `ShellBuilder::on_stall` lib.rs:543).
Candidate: logs 0.3.4 (`DetachedLog`, public `KnownSecrets`; "not compiled or run"), @rightkit/logs 0.1.6 (`createDiagnosticsRecorder`, crash-reason formatter), crash 0.3.4 (`KnownSecrets` in `redact_values`; CHANGELOG says floor logs 0.3.3, manifest 0.3.4).
Required: shell watchdog extension (monotonic elapsed timing, one outstanding ping, observer-gap suppression, recovery cancellation, cooldown, one capture in flight, bounded shutdown) in `rightkit-tauri-shell::watchdog` with HeardRight timings as `WatchdogConfig` inputs; crash capture adapter (`on_stall` → `capture_native_for_pid` + `record_report` + `Retention`); logs commands registered once via PK04-2; logging config shape (stems, retention incl. count-based, bundle sources, consent cache) as additive config on existing types. No `rightkit_crash::UiWatchdog`.
Source: `crates/rightkit-logs/src/{lib.rs, detached.rs, scrub.rs, outbox.rs}` tests 14 files (`detached_capture`, `known_secrets`, `scrub_parity`, `outbox_e2e`); `crates/rightkit-crash/src/{lib.rs, native.rs, dump_helper.rs}` test `crash_e2e`; `crates/rightkit-tauri-shell/src/watchdog.rs`; `packages/logs/src/{index.ts, crash.ts, recorder.ts}`.

- [ ] PK11-1 logs 0.3.4: compile and run `detached_capture`, `known_secrets`, `scrub_parity` on W and M; measure runtime budget (R7); publish.
- [ ] PK11-2 crash 0.3.4: CHANGELOG floor text = manifest (logs 0.3.4); Windows 11 minidump run recorded (`crash_e2e` on W; README:46); README lists `window-errors`, process-kind surface; publish after logs.
- [ ] PK11-3 @rightkit/logs 0.1.6: parity vectors for `scrubLogText` vs Rust `scrub_text` run in both suites; README entry table completed; publish.
- [ ] PK11-4 (required) Watchdog extension in tauri-shell (fields on `WatchdogConfig`, additive) + crash adapter; one bounded episode per stall, resumes, stops cleanly, capture failure visible; consumer HeardRight `ui_stall_watchdog.rs:36`.
- [ ] PK11-5 (required) Logging config shape + consent cache on existing `Outbox`/`ConsentStore`; consumers CodeRight `logs.rs`, ViewRight `logs.rs`.

Qualification: logs gate (default `http`, `tracing`, `log-bridge`, `testing` rows added PK17-4) W/M + Android compile; crash gate default + `window-errors`; tauri-shell `[]` row for the watchdog tests; negative test: a `password` field dropped before IPC.
Publication: logs 0.3.4, crash 0.3.4, @rightkit/logs 0.1.6 as a group (named consumers: all apps' crash redaction; HeardRight detached engine logs); tauri-shell watchdog in the PK04/PK06 shell minor.
Deletion consumers: ViewRight `logs.rs:1`; CodeRight `logs.rs:1` (keeps RK rotation adoption at `lib.rs:1243`); HeardRight `ui_stall_watchdog.rs` scheduler/episode mechanics; every app's log command wrappers, rotation, scrub regexes, consent files, bundlers.

### PK12 — rightkit-http, rightkit-browser cookie artifact, downloader (K12; row M17)

Published: http 0.3.6 (archive-confirmed, zero item delta to tree except the 0.3.7 parse fix); browser 0.3.4.
Candidate: http 0.3.7 (fractional `parse_rate_limit_reset_at`); browser 0.3.5 (`cookies::parse_netscape`, `BrowserPage::import_cookies`, `ManagedRoot` re-export; ScrapeRight G7/G8).
Required: redirect URL-shape check + typed redirect error + string-level URL guard (http minor; consumers CodeRight, MailRight); `rightkit-browser::CookieExport` (declared-domain filter, Netscape serialisation, private publish through fs `AtomicWriter`, empty/invalid export never replaces the old file; consumer ScrapeRight `inapp_sign_in.rs:27`); verified downloader = HTTP stream → fs `AtomicWriter` + digest (consumers rightkit-models downloads for HeardRight/ScrapeRight), not `copy_verified(path,path)`. `SecretStore` production implementation decided (README:18 vs secret.rs) and documented.
Source: `crates/rightkit-http/src/{client.rs, redirect.rs, netpolicy.rs, secret.rs}` tests 27 files; `crates/rightkit-browser/src/{cookies.rs, page.rs, session.rs, fixture.rs}` tests 5 files.

- [ ] PK12-1 http: README drift (0.2.5/0.3.0 text, reqwest 0.12 vs 0.13, socks5 doc); WinHTTP PAC test against a real proxy on W once or gap kept named; publish 0.3.7 only with PK12-3.
- [ ] PK12-2 browser 0.3.5: run `cft_close`, `e2e`, `fixture_session` on W and M; the 0.3.5 cookie tests exist and run; an intermediate browser release only if `06-scraperight.md` names an AP task it unblocks; otherwise the cookie items ship with PK12-4. (RV14)
- [ ] PK12-3 (required) Redirect shape check / typed error / URL guard with tests: redirect to unapproved origin refused, HTTPS→HTTP refused, cross-origin hop drops Authorization.
- [ ] PK12-4 (required) `CookieExport` in browser; tests: correct subset, private permissions, failed export leaves old artifact, clear invalidates engine auth snapshot.
- [ ] PK12-5 (required) Downloader in rightkit-models over http stream + fs AtomicWriter + digest; failure preserves old destination.

Qualification: http gate rows `--all-features`, `async`, `gzip,untrusted` on W/M; browser `held-resource` budget measured (R7) on W/M; R2: scratch client `Client::new(..).send(Request::get(..))`.
Publication: http next minor with PK12-3; browser: one release with PK12-4 unless PK12-2 names an unblocked AP task; models per PK07/PK14.
Deletion consumers: ScrapeRight `inapp_sign_in.rs` export mechanics; CodeRight/MailRight raw transport/retry helpers; app HTTP wrappers, redirect handling, URL guards.

### PK13 — @rightkit/app-update native bridge + tauri-shell `update` + license `update-auth` (K13; row M18)

Published: app-update 0.4.0; no shell update feature; license 0.2.3 has no update-auth.
Candidate: app-update 0.4.1 (`./native`: `createNativeUpdateClient`, commands `rightkit_update_check/status/download/install/cancel`, event `rightkit://update-progress`; `createPersistedCheckCache`; `dist/` stale); license 0.2.4 `update-auth` (`UpdateAuthProvider`, `AuthorizedHeaders`).
Required: tauri-shell `update` feature registering the five commands through PK04-2 over the Tauri updater + `UpdateAuthProvider`; closed DTOs (no URL/header/token across IPC); `UPDATE_MESSAGES` text de-licence-flavoured; persisted check cache + boot probation storage adapters supplied by the host; `@rightkit/release` adoption-evidence validation (PK17-6).
Source: `packages/app-update/src/{index.ts, native/index.ts, check-cache.ts, probation.ts}`; `crates/rightkit-license/src/update_auth.rs`; tauri-shell new `update.rs`.

- [ ] PK13-1 app-update 0.4.1: rebuild `dist/` (native, check-cache present); run contract vectors; README drift (`.previous` restore text → reinstall_required; `getInstallHeaders` strict default; `./native` listed); publish only together with PK13-2 (no consumer can use `./native` before the shell registers the commands).
- [ ] PK13-2 (required) tauri-shell `update` feature + command registration via PK04-2; negative tests: renderer never receives bearer headers; redirect cannot forward credentials outside the compiled allowlist; licence revision rechecked at install.
- [ ] PK13-3 Tests: deactivation during download, changed staging identity, installer interruption, wrong platform/tier, `assertSingleUpdatesVersion`.

Qualification: app-update vitest + contract vectors (M, W); tauri-shell `update` gate row added (PK17-4) on W/M; license `update-auth` row.
Publication: app-update 0.4.1 + tauri-shell minor + license (update-auth already in 0.2.4) as one group.
Deletion consumers: five apps' native update code (rk-compliance-v2 item 7): ViewRight `updateLicense.ts:387` header creation, CodeRight, MailRight, ScrapeRight, HeardRight updaters, version comparers, consent flags, overlay UIs.

### PK14 — rightkit-qa, @rightkit/qa, @rightkit/qa-chrome, M21 execution context (K14; rows M20, M21)

Published: rightkit-qa 0.7.1 (bytes: 34 pub mods; `dialog`, `fake_cli`, `rightkit-fake-cli` bin and `ui_dialog_*` steps are NOT in 0.7.1), @rightkit/qa 0.5.1, qa-chrome 0.1.8.
Candidate: rightkit-qa tree additions (dialog, fake_cli, ui_dialog steps); qa-chrome 0.1.9 (close budget, identity-safe signalling; "not compiled or run", #811).
Required: QA harness glue (rk-compliance-v2 item 6): Tauri invoke/listen mock with per-command fixtures, browser IPC seam, legal-gate helper, journey runner, capture + visual diff, app-registered Control endpoint registry, process-tree stop probe; M21 execution context = daemon `mode::{ModeInput, ProcessInput, AppRoots, decide, init_or_exit, roots_with}` (existing) + immutable run identity propagation only where missing; models root from daemon (PK07); permission-dialog detector implemented or removed; Windows installed-package lane (gap row 56); rightkit#821 bound capture outcome + authenticated terminal cleanup in rightkit-browser `FixtureSession` (own issue, scheduled on Adrian's go).
Source: `crates/rightkit-qa/src/{scenario.rs, embedded/*, storage.rs, userdata.rs, binary.rs}` tests 6 files; `packages/qa/src/*`, `packages/qa-chrome/src/*`; `crates/rightkit-daemon/src/mode.rs`; `crates/rightkit-models/src/suite/paths.rs:93-205`.

- [ ] PK14-1 rightkit-qa: the tree still says 0.7.1 while carrying unpublished `dialog`, `fake_cli` and `ui_dialog_*` additions: bump the tree version before any publish, keep each addition only with a named consumer (candidate: `@rightkit/tauri` `dialog-qa`, PK05-7) or remove it; relabel the 0.7.1 CHANGELOG as published; measure the Windows 0.7.1 budget (README:293, #808) — required before any next publish (R7); run `check` row `control,wv2-0_38` on W.
- [ ] PK14-2 qa-chrome 0.1.9: compile/run on W, M, L; publish; then @rightkit/qa next patch with the exact dep bump; README pin text.
- [ ] PK14-3 (required) models root convergence: `suite::paths` takes its root from daemon mode/AppRoots; no `dirs` fallback in release, no panic path; Android host root via the same typed authority; tests on W/M + Android compile (PK07 consumer).
- [ ] PK14-4 (required) QA glue items with named app deletion (MailRight `tauri_run.rs:109` QA registration; every app's invoke mocks; HeardRight/ScrapeRight journey runners); the Tauri mock is the TEMPORARY exception until this lands.
- [ ] PK14-5 (required) M21 negative tests: release build ignores every selector; activation without root refused; root overlapping user data refused; foreign-app binding refused; marker absent from release artifacts; production credential/device/licence namespaces unreachable from a QA context.
- [ ] PK14-6 (#821, separate dispatch) FixtureSession bound capture outcome + cleanup classes incl. timeout/cancel; axe licence decision inside that issue. Capability `PK14.fixture-capture` has status `deferred-separate-dispatch`: it is outside this delivery, excluded from the PK14 rollup, and no AP task in this delivery may require it. (AH01)

Qualification: rightkit-qa gate (default + `control,wv2-0_38` check + `browser`, `android` rows added PK17-4); @rightkit/qa tests after qa-chrome build; daemon `test-mode` row; models gate.
Publication: qa-chrome then @rightkit/qa patch; rightkit-qa next minor when PK14-3/4 land; models next minor with PK14-3 (+PK07 floor raise if `AppRoots` needs 0.3.1).
Deletion consumers: all six apps' harnesses, wdio shims, process killers, evidence writers, mock servers; app test-mode env parsers (HeardRight `HEARDRIGHT_ENGINE_TEST_MODE` reads at delivery.rs:31, engine.rs:46, settings.rs:322-324 → `init_or_exit` + `EngineEntitlements`).

### PK15 — rightkit-agent-tools + daemon admission for hosted surfaces (K15; row M22)

Published: agent-tools 0.2.0; automation 0.5.0. Candidate: agent-tools 0.2.1 (camelCase/PascalCase exposure hardening, speech-client floor 0.4.0, docs `<app>.*` dev/QA-only); automation 0.5.1 (held, RV05).
Required: applicability record per app (`client | host | both | absent`) tied to real entrypoints; hosted surfaces (Pulse, MailRight) on PK07-4 approvals + PK07-5 service registration; a `desktop.*` test in agent-tools (README:96); Cargo description drops `rightkit-qa-driver`; CodeRight `desktop.*` authority stays Adrian's decision.
Source: `crates/rightkit-agent-tools/src/{toolbox.rs, exposure.rs, backends/*}` tests 4 files.

- [ ] PK15-1 agent-tools 0.2.1: run `schema_control_conformance`, `demo_absent`, `peer_executable` on W/M; add one `desktop.*` test; description fixed; `held-resource` budget measured (R7); publish.
- [ ] PK15-2 (required) Applicability record schema in `rightkit-mandatory.json` (`M22: {role: client|host|both|absent, entrypoints: [...]}`) consumed by `ownership-mandatory` (PK17-3).
- [ ] PK15-3 Negative tests: `paste` tool cannot register; `max_tier=foreground` refused; HeardRight `eval` refused; denied approval causes no effect.

Qualification: agent-tools gate default + `demo` rows (PK17-4) on W/M.
Publication: 0.2.1 after PK15-1; automation only if #821 or an app packet names it.
Deletion consumers: Pulse/MailRight tool gates, exposure lists, CLIs, approval transports (after PK07-4); CodeRight tool wrappers where contracts match.

### PK16 — RightKitSwift 0.9.0 (K16; Apple hosts for M1/M3/M7/M10/M12/M17/M18)

Published: tag 0.8.1. App pins today: Pulse `https://github.com/bogusyogi/rightkit` exact 0.8.1 (`mac/Hub/Package.swift:23`, `mac/Notch/project.yml:46-49`); HeardRight `https://github.com/Orthic-Labs/rightkit.git` exact 0.8.0 (`tauri-app-next/macos-native/HeardRightPillMac/Package.swift:26`, `ios-native/project.yml:17-19`); ScrapeRight and CodeRight Orthic-Labs exact 0.8.1. Canonical URL: `https://github.com/bogusyogi/rightkit` (the RightKit checkout's `origin` remote, which `right-release publish swift` tags); each app's Swift task moves its pin to the canonical URL (Pulse AP-PU-16, HeardRight AP-HR-15, ScrapeRight AP-SR-16, CodeRight AP-CR-14), and PK16-3 updates the `swift`/`stagedSwift` entries of `rightkit-versions.json` at readback (FH26). Candidate: 0.9.0 held (`VERSION` 0.9.0; CHANGELOG "held, not tagged until Adrian's go"; `swift test` 835 pass 2026-10-18; products incl. RightKitSystem, Store, EngineClient, Engine, Update, HTTP, DB, Account, Overlay, Credentials).
Required: only semantics mirrored for named Swift consumers: Pulse Hub (supervised service, store, helper transport), HeardRight helpers (credential/CloudKit hosting mechanics), ScrapeRight iOS native (existing RightKitFS/License/Purchases/CloudSync/ImportQueue consumers, scoped adoption of the 0.9.0 products they already use), CodeRight iOS (`apps/coderight-ios`, existing native modules: LicenseStore, AccountSession, LocalRuntime, MCPSecretStore); HeardRight iOS (`heardright/ios-native`, pins RightKitSwift exact 0.8.0 at `project.yml:17` with License/Purchases/CloudSync/Store/Credentials products; live shared-workflow residue: `LicenseController.swift:67` refresh-generation coordination and :103-161 deferred deactivation/lease-removal retry, `ProEntitlementResolver.swift:316` private `ProBridgeClient` POST/decode, `AccountSessionStore.swift:196` session renewal/device transport with generic `send` at :278; compare against `RightKitAccount/SessionStore.swift:74` and add only the behaviour the shared owner lacks; product copy, consent, StoreKit/CloudKit identities and device-only credential policy stay app-owned) (AH04); exact remote tag binding; no local package path; no four-language codegen; this is scoped adoption per named consumer, not a new full Apple mandatory profile.
Source: `swift/RightKitSwift/Package.swift`, `Sources/*`, `Tests/*`.

- [ ] PK16-1 Fresh `swift test` on M at the commit to tag; generic iOS/watchOS builds. The 835 generic tests do not cover hardware/entitlement paths: each such API used by a named consumer has scoped native evidence (device or entitled host run) or returns a typed `Unsupported` result with a test. (RV14)
- [ ] PK16-2 (required) Move Pulse Swift hub/supervised-service/helper-transport mechanics (sources: `swift/RightKitHub/Sources`, the in-repo package `mac/Hub/Package.swift:21` points at, and `mac/Notch/Sources`; compare type by type against RightKitSystem/Store/EngineClient) and HeardRight helper credential/CloudKit hosting into the matching products (FH15). Migration inventory (exact anchors; not blanket file deletion; domain callback payloads and actions stay app composition):
    - Pulse (`D:/Claude/pulse`): `swift/RightKitHub/Sources/RightKitHub/Settings/HubSettingsStore.swift::{init,get,set}`; `Platform/HubServicesEnvironment.swift` (`EnvironmentValues.hubServices`); `Platform/HubPlatformServices.swift::HubPlatformServices` (:71); `mac/Notch/Sources/Agent/EngineClient.swift::ResidentEngineClient::{current,detach,call}` (:9); `mac/Notch/Sources/Updater`; `ios/Sources/Owners/AppLock.swift::AppLock` (with PK08-7).
    - HeardRight (`D:/Claude/heardright/tauri-app-next/macos-native/HeardRightPillMac/Sources`): `HeardRightAppleCredentials/main.swift::CredentialStore` (:168; `probeStatus` :184, `read` :208, `write` :219, `deleteEverywhere` :235, `copySyncedToDeviceOnly` :243); `HeardRightPillMac/CloudSyncNotifications.swift::CloudSyncNotifications` (:34; `setObserver` :86, `setEnabled` :96, `invalidateSubscription` :369; C exports `hr_cloud_sync_set_observer` :591, `hr_cloud_sync_set_enabled` :599, `hr_cloud_sync_invalidate_subscription` :606); `PillBridge.swift::PillCallbacks` (:7; `markCallbacksReady` :13, `copyText` :32, `updateHistoryItem` :37, `openHub` :46, domain actions :47-62; `hr_pill_init` :103, `hr_pill_shutdown` :114, `hr_pill_push_event` :117, `hr_pill_reveal` :134, `hr_pill_set_visible` :137). CloudKit schema and merge rules stay app data.
- [ ] PK16-4 (required) HeardRight iOS parity: licence refresh-generation and deferred deactivation/lease settlement in RightKitLicense, Pro-bridge exchange (wire-compatible with the existing service payloads; shares test vectors with PK05 where the Rust side exists, but takes no dependency on any Rust publication unless the iOS bridge actually consumes it), account-session renewal/sign-out/device transport in RightKitAccount; each gap found by comparison against the anchors above gets a shared implementation and a test, or a recorded reason it is product policy. Consumer: AP-HR-15; native adoption depends on PK16 slices only. (AH04)
- [ ] PK16-3 Tag the next RightKitSwift version selected by R5 (staged label 0.9.0; breaking product changes take the next minor) via `right-release publish swift`; Adrian dispatching this packet with publication scope is the go the CHANGELOG names (no extra pause); apps pin the repository URL plus the exact tag, and existing exact tags stay until an AP task moves them. (RV14)

Qualification: M only (Apple checks cannot be replaced by Windows compilation); helper protocol compatibility vectors; fresh remote resolution of the tag from a scratch package; HeardRight iOS licence/account/Pro-bridge vectors shared with PK05 (refresh race, deferred deactivation retry, sign-out, bridge decode) run in `swift test`.
Publication: the R5-selected tag after PK16-1/2; later tags per consumer slice, each selected by R5.
Deletion consumers: Pulse `mac/Hub/Package.swift:23` dependency on app-local hub code; HeardRight `cloud_sync_cloudkit.rs:181` helper hosting; ScrapeRight iOS duplicates named in `06-scraperight.md`; CodeRight iOS duplicates named in `07-coderight.md`. Qualification covers all four Swift consumers' protocol vectors, not only Pulse and HeardRight.

### PK17 — @rightkit/release, M23 switches, mandatory set, handoff tooling (rows M19, M23, M21 metadata)

Published: release 0.4.0. Candidate 0.4.1 staged (production-mode frontend build, `development` resolve-condition refusal, license-testing bundle markers, Windows `publish cargo --local`, sidecar duplicate-member fix); unreleased: iOS recipe, #778 updater signing, disk hygiene (#776) — excluded here (RV02).
Required: release-switch registry classification of security controls (db `JOURNAL_ENV`, credentials guard vars → classes S4/S1 with conditions) vs forbidden test selectors; scanner rule for the daemon loopback feature separate from `remoteHost`; `ownership-mandatory` reads the M22 applicability record and the new M21/M23 owners; mandatory-set minimums raised (device 0.3.1, legal-ui 0.4.1, onboarding 0.6.2, rightkit-qa 0.7.1) and `ownerPending` removed for M21/M23 only by `PK17.activation`, which consumes satisfied owner capability receipts (`PK17.scanner`, `PK14.mode-roots`, `PK01.atomic-baseline`) through a signed §6 snapshot plus an authenticated trust-sync of that snapshot, never PK rollup statuses; M21/M23 activation waits for that qualified receipt (until then, today's report/enforce modes only); (RV11) gate rows added for new features; adoption-evidence validation (active target/features, resolved exact version + integrity, exercised entrypoint, deletion list, exception entries); cargo licence notices CLI route (`rust-licenses`); recipes named in rk-compliance-v2 item 8 only where an app packet names the script it deletes.
Source: `packages/release/{release-switch-scan.mjs, release-switch-registry.json, ownership-mandatory-set.mjs, ownership-mandatory-set.json, publication-gate.mjs, publish-cargo.mjs, qa-build-features.mjs}`; `ci/gate.config.json`.

- [ ] PK17-1 Qualify 0.4.1 staged content on M and W (`pnpm test`, registry parity, signer tests); publish (M only for `publish npm --local`).
- [ ] PK17-2 Scanner: new rule for the daemon loopback feature (PK06-5) distinct from `remote-feature-undeclared`; `remoteHost` semantics unchanged; fixtures under `fixtures/release-switches/`.
- [ ] PK17-3 `ownership-mandatory`, in two separately recorded capabilities (AH02):
    - `PK17.scanner` (owner implementation): M21 owners (rightkit-fs, rightkit-daemon, rightkit-qa), M23 owners (@rightkit/release, rightkit-daemon) and the M22 applicability record are implemented, qualified and published in report mode with `ownerPending` unchanged (needs `PK17.release`, not `PK17.gate-rows`); `--enforce` semantics unchanged. This capability never depends on activation.
    - `PK17.activation` (enforcement activation): consumes satisfied capability receipts, not PK rollups: `PK17.scanner`, `PK14.mode-roots` (daemon mode + rightkit-qa) and `PK01.atomic-baseline` (fs `app_dirs`/`test_mode`). It lands the minimum raises and `ownerPending` removal through a signed §6 snapshot plus an authenticated trust-sync of that snapshot. Bootstrap: (1) today's policy in report mode; (2) `PK17.scanner` and the owner capabilities qualify and publish; (3) activation applies. Activation state never gates the receipts it consumes. (RV11, AH02)
- [ ] PK17-4 `ci/gate.config.json` rows added: db `sqlcipher` / `sqlcipher-system`; license `client,update-auth,subject-entitlement,engine-gate`; daemon `remote` + loopback feature; tauri-shell `update`, `--no-default-features --features wv2-0_39` and `wv2-0_39,engine,logs` (Windows only; ViewRight, ScrapeRight and CodeRight pin `wv2-0_39`); rightkit-qa `browser`, `android`, `control,wv2-0_39` check (Windows only); logs `tracing`, `log-bridge`, `testing`; crash entry with default + `window-errors` (no crash entry exists today); agent-tools `demo`; rightkit-pdf macOS feature; plus the working-tree-only tauri-shell `logs` featureSet and `engine,logs` check row, which this task stages (the `rightkit-pdfium` entry is out of scope and stays unstaged). Each row runs green before the owning PK publishes. (FH10, FH11, FH38)
- [ ] PK17-5 Cargo licence notices CLI route (`cargo-license-notices.mjs`) so `rightkit-legal rust-licenses` unblocks (PK10-4).
- [ ] PK17-6 Adoption-evidence validation command (its tests include the §6 worked cases as executable JSON fixtures: helper-trust ready while encrypted-lifecycle pending, QA/mode ready while fixture-capture deferred, device 0.3.1 via qualify-existing, N/A task never ready): checks the §6 manifest entries against the app's lockfile/Cargo.lock, active target and feature closure, and requires a recorded exercised-entrypoint receipt (test or run) per adopted contract. CHANGELOG and deletion-list prose is context only and never satisfies a runtime check. (RV11)
- [ ] PK17-7 Handoff (§6) emitted by the publisher: one append-only snapshot per `publish` or `qualify-existing` event, plus the cumulative `published-release-manifest.json` regenerated from all snapshots.
- [ ] PK17-8 README: document `ownership-mandatory`, `ownership` subcommands, D4 wording aligned with the mandatory set; smol-toml pin text.
- [ ] PK17-9 Release recipe inventory (M19), source-backed: per app, packaging, signing, notarization, staging, helper, asset and notice steps with the actual script/symbol closure from each app packet's "M19 release recipe caller closure" (e.g. HeardRight `sign-macos-bundle`, `mac-dmg`; ScrapeRight `prepare-runtime`; CodeRight `package-windows-headless`, `fetch-embed-assets`). A recipe enters @rightkit/release where any app packet's M19 closure names an app-neutral mechanic (one current consumer is sufficient); app-specific identifiers, payload lists and assertions stay as config; wrappers that already delegate stay as config. (FH05) New iOS/#778 recipe features are deferred only where outside the locked spec. App M19 closures gate PK13/PK17 adoption tasks. (RV13)

Qualification: `pnpm test` on M and W; `fixtures/release-switches/` pass; a seeded forbidden feature fails a fixture build; `inventory check` clean.
Publication: after PK17-1; later patches as PK17-2..8 land (each with its own qualification; none blocks crate publishes).
Deletion consumers: six apps' build/sign/notarize scripts named in their packets; app-level env switches not inventoried.

## 4. Native host matrix

| Package | W (Dell) | M (Mac mini) | L (PR check) | A (compile) | Notes |
|---|---|---|---|---|---|
| fs, child-process, db, app-settings, http, logs, crash, daemon, job-queue, sidecar | run | run | fs/credentials/daemon only | app-settings, logs, crash, credentials | Windows-only tests (`.rs*`) run on W |
| credentials | run (`windows_*`) | run (+ isolated keychain) | run | compile | Keystore not on device |
| license, device | run | run | – | – | vectors identical |
| tauri-shell | run `wv2-0_38`, `wv2-0_39` | run | – | – | content-devtools check row only |
| overlay | smoke | unit | – | – | no tests dir today |
| qa, browser, automation, agent-tools, sandbox, models | run | run | qa-chrome only | models compile | `held-resource` budgets measured |
| npm packages | test | test + publish | – | – | `publish npm --local` is Mac-only |
| RightKitSwift | – | test + tag | – | – | iOS/watchOS generic builds |

## 5. Acceptance scenarios (from K01–K16, mapped)

Each PK's tests must cover its K acceptance list in Astra r1 §5; the packet names only the additions beyond existing tests. Program-level: Q1 upgrade compatibility (settings, identities, legal assent, credentials, history) proven by an upgrade test per app packet against the handoff versions; Q2 untrusted caller refusal (PK04/PK05/PK13); Q3 interruption at durable phases (PK01/PK03/PK07); Q4 child failure classes (PK02/PK06); Q5 concurrency/replay (PK04/PK07); Q6 native parity on each PK's affected native targets per the §4 host matrix (W and M for cross-platform crates and npm packages; M only for RightKitSwift; L and A only where §4 lists them); Q7 production artifact immune to QA env/roots (PK14/PK17); Q8 deletion + routing (app packets, validated by PK17-6). Every app's existing exception ledger (`docs/rightkit-exceptions/M*.md`) is reconciled by its app packet; entries this packet's owner work retires include at least ViewRight M8 B1 (`destination_write_lock`) by `PK01.write-locks`, CodeRight M7 B1 (loopback HTTP/bearer engine) by `PK06.loopback`, and Pulse M5 (raw `Command` pass-through) by `PK02.capture` (FH03).

## 6. Handoff manifest (PK17-7; shared keys with README.md and 02..07)

Two files, both under `docs/plans/2026-10-20-rightkit-implementation/handoff/`:
- **Snapshots** `snapshots/<nnnn>-<event>-<ecosystem>-<name>-<version>.json`: one per event, append-only, never edited. `event` is `publish` (this run uploaded the version) or `qualify-existing` (an already published version is fetched, its exact bytes qualified, and a capability satisfied without republishing). (AH03)
- **Delivery file** `published-release-manifest.json`: the cumulative stable view the app packets read, regenerated from all snapshots after each event. It carries `selectedSnapshot {path, sha256}` binding it to the newest snapshot it includes; the delivery note to Adrian and to each app quotes both SHA-256 values.

No value is typed by hand. `version`, `integrity`, `archiveSha256` and `registryReadback` come from the publisher's readback (or, for `qualify-existing`, from the fetched archive and registry); `publisherTool` from `right-release --version` on the publishing host; every `status` and `ok` from executed checks.

**Before the emitter exists (FH07).** Until `PK17.handoff` (PK17-7) is published, the executor writes snapshots with a small script that copies the publisher's evidence paths and the registry readback verbatim (`snapshotProducer = { "kind": "manual", "tool": "<script path + sha256>", "host": "<host>" }`; `publisherTool` keeps its per-event meaning and stays null for `qualify-existing`); no value is typed by hand. A manual snapshot must pass the same schema, hash and evidence checks as an emitted one before any readiness derives from it. When PK17-7 lands, the emitter reconstructs each manual snapshot separately and compares; original bytes are never overwritten. On a mismatch or correction it appends a superseding event that names the snapshot it supersedes, and only the readiness derived from the affected entries is invalidated until the superseding event is accepted.

**Unchanged published packages (FH09).** Every published, unchanged package that a satisfied slice or an app pin relies on gets a `qualify-existing` snapshot, so every app pin is derivable from the manifest: at least device 0.3.1, db 0.5.0, app-settings 0.2.4, sidecar 0.3.0, rightkit-pdf 0.3.2, rightkit-overlay 0.1.2, @rightkit/platform-ui 0.4.0, @rightkit/theme 0.3.1, @rightkit/legal 0.6.1, @rightkit/legal-ui 0.4.1, @rightkit/onboarding 0.6.2, and rightkit-qa 0.7.1 (fetched archive only, not the tree).

**`qualify-existing` events (AH03).** `publisherTool` is null and `originalPublication` records `{publishedAt, publisher}` from the registry, with `publisher: "unrecorded"` when history does not say; the current run's tool and host go in `qualifier {tool, host}`. `sourceCommit` is the verified tag commit where one exists, otherwise null with a reason. `packedConsumer` is `{ "ok": null, "notApplicable": "unchanged published bytes" }`; `fetchedConsumer` is required. Worked case: device 0.3.1 satisfies `PK05.device` by a `qualify-existing` event, no republish.

**Capabilities (AH01).** PK01..PK17 are rollups. Readiness is derived from capability slices `PKnn.<slice>`, each one independently qualified and receipted:

| PK | Capability slices (tasks) |
|---|---|
| PK01 | `atomic-baseline` (published 0.4.4 atomic/state/private/lock/digest/app_dirs; `qualify-existing` unless a later PK01 publish carries it) · `atomic-publication` (01-2 retry; 01-3 `publish_dir_with` only if a consumer is named, else removed) · `lease` (01-1) · `write-locks` (01-5) · `transactions` (01-6,7); 01-4 relabel travels with the first PK01 publish |
| PK02 | `capture` (02-1 for `run`/`run_with_stdin`, 02-4 suites, 02-5 doc) · `configured-runner` (02-1 for `run_command`, its 02-3 consumer) · `streaming` (02-3 `run_streaming`/`StreamOptions`) · `breakaway` (02-2 + explicit builder). Each slice's tests cover only its own exports; an unpublished export whose slice has no named consumer is removed before publish |
| PK03 | `sqlite-baseline` (published db 0.5.0 plain open, read pool, migrate, backup; `qualify-existing`, no encryption, no new code) · `protected-store` (03-1a; existing secure backend, the credential-store baseline every credential user needs) · `helper-trust` (03-1b) · `encrypted-lifecycle` (03-2..6) |
| PK04 | `settings-baseline` (published app-settings 0.2.4 `Store`/`Document`/`SqlSettings` delegation; `qualify-existing`, no UI or host-services gate, no new code) · `host-services` (04-1,2,3,4,6) · `write-security` (04-5) |
| PK05 | `entitlements-baseline` (published license 0.2.3 `EngineEntitlements`, verify, lease envelope; `qualify-existing`, no new code; consumer HeardRight `is_pro()` fallback removal) · `license-client` (05-1,2,3,7; needs `PK04.host-services`) · `device` (05-4, qualify-existing) · `account` (05-5,6) |
| PK06 | `job-queue` (06-1,2) · `engine-host` (06-3) · `daemon-client` (06-4,6) · `loopback` (06-5) |
| PK07 | `migration` (07-1,2) · `root-cutover` (07-3) · `approvals` (07-4) · `services` (07-5); 07-6 tests split by slice |
| PK08 | `shell-modules` (08-1,2) · `surface` (08-3) · `overlay` (08-4) · `preview` (08-5) · `pdf` (08-6) · `app-lock` (08-7: the new credentials OS-auth primitive plus the shell coordinator; its receipt must include native evidence for each required backend, LocalAuthentication on M, Windows Hello on W, typed `Unsupported` tests elsewhere; `PK03.protected-store` or any credentials storage receipt never satisfies it); 08-8 tests split by slice |
| PK09 | `primitives` (09-1..4) |
| PK10 | `receipts` (10-1,2,5) · `license-step` (10-3) · `notices` (10-4, with PK17-5) |
| PK11 | `redaction` (11-1,2,3) · `watchdog` (11-4) · `log-config` (11-5) |
| PK12 | `redirect` (12-1,3) · `cookie-export` (12-2,4) · `downloader` (12-5) |
| PK13 | `update` (13-1,2,3) |
| PK14 | `qa-chrome` (14-2) · `mode-roots` (14-1,3,5) · `harness` (14-4) · `fixture-capture` (14-6, deferred-separate-dispatch) |
| PK15 | `agent-tools` (15-1,3) · `applicability` (15-2) |
| PK16 | `swift-release` (16-1,3) · `pulse-hub`, `heardright-helpers` (16-2) · `heardright-ios` (16-4) · `scraperight-ios`, `coderight-ios` (16-2 scope per consumer) |
| PK17 | `release` (17-1) · `scanner` (17-2, 17-3 scanner part) · `gate-rows` (17-4; each added row gates only its owning crate's release, and `scanner` never waits on rows for other crates) · `activation` (17-3 activation part) · `notices` (17-5) · `adoption-evidence` (17-6) · `handoff` (17-7) · `docs` (17-8) · `recipes` (17-9) |

Worked cases: (1) `PK03.helper-trust` satisfied by the credentials patch makes AP-HR-03 ready while `PK03.encrypted-lifecycle` is still pending; a credential user that needs only storage requires `PK03.protected-store`, not helper trust. (2) `PK14.mode-roots` and `PK14.harness` satisfied make the QA/mode AP tasks ready while `PK14.fixture-capture` stays deferred.

```json
{
  "schemaVersion": 1,
  "spec": { "commit": "5ce6e2c32843edd28b003162f31f50f6cce0d9b5", "sha256": "f01330aed8e63a74ee290e31ad1c4c728dd8b3f8414bea4bea17dd4855701a49" },
  "generatedAt": "<iso>",
  "selectedSnapshot": { "path": "snapshots/<nnnn>-<...>.json", "sha256": "<hex>" },
  "packages": [
    {
      "event": "publish | qualify-existing",
      "id": "<cargo|npm|swift>:<name>",
      "ecosystem": "cargo | npm | swift",
      "name": "<name>",
      "version": "<readback>",
      "sourceCommit": "<publishing commit | verified tag commit | null>",
      "sourceCommitNullReason": "<null unless sourceCommit is null>",
      "archiveUrl": "<registry url | repository url + tag>",
      "integrity": "<crates.io index cksum | npm integrity SRI | tag commit sha>",
      "archiveSha256": "<sha256 of the .crate/.tgz bytes | sha256 of the git source archive of the tag>",
      "publishedAt": "<iso>",
      "publisherTool": { "version": "<right-release --version>", "source": "installed-published | source-tree-copy", "host": "win32 | darwin" },
      "snapshotProducer": { "kind": "emitter | manual", "tool": "<emitter version | script path + sha256>", "host": "win32 | darwin" },
      "originalPublication": { "publishedAt": "<registry iso>", "publisher": "<recorded | unrecorded>" },
      "qualifier": { "tool": "<right-release --version>", "host": "win32 | darwin" },
      "registryReadback": { "lookup": "<index/npm view/ls-remote output path>", "versionsJsonSection": "<rightkit-versions.json registry entry>" },
      "features": ["<published feature sets>"],
      "targets": ["win32-x64", "darwin-arm64"],
      "qualification": [
        { "host": "win32 | darwin | linux-pr | android-compile", "features": "<feature set>", "gateEvidence": "<path>", "result": "pass | fail | not-applicable" }
      ],
      "runtimeBudget": "not-required | measured:<path> | deferred:<reason>",
      "packedConsumer": { "ok": false, "notApplicable": null, "example": "<name>", "evidence": "<path>" },
      "fetchedConsumer": { "ok": false, "example": "<name>", "evidence": "<path>" },
      "reviewer": "astra | fable"
    }
  ],
  "capabilities": {
    "PK01.lease": { "status": "pending", "tasks": ["PK01-1"], "packageIds": [], "features": [], "targets": [], "evidence": [] }
  },
  "contracts": {
    "PK01": { "status": "pending", "capabilities": ["PK01.atomic-baseline", "PK01.atomic-publication", "PK01.lease", "PK01.write-locks", "PK01.transactions"] }
  },
  "apps": {
    "<app>": {
      "tasks": { "AP-XX-nn": { "requiresCapabilities": ["PKnn.<slice>"], "ready": false, "applicable": true, "applicabilityResolved": false, "naReason": null, "evidence": [] } },
      "readyForAdoption": []
    }
  }
}
```

Rules:
- `capabilities` always lists every slice in the table above, `contracts` always lists all 17 ids PK01..PK17, and `apps` always lists every AP id of the six packets, derived by extracting the task IDs (`AP-<APP>-<nn>`) from the frozen app packets 02–07 rather than from a hand-kept list (at freeze: PU01–19, HR01–21, VR01–14, MR01–12, SR01–17, CR01–15, 98 tasks; the manifest's AP ID set must equal the extracted frozen task set exactly: an omission, an extra ID or a duplicate fails validation), including tasks marked `"applicable": false` (N/A with a reason in `evidence`).
- `capabilities.<id>.status` is `satisfied` only when every task of that slice is checked, every qualification row for its packages, features and targets is `pass` or an evidenced `not-applicable` (the row's evidence states why the host/feature does not apply), and every package has `fetchedConsumer.ok` from a `publish` or `qualify-existing` event. `deferred-separate-dispatch` is allowed only for `PK14.fixture-capture`.
- `contracts.PKnn.status` is a derived rollup: `satisfied` when every slice except deferred ones is satisfied, `partial` when some are, `pending` otherwise. No readiness rule reads it.
- Field rules: for `event: "publish"`, `publisherTool` is set and `originalPublication`/`qualifier` are null. For `event: "qualify-existing"`, `publisherTool` is null, `originalPublication` and `qualifier` are set, and `packedConsumer` is `{ "ok": null, "notApplicable": "unchanged published bytes" }`. `sourceCommitNullReason` is required exactly when `sourceCommit` is null.
- `tasks.<AP-id>.ready` is derived: true only when `applicable` is true, `applicabilityResolved` is true, and every `requiresCapabilities` entry is `satisfied`. `applicabilityResolved` becomes true only after every conditional requirement of the task has a source-backed disposition; while any is unresolved the task is not ready, and an unresolved list never counts as empty. A conditional requirement found absent is omitted on its own; the whole task is N/A only when no work in it applies; app packets map each AP task to the slices it actually needs. A task with `applicable: false` has `ready: false`, a `naReason`, and never appears in `readyForAdoption`. A preflight task with known applicability and an empty `requiresCapabilities` becomes ready once the manifest itself verifies (hashes and schema).
- Readiness is owner readiness only: it says the RightKit side is available, never that the app task is done. App completion is recorded by the app's own evidence (PK17-6), and an app's final closure also requires every earlier AP task it depends on to be complete. `readyForAdoption` is the derived list of ready ids. Neither is ever set from prose.
- App packets reference AP ids and PK ids, never version numbers; versions are read from `published-release-manifest.json`.
- RightKitSwift entries carry the repository URL and exact tag; `archiveSha256` is the hash of the git source archive for that tag.
- The final handoff (all non-deferred slices satisfied) is the same file with every capability, every contract and every AP id present.

## 7. Executor brief (copy to the agent)

```
RightKit package packet 01 — execute PK by PK; rules §1; order §2; stop only on an affected failure.
[ ] 0. Pull main; confirm HEAD includes 5ce6e2c3; record `git status`; unrelated dirty files are preserved untouched and never staged (stage named files only); no worktrees, no branches (D3).
[ ] 1. For each capability slice (§6) in dependency order within its lane (an existing release that already satisfies a slice gets a `qualify-existing` event instead of a publish):
    [ ] a. Code the required items (child workers, edit-only, named files) and reshape the staged candidate to fit; raise floors only for consumed new API.
    [ ] b. R1: exact gate featureSets/checkFeatureSets rows (PK17-4 rows included) on W and M; fmt; affected-closure check; secret scan. Before any push: the R10 push gate.
    [ ] c. R7: runtime budget measured and recorded where the crate is budgeted.
    [ ] d. R8: Astra sign-off on the exact diff.
    [ ] e. `qualify-existing` slices skip this step entirely (no R2(a), no new-version lookup, no upload) and go to f against the fetched archive. Otherwise: R2(a) packed-artifact scratch consumer; live registry lookup shows the number absent (§2 collision handling); `right-release publish cargo --crate <c> --local` (host per R6) or `publish npm --package <p> --local` (Mac) or `publish swift` (Mac; dispatch with publication scope is the go).
    [ ] f. R2(b): fetched scratch consumer resolves the published version and runs the PK example; on failure publish the fix as the next number.
    [ ] g. Snapshot written and `published-release-manifest.json` regenerated (§6); CHANGELOG relabelled; README drift items in the PK closed.
[ ] 2. Never: republish a number, yank-and-reuse, Cargo/npm path/git/patch deps (Swift: exact remote tag only), remoteHost on desktop apps, renderer secret getters, second watchdog, forced native API-key entry, silent breakaway, DevTools outside ViewRight content-only.
[ ] 3. After each publication: tell each app whose `readyForAdoption` list changed, quoting the manifest and snapshot SHA-256.
```
