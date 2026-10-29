# Desktop app template (Rust + Tauri + React)

The starting point of a Right Suite desktop app: a small, buildable skeleton of the pattern the mature apps share
(ScrapeRight is the reference). It carries **no product behaviour and no real brand**, and it never copies shared code:
every RightKit capability it uses is an exact pin from the published set (`packages/release/rightkit-versions.json` `registry`), and the app
declares the whole desktop mandatory set (`packages/release/ownership-mandatory-set.json`, profile `desktop`) from its first commit.

A freshly instantiated app passes, with an empty baseline: `rightkit-gate check` (the offline app rules, `pnpm check`; the published
`@rightkit/release` has carried it since 0.6.5, bogusyogi/rightkit#1148), the files and host rows of `right-release doctor`, the pipeline scan,
the release-switch scan, the Cargo, QA and legal contracts and `right-release ownership-mandatory --enforce`.
`packages/release/template-contract.test.mjs` runs those evaluators on a fresh instance on every change to the template or to
`right-release init`.

**Pin.** The pin is the latest published `@rightkit/release`, 0.6.11: the registry check refuses a pin the registry does not have,
the build refuses one behind its latest, and `template-contract.test.mjs` requires every pin to equal the published set. The `check`
command (`pnpm check`) ships from 0.6.6, and the JSON release config is loaded by every command from 0.6.6, so `pnpm release:*` and
`pnpm dev:build:*` run from the pinned package.

**Pin sequencing.** This template pins `@rightkit/release` 0.6.11, the latest published release; RightKit's own tree is at 0.6.12 (unpublished). The template's
commands wait for 0.6.12 only for what that release adds (dev, test and package run the compile-tier app rules): everything the
template itself runs is in 0.6.11. The pin moves to 0.6.12 in the commit after it is published, and this paragraph is deleted in that commit.

The template commits no git hook: commit and push run no gate and no local hook check (`docs/GATE-CONTRACT.md`). `prepare`
runs `rightkit-gate install-hook`, which writes no hook; it removes a `.githooks/pre-push`, forwarder or ownership pre-commit hook that an
earlier release wrote (matched by its own header only, never a foreign hook) and registers the one-door agent hook.

**Installing inside the RightKit repository.** This directory sits inside RightKit's own git repository, so `prepare`
(`rightkit-gate install-hook`) acts on the repository at that root, not on the template: it would register the agent hook in RightKit's
`.claude/settings.json` and edit RightKit's local git hook configuration. Install here with `pnpm install --ignore-scripts`, and never run
`rightkit-gate install-hook` from this directory. `pnpm check` here likewise checks RightKit's
committed files, not the template's; `template-contract.test.mjs` runs `rightkit-gate check` on a fresh instance instead. An app copied into its own
repository installs normally; delete this paragraph there.

## What is in it

| Area | Files | What it does |
|---|---|---|
| Frontend | `src/`, `vite.config.ts`, `index.html` | `@rightkit/react-shell` `AppShell` (titlebar, caption controls, layout), `@rightkit/platform-ui`, `@rightkit/logs` renderer events, `@rightkit/app-update` update tiers, `@rightkit/tauri` + `@rightkit/tauri-shell` host adapters, `@rightkit/theme` brand tokens; first run (`src/Setup.tsx`: `@rightkit/onboarding`, `@rightkit/legal-ui`, `@rightkit/legal`), Settings (`@rightkit/settings-ui`), licence (`@rightkit/license`), all over the shell's host services (`src/lib/services.ts`) |
| Shell (host) | `src-tauri/` | `rightkit-tauri-shell` hosts the window (hardened webview, window state, single instance, tray and menu hooks, renderer log commands) and registers the settings, legal-receipt and licence services under one caller policy (`src/services.rs`); `rightkit-device` keeps the device identity in the OS credential store; `rightkit-crash` installs the process hook first |
| Supervisor | `src-tauri/src/lib.rs` (`engine_config`, `engine_call`) | `rightkit-job-queue` `EngineSupervisor` owns the engine sidecar: restart budget, per-call deadline, no replay of a timed-out call |
| Engine + CLI | `crates/engine`, `crates/protocol` | the engine serves the `rightkit-job-queue` contract (one command table for `--stdio` frames and the one-shot CLI); `rightapp-protocol` holds the app id, method names and the data root |
| Release | `right-release.config.json`, `package.json` scripts | JSON-only release config (M24): declarative `winPackage` / `macPackage` recipes, `sidecars` staging, dev installers, `legal`, `qa`. No script, no `.mjs` config |
| Legal | `legal/`, `legal-snapshot.config.json`, `src/generated/legal-contract.json` | the six legal documents, the generated manifest and acceptance contract, passing the release legal contract (M12, M14) |
| QA | `rightkit-qa.toml`, `journeys.toml`, `tests/scenarios/`, `src-tauri/tests/` | declared user journeys run against the real built app on isolated data (M20, M25); every Rust test is tagged `// rk-test: pure` or `contract` |
| Switches | `switches.json` | the release switch inventory (M23): every environment read in the sources, with the build feature that compiles it out |
| Gate | `prepare` and `check` scripts | `pnpm install` retires the git hooks an earlier release wrote and registers the agent hook (A-16); no git hook runs a gate; `pnpm check` runs the offline app rules on demand |
| Line endings | `.gitattributes` | LF in every checkout, Windows included: the legal documents are hashed byte for byte, `Cargo.lock` and `pnpm-lock.yaml` are held by `--locked`, and any shell script runs under `sh` |
| Mandatory set | `rightkit-mandatory.json`, `.rightkit/ownership-exceptions.json` | the desktop profile declaration and an empty exception ledger |
| Advisory policy | `deny.toml`, `.cargo/audit.toml` | cargo-deny and cargo-audit policy; the ignores mirror RightKit's `ci/advisory-exceptions.json` |
| Brand | `assets/brand/brand.json` -> `src/brand.css` | token file generated into Tailwind v4 `@theme` (`pnpm brand:sync`) |

## Foundation owners

Every desktop row of the mandatory set is declared, so a new app starts complete, and every owner except the database is wired in
code. `rightkit-db` is **declared only** until the engine has product state; it is listed in `placeholders.json` `declaredOnly`, and the
template contract test fails the moment it is wired but still listed.

| Rows | Owner | State |
|---|---|---|
| M1 | `rightkit-tauri-shell`, `rightkit-device`, `@rightkit/tauri` | wired |
| M2 | `@rightkit/react-shell`, `@rightkit/platform-ui`, `@rightkit/theme` | wired |
| M3, M4, M5 | `rightkit-job-queue` (supervisor and contract), `rightkit-child-process` | wired; `rightkit-sidecar` is declared on the engine (the framed transport the contract serves through) |
| M6, M7 | `rightkit-daemon` (`mode`: release or test data root) | wired |
| M8 | `rightkit-fs` (private data directory) | wired |
| M11 | `rightkit-credentials` | wired (through `rightkit-device`) |
| M15, M16 | `rightkit-tauri-shell` `logs`, `@rightkit/logs`, `rightkit-crash` | wired |
| M18 | `@rightkit/app-update` | wired (`src/lib/updates.ts`) |
| M19 | `@rightkit/release` | wired (config, scripts) |
| M20 | `rightkit-qa`, `@rightkit/qa`, `@rightkit/qa-chrome` | wired (`qa-native` feature, journeys) |
| M10 | `rightkit-app-settings`, `@rightkit/settings-ui` | wired: the settings document under the data root behind the shell's `rightkit_settings_*` commands (`src-tauri/src/services.rs`), the Settings view (`src/App.tsx`) |
| M14 | `@rightkit/legal`, `@rightkit/legal-ui`, `@rightkit/onboarding` | wired: the first-run agreement gate (`src/Setup.tsx`) records the acceptance in the shell's native legal receipt, which checks it against the generated manifest and contract compiled into the shell and stamps the time itself |
| M12, M13 | `rightkit-license`, `@rightkit/license` | wired: the native licence client behind the `rightkit_license_*` commands, the onboarding licence step and the Settings licence section. Disabled (`mode: disabled`, no network) until the app sets `LICENSE` in `src-tauri/src/services.rs` to its registered appKey, host and public key (bogusyogi/rightkit#272) |
| M17 | `rightkit-http` | wired: `AppHttp`, the app's first-party client pinned to the licence host (HTTPS only); none while licensing is disabled |
| M9 | `rightkit-db` | **declared only** (the engine opens no database yet) |
| M22 | `rightkit-agent-tools` | wired: the engine's CLI contract, `rightapp-engine contract` (every tool with its effect and approval; `crates/engine/src/main.rs`), role `host` in `rightkit-mandatory.json` |

Journeys J-002 and J-003 drive the first run (gate, receipt, settings, licence status, relaunch) in the real app; the shell's
`services` tests hold the receipt, settings and HTTP rules without a window.

## Who supervises, who hosts

- **`rightkit-job-queue` supervises the engine.** `EngineSupervisor` owns the `rightapp-engine` child: it boots it at setup, restarts
  it on demand within 5 per minute, and bounds each call with a deadline. A call that outlives its deadline kills the engine and
  fails; it is never replayed, because the command may already have run.
- **`rightkit-tauri-shell` hosts the window.** `Shell::builder("rightapp").plugin()` goes first, so single-instance arbitration runs
  before any other plugin. Commands are registered through `rightkit_tauri_shell::handler![...]`, which adds the shell's own.

## Data root

`rightapp_protocol::app_data_root()` is the one data root for the shell and the engine. It goes through `rightkit_daemon::mode`: a
release build resolves the product directory and nothing in the environment can move it; only a `test-mode` build (QA) binds it to
the scenario's isolated directory, through `RIGHTKIT_QA_DATA_DIR`. The shell keeps window state under `shell/`, logs under `logs/` and
crash reports under `crashes/`; the engine keeps its own files beside them. The shell passes no app-specific variable to the engine.

## QA, journeys and what never ships

- `journeys.toml` declares the app's proof: J-001 (the engine answers its command table as a sidecar and as a CLI) and J-002 (the app
  launches hidden, supervises its engine and answers the doctor call from the window). `pnpm test:e2e` builds the frontend and runs
  `rightkit-qa journeys run`.
- The QA build is the `qa-native` feature of `src-tauri` (control server, test-mode data root, `RIGHTAPP_ENGINE_BIN`) and `test-mode` of
  the engine. They are off by default, `right-release` refuses to seal a release or update whose graph or binary carries them, and
  `switches.json` records that the environment reads exist only behind `qa-native`. A new environment read needs an entry there.
- Rust tests must be tagged (`// rk-test: pure` for logic with no process, `contract` for a real child process behind a contract); a
  test never launches the app itself. `journeys.toml` `[tests] untagged_baseline = 0`.
- There is no Node UI harness and no in-page IPC mock.

## Create a new app (checklist)

Work from the **primary checkout** of the new app's repository. Read `docs/rules/release-signing.md` before step 9 and
`docs/rules/rightkit.md` before any Rust command.

1. **Copy and rename.** Copy this directory to the new repository root and `git init`. From there run
   `node <rightkit checkout>/packages/release/cli/right-release.mjs init --name "<Product Name>" --id <reverse.dns.bundle.id>`
   (`--slug`, `--dir` and `--dry-run` are optional). It rewrites the template identity in text files, re-sorts `Cargo.lock` so
   `cargo --locked` accepts it, re-hashes the legal manifest and prints the steps below that only a person can do. It keeps the
   `r2-rightapps` distribution service name.
2. **Install.** `pnpm install` (never npm or npx); the template is its own pnpm workspace. `prepare` registers the agent hook and writes no git hook.
   Commit `pnpm-lock.yaml` and `Cargo.lock`.
3. **Doctor.** `pnpm release:doctor`. Every files and host row must pass before you build anything.
4. **Register the repository on each host.** Add it to `packages/release/ownership-repositories.json` in RightKit (and its byte copy
   under `packages/build-control-policy`), then run `right-release ownership trust-refresh` on the Mac and on the Dell. That needs a
   person with administrator rights; an agent cannot do it.
5. **Brand.** Run `/brand <code>`; put the approved palette in `assets/brand/brand.json`, then `pnpm brand:sync`. Add the real icon
   master and run `pnpm exec tauri icon <master.png>` (replaces the placeholder icons and adds `icon.icns`).
6. **Legal.** Set the app's public legal host in `legal-snapshot.config.json`, replace `legal/PRODUCT_SCHEDULE.md` and
   `legal/PRIVACY_NOTICE.md` with reviewed text, then `pnpm exec legal-snapshot` (it regenerates `legal/legal-manifest.json` and
   `src/generated/legal-contract.json`). The release legal contract refuses placeholder markers and a missing third-party notice.
7. **Pins.** Keep every `@rightkit/*` and `rightkit-*` pin exact and equal to the published registry; the `rightkit-registry` row of
   `right-release doctor` verifies them, and `right-release deps --check` / `--audit` report outdated packages and advisories. When bumping an npm pin, keep the old version in `minimumReleaseAgeExclude` in `pnpm-workspace.yaml` until the install
   has rewritten `pnpm-lock.yaml`, then remove it. Each `tauri-plugin-*` crate is pinned to the same version as its npm counterpart.
8. **Engine, shell, journeys.** Domain work goes in `crates/engine` behind the contract (names in `crates/protocol`); keep the shell a
   thin host and add each plugin's permission to `capabilities/default.json`; add a journey to `journeys.toml` for every release-critical
   flow, and an entry to `switches.json` for every new environment read.
9. **Updater and release.** Replace the endpoint and `pubkey` in `src-tauri/tauri.conf.json` (the suite updater public key, copied from
   a shipped app; never generate keys). In `right-release.config.json` set the artifact names and, if the app is public,
   `distribution.provider` to `github-releases`. Signing and notarization are provisioned: never run `az login` or browser auth.
10. **Verify before the first commit.** `pnpm typecheck`, `pnpm build`, `rustright cargo test --manifest-path Cargo.toml`,
    `pnpm test:e2e`, `pnpm release:doctor`, `pnpm dev:build:win` or `pnpm dev:build:mac` on the matching host.

`placeholders.json` lists what an app must still replace before it can ship.

## Gate and advisories

Commit and push run no gate (the gate is for building: `rightkit-gate --to compile`); CI runs the checks on every push. `deny.toml` and `.cargo/audit.toml` are this workspace's
advisory policy. The ignores in them (glib on Linux only; `zip` through `tauri-plugin-updater`) mirror RightKit's
`ci/advisory-exceptions.json`, with the same review date: drop them when the dependency moves on, and add none without recording why
the advisory does not apply. After a `cargo update`, re-check the lock against the advisory database.
