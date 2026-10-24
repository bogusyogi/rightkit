# `rightkit-qa run-tests`: compile in the broker, run outside it

The broker compiles. It does not run end-to-end tests.

`rustright cargo test` holds the broker's compile slot for as long as the test binary runs. A scenario run takes minutes, and every build in the queue waits behind it (Pulse, 2026-10-23: 55 minutes with 15 builds queued). A real-app test cannot run there at all: the broker's context has no GUI session (macOS `open` exits 1) and a scrubbed environment. Five apps wrote the same two-step by hand: `rightkit/scripts/run-ui-tests.sh`, `postright/scripts/run-e2e.sh`, `viewright/scripts/qa-native.sh`, and Pulse's `scripts/gate.sh` `test_cargo` with `scripts/qa/run-test-bins.py`. `rightkit-qa run-tests` is that two-step as one command, with a receipt, so every app can delete its copy (plan item 2.4; gaps A-13, A-14, C4-13).

```sh
rightkit-qa run-tests --app genright -p genright-e2e --test fast_smoke -- --test-threads=1 --nocapture
```

## What it does

1. **Compile.** `cargo test --no-run --message-format=json <cargo arguments>`, through `rustright cargo` when `rustright` resolves on `PATH`. A host without it compiles with plain cargo (the cloud host), unless `--require-broker` / `[run_tests] require_broker` / `RIGHTKIT_QA_REQUIRE_BROKER=1` is set, which refuses instead. The argv is checked by the same validator `journeys run` uses (`journeys::build_record::validate_build_command`): compile-only, JSON messages, no `--target-dir`, `--out-dir` or `--artifact-dir`. Or `--from-cargo-json <file>` (`-` is stdin): compile nothing and take the executables of an existing listing (Pulse's gate compiles with its own wrapper and hands over the file).
2. **Select.** Every `compiler-artifact` record with `profile.test = true` and an `executable`: unit-test binaries (`lib`, `bin`), integration tests (`[[test]]`, `harness = false` ones included) and the benches cargo itself runs under `cargo test`. Build scripts, proc-macros, libraries and plain example binaries are not test executables. Duplicates collapse by path; the result is sorted by package and target, so one compile always gives one run order. The listing must end with a successful `build-finished` record, or it is not a complete compile (exit 2 for a listing, 3 for a compile).
3. **Run.** Each executable runs outside any broker job, from its package directory (the directory of the manifest that owns the target, as cargo does), `--parallel` (default 2) at a time, under its own deadline and a total one. Output goes straight to files. A deadline kills the whole process tree (a process group on Unix, a Job Object on Windows). If `run-tests` itself dies, the test binary is taken down with it (Windows: Job Object; macOS: a watchdog on the whole group; Linux: SIGTERM to the binary). Descendants still running when a binary exits on its own are recorded as `survivors` and killed.
4. **Record.** One JSON receipt (below), the cargo listing, each binary's `stdout`/`stderr`, and one summary line per binary on the terminal:

```
run-tests: 2 test binaries (parallel 2, 1800s per binary, 7200s total)
     Running genright-e2e::fast_cli_media (/…/target/debug/deps/fast_cli_media-3f2a…)
     Running genright-e2e::fast_smoke (/…/target/debug/deps/fast_smoke-91bc…)
run-tests: genright-e2e::fast_smoke exit=0 passed=12 failed=0 ignored=1 41.3s
run-tests: genright-e2e::fast_cli_media exit=101 passed=2 failed=1 ignored=0 52.8s
run-tests: genright-e2e::fast_cli_media: FAILED cli::media_roundtrip
run-tests: 2 test binaries: 1 passed, 1 failed, 0 timed out, 0 could not run, 0 not run (tests: 14 passed, 1 failed, 1 ignored)
run-tests: FAILED: genright-e2e::fast_cli_media failed (exit 101)
run-tests: exit 1
run-tests: receipt /…/runs/genright-run-tests/<run id>/receipt.json
```

A binary that did not pass also prints its failed test names and the end of its output; `--show-output` prints every binary's output in full.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Every binary passed, and the run rules held. |
| 1 | A binary failed, timed out, could not be started, or was not run because the total deadline passed. Also: `--deny-ignored` and a test was ignored; or every binary reported a summary and not one test ran (a filter that matches nothing is not a pass; `--allow-empty` accepts it). A binary that exits 0 over a `test result: FAILED` line of its own is a failure too. |
| 2 | Setup error: bad arguments, no app key, an unreadable listing, a listing that is not a complete compile, a compile that listed no test executable, a refused compile route. |
| 3 | The compile failed or timed out, or cargo's `build-finished` says it failed. Nothing ran. |

Every outcome after the run directory exists leaves a receipt, a failed compile and a setup error included.

## Options

Anything that is not one of these goes to `cargo test --no-run` untouched (`-p fx --test ui --features qa --locked --offline`). Everything after the first `--` goes to every test binary. Cargo's own `--config` therefore reaches cargo; this command's config-file flag is `--qa-config`. A flag wins over `[run_tests]`, which wins over the default.

| Flag | `[run_tests]` key | Default | |
|---|---|---|---|
| `--root <dir>` | | `.` | App root: where cargo runs and `cwd = "workspace"` points. |
| `--qa-config <file>` | | `<root>/rightkit-qa.toml` if present; `RIGHTKIT_QA_CONFIG` | A named file that is missing is an error. |
| `--app <key>` | | `app` of `rightkit-qa.toml`, else of `journeys.toml` at the root | Required; never guessed. Run storage uses `<app>-run-tests`. |
| `--from-cargo-json <file>` | | | Use an existing listing; compile nothing. Cargo arguments and `--build-with` are refused with it, not ignored. |
| `--build-with auto\|rustright\|cargo` | | `auto` | `auto`: the broker when on `PATH`, else plain cargo. |
| `--require-broker` | `require_broker` | off | No plain-cargo fallback. |
| `--parallel <n>` | `parallel` | 2 (1 to 32) | Binaries at the same time. |
| `--binary-timeout-s <n>` | `binary_timeout_s` | 1800 | One binary's deadline. |
| `--total-timeout-s <n>` | `total_timeout_s` | 7200 | All binaries together, from the start of the run phase (the compile is not counted). A binary running at the deadline is killed and reported `timed-out` with `timeout_kind: "total"`; binaries not yet started are `not-run`. |
| `--compile-timeout-s <n>` | `compile_timeout_s` | 2700 | The compile's deadline. |
| `--cwd package\|workspace\|<dir>` | `cwd` | `package` | `<dir>` is relative to `--root`. |
| `--env KEY=VALUE` (repeat) | `env` (table) | | Added to every binary's environment; a flag wins per key. Receipts record the names, never the values. |
| `--unset-env KEY` (repeat) | `unset_env` | `RIGHTKIT_FORWARD_ENV`, `RUSTC_WRAPPER` | Removed from every binary's environment before `env` is applied. A key in the config replaces the default list; a flag adds to it. |
| `--isolate-tmp` / `--no-isolate-tmp` | `isolate_tmp` | off | Each binary gets its own short temp directory (`<temp>/rkt-<8 hex>/<index>`, `TMPDIR`; `TEMP` and `TMP` on Windows), removed when it exits. Short on purpose: a macOS socket path is limited to 104 bytes. |
| `--deny-ignored` | `deny_ignored` | off | An ignored test is a test that did not run. |
| `--allow-empty` | | off | Accept a run in which no test ran. |
| `test_args` | `test_args` | none | Libtest arguments used when the command line has no `--`. An explicit `--` replaces them (an empty one too). |
| `--show-output` | | off | Print every binary's output. |
| `--managed-root <dir>` | | `RIGHTKIT_MANAGED_ROOT` or the platform default | Run storage root. |
| `--run-id <id>` | | a new id | Name the run directory. |
| `--receipt <file>` | | | Also copy the receipt here, for a gate that wants a fixed path. |

Environment the binaries get: the caller's, minus `unset_env`, plus `CARGO_MANIFEST_DIR` and `CARGO_PKG_NAME` (cargo sets both for a test process and tests read them at run time), plus the isolated temp directory, plus `env`. `RIGHTKIT_UNDER_TEST` is deliberately not set: it is a test-mode activation switch for the app under test, not for every unit test; an app that needs it declares it in `[run_tests] env`.

```toml
# rightkit-qa.toml
app = "genright"

[run_tests]
parallel = 2
binary_timeout_s = 1800
total_timeout_s = 7200
test_args = ["--test-threads=1"]
require_broker = true            # Mac and Dell: no plain-cargo fallback

[run_tests.env]
GENRIGHT_QA = "1"
```

`journeys.toml` does not carry run settings. An app with no `rightkit-qa.toml` passes `--app` and its flags.

## Run storage and the receipt

`<managed root>/runs/<app>-run-tests/<run id>/` holds `receipt.json`, `listing.jsonl` (the cargo listing), `compile.stderr.log`, `logs/<index>-<package>-<target>.stdout.log` and `.stderr.log`, and the ordinary `.rightkit-run.json` marker. The app key is `<app>-run-tests`, not `<app>`: retention keeps the newest passing run per app and kind and every failed run, so sharing the key would let a passing `run-tests` run delete the app's last passing journeys run. `rightkit-qa prune-runs` and the post-job prune treat these runs like any other.

`rightkit.run-tests.receipt/1`, snake_case, written whatever the outcome:

| Key | Content |
|---|---|
| `schema`, `run_id`, `app`, `started_at`, `finished_at`, `duration_ms`, `root`, `host` | Identity and timing. |
| `compile` | `source` (`broker`, `cargo` or `listing`), `managed`, `program`, `argv`, `exit`, `timed_out`, `duration_ms`, `request_id` (the broker line, when printed), `note` (why a non-broker route was used), `listing`, `listing_sha256`, `build_finished`, `test_binaries`. |
| `settings` | The merged settings: `parallel`, the three timeouts, `test_args`, `cwd`, `env_keys` (names), `unset_env`, `isolate_tmp`, `deny_ignored`, `allow_empty`, `require_broker`, `config` (the file used). |
| `binaries[]` | `index`, `package`, `target`, `kind`, `binary`, `sha256` (taken just before the run), `cwd`, `args`, `status` (`passed`, `failed`, `timed-out`, `error`, `not-run`), `exit`, `signal`, `timed_out`, `timeout_s` and `timeout_kind` (the deadline that applied), `started_after_ms`, `duration_ms`, `stdout`, `stderr`, `stdout_bytes`, `stderr_bytes`, `summary` (`results`, `ok`, `passed`, `failed`, `ignored`, `measured`, `filtered_out`, summed over the binary's `test result:` lines; `null` for a custom harness that prints none), `failed_tests` (at most 200), `survivors`, `tmpdir`, `error`. |
| `totals` | Binary counts by status, `without_summary`, and test counts summed over the summaries. |
| `verdict` | `passed`, `exit_code`, `reasons[]`. Computed from the facts above, never passed in. |

## Replacing the app copies

| Copy | Replace with |
|---|---|
| `rightkit/scripts/run-ui-tests.sh -p cutright-e2e --test ui -- --nocapture` | `rightkit-qa run-tests --app cutright -p cutright-e2e --test ui -- --nocapture` |
| `postright/scripts/run-e2e.sh scenarios` | `rightkit-qa run-tests --app postright -p postright-e2e --test scenarios -- --test-threads=1 --nocapture` (the package directory is `crates/e2e`, as its `cd` was) |
| `viewright/scripts/qa-native.sh` step 2 | `rightkit-qa run-tests --app viewright --manifest-path src-tauri/qa-e2e/Cargo.toml --test pdf_edit --test shell --test formats --test upgrade --env VIEWRIGHT_QA_BIN="$VIEWRIGHT_QA_BIN" -- --nocapture` (one compile; the staging of the app binary stays in the script) |
| Pulse `test_cargo` + `scripts/qa/run-test-bins.py` | `rightkit-qa run-tests --app pulse --root "$PULSE_ROOT" <cargo args> --env PULSE_TEST_CREDENTIALS=memory --binary-timeout-s "${PULSE_TEST_DEADLINE_S:-1800}" --receipt "$PULSE_SCRATCH/run-tests-$target_name.json" -- <test args>`; or keep Pulse's own compile and pass `--from-cargo-json "$listing"`. Pulse's NOT RUN accounting reads `totals.tests.ignored` from the receipt, or sets `--deny-ignored`. |
| GenRight `test:e2e` | `rustright cargo build -p genright-engine --features test-mode && rightkit-qa run-tests --app genright -p genright-e2e -- --test-threads=1` |
| ScrapeRight `qa:ui-native` | `rightkit-qa run-tests --app scraperight --locked -p scraperight-ui-e2e --test native_journey -- --ignored --nocapture --test-threads=1` |
| HeardRight `qa:native:journey` | `rightkit-qa run-tests --app heardright --manifest-path ./qa-e2e/Cargo.toml -- --nocapture` |
| CutRight `scripts/test.sh` | `rustright cargo test --workspace --exclude cutright-ui-e2e --features cutright-engine/test-mode` (simple tests, in the broker) and `rightkit-qa run-tests --app cutright -p cutright-ui-e2e` (outside it) |

## Not covered

- **Doctests.** `cargo test --no-run` builds none and cannot run them. Run `cargo test --doc` in the broker (short) or not at all.
- **`cargo run` binaries** (CodeRight's `coderight-qa` runner). This command runs test executables. Those E2E binaries go to `rightkit-qa journeys run` (ADR-0001) or need a `--bin` mode that does not exist yet.
- **Windows and macOS.** The Windows Job Object, the macOS watchdog and file-redirected stdio on Windows come from `rightkit-child-process` and were not exercised on this cloud (Linux) host. The integration tests in `tests/run_tests.rs` run on every host and are the check; the Unix-only ones are marked.
- **Wiring `right-release test`.** A `cargo` suite kind in `packages/release/test-command.mjs` that calls this command is a follow-up (A-13 fix text).

## Check rule G22 (`e2e-under-cargo-test`)

For the integrator, to add to `packages/release/app-check.mjs` (which `scripts/preflight.mjs` calls). The rule text, the detection heuristic and the fixtures below were run as a throwaway prototype over the nine app checkouts (see "Measured").

C4-13 calls this rule G24; the plan and A-13 number it G22, and G23 (mandatory set) and G24 (exception ledger) are taken. Use G22.

### Rule text

> **G22 e2e-under-cargo-test.** A command that executes the tests, or a binary, of an end-to-end crate through `cargo test`, `cargo run` or `cargo nextest` (plain, or through the RustRight client as `rustright cargo`, or as `rightkit` for as long as the retired alias lasts) is a finding. The broker compiles; it does not run end-to-end tests. Compile the crate with `--no-run` (or `cargo build`) and run it outside the broker with `rightkit-qa run-tests` (or `rightkit-qa journeys run`).
>
> Where: a `package.json` script, a shell, PowerShell, Node or Python script, a `Makefile` or `justfile` recipe, a GitHub workflow `run:` step, a git hook, or a command in a runbook document.
>
> Finding text: `${file}:${line} ${what} runs the end-to-end crate ${crate} under cargo test (${via}): the test binary runs in the broker's context and holds its compile slot for the whole run`
>
> Fix text: `compile it with --no-run and run it outside the broker: rightkit-qa run-tests -p ${crate} [--test <target>] -- <libtest args>`
>
> Identity (ratchet, no line numbers): `e2e-under-cargo-test|${file}|${subject}`, where `subject` is the script name for a `package.json` script and the crate name for anything else. Ratcheted per app like G13 to G21: `ci/app-check-baseline.json` lists today's findings, the list only shrinks, a listed finding that is gone is STALE.

### Detection heuristic

An **end-to-end crate** is a package (a `Cargo.toml` with `[package]`) that satisfies both:

1. It depends on `rightkit-qa` **not optionally**: in `[dependencies]`, `[dev-dependencies]` or a `[target.*]` table of either, without `optional = true` (resolve `package = "rightkit-qa"` renames). An optional dependency is the in-app control server behind the app's `qa-native` / `dev-control` feature; the app crate itself is not an end-to-end crate (the `src-tauri` crate of all nine apps declares it that way; PostRight's also has a dev-dependency, used only for `rightkit_qa::workspace`, which criterion 2 excludes).
2. A `.rs` file of that crate (not under `examples/`) references a **driver** module: `rightkit_qa::` followed by `harness`, `control`, `suite`, `scenario`, `engine`, `in_app`, `interactive`, `page`, `browser` or `android`, or a `use rightkit_qa::{...}` list that names one. The helper modules (`mock`, `util`, `media`, `loudness`, `evidence`, `workspace`, `process`, `storage`, `settings`) and the static `guard` do not count: PostRight's `postright-mock-platform` (`rightkit_qa::mock`), CutRight's `cutright-engine` tests and PostRight's `src-tauri/tests/supervisor.rs` (`rightkit_qa::workspace`) are ordinary integration tests.

A crate's name (`*-e2e`, `qa-*`) is not evidence; print crates that match only by name for a reviewer.

Signals that are **not** enough on their own, so the rule must not use them: a file under `tests/` (every crate has integration tests), `harness = false` (G14 requires it of any custom `main`), `required-features` (GenRight's `paid` and `local-models` gate E2E and ordinary tests alike), `#[ignore]` (ScrapeRight's E2E are `#[ignore]` and run with `--ignored`; ordinary crates ignore slow tests too). Inside an end-to-end crate every integration test target (`tests/*.rs`, `[[test]]`) and bin is end-to-end, except a target whose name is `qa_surface_guard`, which only runs the static guard (the `--test` selector of the command decides; `-p` alone selects the whole crate). The `[[test]]` names the apps use, for fixtures: GenRight `fast_*`, `local_*`, `paid_*`, `installed_app`; PostRight `scenarios`, `ui`; ViewRight `pdf_edit`, `shell`, `formats`, `upgrade`; HeardRight `installed_app_ax`, `native_pill_regression`; ScrapeRight `native_journey`, `native_shell_real`; CutRight `batch_flow`, `edit_shots`, `scopes_round_trip_flow` and others.

A **command** is a match of `(?:(?:rustright|rightkit)\s+)?cargo\s+(?:\+\S+\s+)?(test|run|nextest)\b` followed by the rest of the command up to `&`, `;`, `|` or a backtick, in:

- `package.json`: each value of `scripts`; the cwd is the directory of that `package.json`.
- Script files and workflow steps: every line that is not a comment (`#`, `//`, `REM`, `::`, `;`); the cwd is the repository root unless an earlier `cd` or `Set-Location` in the file says otherwise.
- Runbook documents: the text of fenced code blocks and of inline code spans in `README.md`, `AGENTS.md`, `CLAUDE.md` and `docs/**/*.md`. Excluded as history: `docs/archive/**`, `docs/plans/**`, `docs/provenance/**`, `docs/handoff*/**`, any file with a date in its name, `CHANGELOG.md`, `PENDING.md`, `STATUS.md`, `state.md`, `tasks/**`, `**/evidence/**`. Prose outside code is never a command.

The command is **exempt** when it contains `--no-run`, or `rightkit-qa run-tests`, or `rightkit-qa journeys run`. `cargo build`, `check` and `clippy` are not matched.

The command **selects** an end-to-end crate `C` when any of these holds:

1. `-p C`, `--package C` or `--package=C`.
2. `--manifest-path P`, with the directory of `P` (relative to the cwd, then to the repository root) equal to the directory of `C`. Or equal to the root of a Cargo workspace, with `--workspace` or `--all` and no `--exclude C`, and `C` a member (see 4).
3. `--workspace` or `--all` with no `--manifest-path`, and no `--exclude C`, where the cwd holds a `[workspace]` of which `C` is a member (see 4).
4. Membership: the crate directory matches a `[workspace] members` glob relative to the workspace root, is not under `exclude`, and does not itself contain a `[workspace]` table (a nested `[workspace]` makes it its own root: Pulse `hub/qa-e2e`, ViewRight `src-tauri/qa-e2e` and CodeRight `qa-e2e` are not members of the workspace around them).
5. `--test T` and no `-p` / `--manifest-path`, where `T` is a test target of exactly one end-to-end crate: a `[[test]] name`, or a file `tests/T.rs` / `tests/T/main.rs`.
6. For `cargo run` only: `--bin B` where `B` is a `[[bin]]` (or `src/main.rs`, `src/bin/B.rs`) of an end-to-end crate.

Known limits, deliberate for an offline check: no `cargo` binary, so path dependencies that cargo adds to a workspace implicitly are not seen; a command assembled from variables (`$CMD`, `"${cargo_args[@]}"`) is matched only on the literal text; a `package.json` script that calls another script is judged at the script that holds the `cargo` text; a cargo alias (`cargo t`) is not matched (the broker forbids `.cargo/config.toml` aliases).

### Fixtures

The lines are quoted with the client spelled `rustright`; most of the checkouts still spell it with the retired alias (P11), and the rule matches both names until that sweep ends.

Positive (each must produce one finding):

| Fixture | Why |
|---|---|
| GenRight `package.json:48` `test:e2e`: `rustright cargo build -p genright-engine --features test-mode && rustright cargo test -p genright-e2e -- --test-threads=1` | `-p`; the `cargo build` before `&&` is not matched. Also `:50` `test:e2e:paid`. |
| ScrapeRight `package.json:52` `qa:ui-native`: `rustright cargo test --locked -p scraperight-ui-e2e --test native_journey -- --ignored --nocapture --test-threads=1` | `-p`; `--ignored` does not exempt. Also `:86`. |
| HeardRight `tauri-app-next/package.json:20` `qa:native:journey`: `rustright cargo test --manifest-path ./qa-e2e/Cargo.toml -- --nocapture` | `--manifest-path` resolved against the directory of that `package.json`. Also `:14`, `:15`, `:17`. |
| CodeRight `apps/coderight-tauri/package.json:93` `qa:native`: `rustright cargo build ... && rustright cargo run --manifest-path qa-e2e/Cargo.toml --bin coderight-qa` | `cargo run`. Also `:94`. |
| CutRight `scripts/test.sh:9`: `exec rustright cargo test --workspace --features cutright-engine/test-mode "$@"` | `--workspace` and `cutright-ui-e2e` is a member. |
| PostRight `scripts/test.sh:6`: `exec rustright cargo test --workspace --no-fail-fast ...` | The same, `postright-e2e` is a member. |
| MailRight `docs/QA-HIDDEN-BROWSER.md:74`: ``Run it with `rustright cargo test --manifest-path qa/native-e2e/Cargo.toml` `` | An inline code span in a runbook. |

Negative (each must produce no finding):

| Fixture | Why |
|---|---|
| PostRight `scripts/run-e2e.sh:17`: `out="$(rustright cargo test -p postright-e2e --test "$target" --no-run 2>&1)"` | `--no-run`: compile only. |
| PostRight `package.json:21` `test:rust`: `... rustright cargo test --workspace --exclude postright-e2e --no-fail-fast` | `--exclude`. `postright-mock-platform` is a member but only uses `rightkit_qa::mock`. |
| ViewRight `scripts/qa-native.sh`: the `:12` comment line `# ... rustright cargo test --manifest-path src-tauri/qa-e2e/Cargo.toml`, and the `--no-run` command at `:59` | A comment is not a command; `--no-run`. |
| ViewRight `package.json:52` `verify:rust`: `rustright cargo test --manifest-path src-tauri/Cargo.toml --workspace` | `src-tauri/qa-e2e` has its own `[workspace]`; `src-tauri` depends on `rightkit-qa` optionally. |
| Pulse `scripts/gate.sh:75`: `cargo test --no-run --message-format=json "${cargo_args[@]}"` | `--no-run`; the executables run through `run-test-bins.py` (and after migration through `rightkit-qa run-tests`). |
| CutRight `crates/cutright-engine` (`rightkit-qa` dependency, `workspace::create` only) | Not an end-to-end crate. |

### Measured

The prototype over the nine checkouts on 2026-10-24 (pulse e39b3d1, postright a7fb583, viewright d1bac5a, genright 114071d, scraperight 472eb26, heardright 6ba4bba, coderight 9e2b2ff, cutright f18ce87, mailright 1691092) found these end-to-end crates: `pulse-hub-qa`, `postright-e2e`, `viewright-qa`, `genright-e2e`, `scraperight-ui-e2e`, `heardright-native-e2e`, `coderight-qa-e2e`, `cutright-ui-e2e`, `mailright-native-e2e`. The `package.json` / script findings are the 13 in the positive table (GenRight 2, ScrapeRight 3 (`:44` `verify:rust` is the third, `cargo test --locked --workspace`), HeardRight 4, CodeRight 2, CutRight 1, PostRight 1); runbook documents add 15 (GenRight 7, CutRight 3, PostRight 2, ScrapeRight 2, MailRight 1; `AGENTS.md`, `README.md`, `docs/E2E.md`, `docs/QA.md`, `docs/agent-rules.md` and `docs/QA-HIDDEN-BROWSER.md`). Pulse and ViewRight have none: they already compile with `--no-run` and run the binaries themselves. After the apps migrate (plan 3.5) every count is zero, which is the acceptance for A-14.
