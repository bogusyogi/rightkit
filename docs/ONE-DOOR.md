# One door: `rightkit-gate`

Adrian, 2026-10-25: "only a single entry point into builds and CI … whether we are on local or we are on cloud … one and only
one way to build. Anything else gets rejected or doesn't work. If it's local, it goes through the broker. If it's on cloud, it
doesn't." and "delete all the previous callers, all gates gone, combine them into 1".

This file is the contract every change in this branch implements. Nothing here is optional.

## The door

`rightkit-gate` (bin of `@rightkit/release`, `packages/release/bin/rightkit-gate.mjs`) is the ONLY command that checks, compiles,
lints, tests or admits anything. Every other caller is deleted or becomes a one-line call of `runGate()`.

```
rightkit-gate [--all | --base <ref>] [--commit <rev>] [--stage A|B|all] [--receipt <file>] [--plan] [--pre-push] [--no-cache]
rightkit-gate install-hook [--force]
rightkit-gate --version
```

- `--all`: every tracked file counts as changed (full-tree run). Default: `--base origin/<default branch>`.
- `--commit <rev>`: check that exact commit (exported with `git archive`), not HEAD. Used by the broker's admission.
- `--stage A`: class A only (file-only, no compile, no network beyond the pinned advisory DB). `B`: class B (requires a green
  class A receipt for the same commit+tree in the same run or in `--receipt`). `all` (default): A then B.
- `--receipt <file>`: write the receipt there (always written to `<git common dir>/rightkit-gate/receipts/<commit>.json` too).
- The old `rightkit-gate check` subcommand is REMOVED: app-check rules (`app-check.mjs` RULES) run as class A scans inside the
  plan (one scan row per rule set, `id: "app-check"`), never as a separate command.

Exported API (same file): `export async function runGate({ root, all, base, commit, stage, receiptFile, env, spawn }) → { ok, receipt }`.
Library callers (right-release build/release/publish, scripts) import this; they never plan or run scans themselves.

## Executor (chosen inside the door, never by the caller)

`packages/release/gate-executor.mjs`, `export function resolveExecutor({ env, platform, exists, spawn }) → { kind, cargo, problem }`:

1. `rustright` on PATH → `{ kind: "broker", cargo: ["rustright", "cargo"] }`. Every cargo call goes through the broker.
2. else `brokerManagedHost(env, platform)` (cargo-contract.mjs: Mac/Dell markers, broker socket, managed shim) → `{ kind: "refuse",
   problem: "this is a managed host and the RustRight broker is not running; start it — the gate never compiles around it" }`.
3. else (cloud session, CI runner, any unmanaged host) → `{ kind: "direct" }`: the gate provisions the tools in
   `packages/release/qualify-tools.json` (download, sha256 verify, cache under the shared cache root, prepend to PATH; Rust minimum
   checked with `rustc --version`; Linux libraries checked with `pkg-config --exists`, a missing one is a refusal naming the
   `apt-get install` line from the manifest), then writes a run-scoped cargo shim that runs `cargo-guard.mjs` with
   `RK_GATE_RUN=<nonce>` and puts it first on PATH for the scans.

## Lockout (everything else is rejected)

- `cargo-guard.mjs`: a heavy cargo command (anything outside LIGHT_COMMANDS) is REFUSED unless `RK_GATE_RUN` names a live run
  file `<cache root>/gate-runs/<nonce>` written by the gate (pid + commit), or the broker invokes it (`RIGHTKIT_BUILD_BROKER_SOCKET`
  set by rustright). Message: "cargo is reachable only through rightkit-gate".
- CI: every workflow job that checks, compiles, lints, tests or admits runs `rightkit-gate …` and nothing else for that purpose.
  `scripts/ci/workflow-contract.mjs` gains rule `one-door`: a `run:` step invoking cargo, rustc, rustright, actionlint,
  cargo-deny, cargo-audit, clippy, `pnpm test`/`pnpm run test`, tsc, eslint, biome, or any deleted script is a contract failure.
  Toolchain setup steps (setup-node, pnpm/action-setup, rust-toolchain) stay.
- The broker (RustRight `check_admission.rs`) admits a compile only for a commit with a green `rightkit-gate --stage A --commit
  <sha>` receipt; a missing gate, a missing receipt, a red or mismatched receipt, or `report`/`off` modes REFUSE. No bypass mode.

## Receipt (the one evidence format)

`packages/release/gate-receipt.mjs`: `writeReceipt(r)`, `readReceipt(file, { commit, tree, stage })` → `{ receipt } | { problem }`.
Schema `rightkit.gate-receipt/1`: `{ schema, commit, tree, stage, executor: { kind, cargo, tools: {name: version+sha256} },
rust, node, pnpm, scans: [{ id, class, status, ms, detail? }], ok, startedAt, finishedAt, gate: { version, digest } }`.
`ok` is true only when every planned scan of the stage passed (deferred, not-run, skipped, cached-from-another-commit = not ok).
Publication, release, host qualification and broker admission accept ONLY this receipt via `readReceipt`; every other evidence
format (scan-receipt, gate-evidence, pipeline receipts, local-admission, semantic-admission, publication-gate proofs) is deleted.

## Deleted (callers rewired to `runGate` / `readReceipt`)

RightKit repo scripts: `scripts/local-gate.mjs`, `scripts/preflight.mjs`, `scripts/ci/gate.mjs`, `scripts/ci/gate-evidence.mjs`,
`scripts/ci/rk-preflight.mjs`, `scripts/ci/rk-npm-preflight.mjs`, `scripts/ci/run-deterministic-scans.mjs`,
`scripts/ci/ci-scans-contract.mjs` (its contract moves into deterministic-scans tests), `scripts/ci/scan-receipt.mjs`,
`scripts/ci/local-admission.mjs`, `scripts/ci/semantic-admission.mjs`, `infrastructure/release-preflight/`.
`@rightkit/release`: `preflight.mjs`, `prebuild-gates.mjs`, `source-gate.mjs`, `release-checks.mjs`, `local-qualify.mjs`,
`pipeline-admission.mjs`, `publication-gate.mjs`, `admission.mjs` (its non-gate helpers, if any, move to the module that uses
them); `right-release` subcommands `check`, `doctor`, `qualify`, `test`, `gate`, `preflight` are removed (they print "use
rightkit-gate" and exit 2). `right-release build|release|publish|dev` call `runGate({ stage: "all" })` first and stop on `!ok`.
Every deleted file gets a row in `docs/provenance/retirements.md`-style list in `docs/ONE-DOOR.md#retired` (path → successor).

## Retired
(filled by each lane: `path → successor`)
