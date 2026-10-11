# Desktop app template (Rust + Tauri + React)

Desktop counterpart of `swift/ios-app-template`. A minimal, buildable skeleton of the Right Suite
desktop pattern as it exists in the mature apps (ScrapeRight is the reference; ViewRight, HeardRight,
GenRight and CutRight agree on the shape). It carries **no product behaviour and no real brand**.

The template **consumes** shared RightKit packages and crates by exact published version. It never
copies them. Nothing in it is a stand-in for shared code: every shared capability it uses is pinned to
the version `packages/release/rightkit-versions.json` lists as `registry` (published).
[`placeholders.json`](placeholders.json) lists only what remains genuinely unshipped (packaging scripts
and the updater endpoint/key).

## What is in it

| Area | Files | RightKit capability consumed |
|---|---|---|
| Frontend | `src/`, `vite.config.ts`, `index.html` | `@rightkit/app-shell` 0.4.1 (`AppShell`: titlebar, caption controls, layout; bridge to the window), `@rightkit/platform-ui` (platform + QA bootstrap), `@rightkit/logs` (renderer events), `@rightkit/updates` (patch/update tier policy), `@rightkit/tauri` (typed host adapter; add when wiring license/device) |
| Shell (host) | `src-tauri/` (`tauri.conf.json`, capabilities, `build.rs`, `lib.rs`, diagnostics) | `rightkit-shell` =0.1.4 (hosts the window: hardened webview, window state, single instance, watchdog; full `Shell::builder(..).plugin()`), `rightkit-service` =0.1.3 (`Suite` data root), `rightkit-tauri` =0.1.2 (`DeviceService`), `rightkit-logs` =0.2.0 (JSONL + redacted diagnostics) |
| Supervisor | `src-tauri/src/lib.rs` (`engine_config`, `engine_call`) | `rightkit-jobs` =0.2.6 (`EngineSupervisor`: owned sidecar, restart budget, per-call deadline, no replay); `rightkit-process` =0.3.3 (`OwnedCommand` spawn) |
| Engine sidecar + CLI | `crates/engine`, `crates/protocol` | `rightkit-jobs` =0.2.6 serve contract: one `Dispatcher` for `--stdio` frames and the one-shot CLI; durable `JobStore` and `JobHandler` for job kinds |
| Release | `right-release.config.mjs`, `package.json` scripts | `@rightkit/release` 0.2.151 (`right-release build|upload|dev|doctor`) |
| Brand | `assets/brand/brand.json` -> `src/brand.css` | token file generated into Tailwind v4 `@theme` |
| E2E | `rightkit-qa.toml`, `tests/scenarios/`, `crates/engine/tests`, `src-tauri/tests` | Rust `rightkit-qa` engine black-box scenarios (real binary, isolated data dir, muted audio) plus real-process Rust tests of the supervisor; no Node harness |
| Helpers + gate | `crates/tools` (`rightapp-tools brand-sync|stage-engine|check|init`) | Rust helpers; `check` rejects Node harnesses, loose or non-published pins, and any placeholder whose package is listed in `rightkit-versions.json` |

Unlicensed: `@rightkit/license`, `@rightkit/legal`, `@rightkit/legal-ui` are an add-on (below).

## Who supervises, who hosts

- **`rightkit-jobs` supervises the engine.** `EngineSupervisor` (in `src-tauri/src/lib.rs`) owns the
  `rightapp-engine` child through `rightkit-process`: it boots it at setup, restarts it on demand within
  5 per minute, and bounds each call with a deadline. A call that outlives its deadline kills the engine
  and fails; it is never replayed, because the command may already have run.
- **`rightkit-shell` hosts the window.** `Shell::builder("rightapp").plugin()` installs the hardened
  webview, navigation policy, window state, single-instance arbitration and the hang watchdog. Its
  `engine` feature is off: it is a thin client host with no restart budget or lifecycle, so enabling it
  would run a second, weaker supervisor beside `EngineSupervisor`.

## Data root

`rightapp_protocol::app_data_root()` is the one data root for the shell and the engine:
`<suite>/runtime/rightapp/data`, owner-only, where `<suite>` comes from `rightkit_service::Suite::resolve()`
(the platform default, or `RIGHTKIT_SUITE_ROOT`). The shell stores window state under its `shell`
subdirectory and device identity and logs under the root. The engine keeps its job store in `engine/`.
The shell passes no app-specific variable to the engine. `RIGHTAPP_APP_DATA` remains only as the QA
harness override (`rightkit-qa.toml` `[engine] data_dir_env`).

## Create a new app (checklist)

Work from the **primary checkout** of the new app's repository; do not create branches or worktrees
without Adrian. Read `docs/rules/release-signing.md` before step 8 and `docs/rules/rightkit.md`
before any Rust command.

1. **Copy and rename.** Copy this directory to the new repo root, then
   `rightkit cargo run -q -p rightapp-tools -- init --slug <lowercase> --name "<Product Name>" --org <reverse.dns.prefix>`.
   It rewrites `RightApp`/`rightapp`/`RIGHTAPP`/identifier and runs the gate.
2. **Brand.** Run `/brand <code>`; put the approved palette in `assets/brand/brand.json`
   (token names `brand`, `brand-*`), then `pnpm brand:sync`. Add the real logo/icon master,
   then `pnpm tauri icon <master.png>` (replaces the placeholder icons and adds `icon.icns`).
3. **Toolchain and install.** Check `packageManager`/`engines`/`rust-toolchain.toml` match the
   workspace defaults, then `pnpm install` (never npm/npx). The template is its own pnpm workspace
   (`packages: ["."]`), so the lockfile stays local. Commit `pnpm-lock.yaml` and `Cargo.lock`.
4. **Pins.** Keep every `@rightkit/*` exact (no `^`, no git/link/file/workspace) and equal to its
   `registry` version; keep every `rightkit-*` crate `=x.y.z` at its `registry` version in
   `packages/release/rightkit-versions.json`. `pnpm check:template` verifies this inside the RightKit
   checkout. When bumping an npm pin, keep the old version in `minimumReleaseAgeExclude` in
   `pnpm-workspace.yaml` until the install has rewritten `pnpm-lock.yaml` with the new pin (the old lock
   entry would otherwise fail the minimum-release-age policy); then add the new version and remove the old.
   `check:template` warns about an exclusion whose version the lockfile no longer contains.
   Each `tauri-plugin-*` crate is pinned `=x.y.z` to the same version as its npm counterpart
   (`@tauri-apps/plugin-*`, exact); `check` fails on any mismatch, so `right-release dev` never meets two
   Tauri plugin generations.
5. **Engine.** Staging the sidecar is two top-level managed calls, never one that nests a broker call
   (a managed RightKit request refuses nested broker calls, exit 125). `pnpm build:engine` (release) runs
   `rightkit cargo build -p <engine> --release` then `rightkit cargo run -p rightapp-tools -- stage-engine --release`;
   `pnpm build:engine:dev` does the same with `--profile release-iterate`. `stage-engine` makes no process
   calls: it finds the built engine beside its own tools binary and copies it to
   `src-tauri/binaries/<engine>-<triple>`. `right-release dev` runs `tauri build`; its dev config overrides
   `beforeBuildCommand` to `pnpm build && pnpm build:engine:dev`, so dev uses `release-iterate` and release
   uses `--release`. Domain work is a job kind: add the kind to `DomainJobs` in `crates/engine/src/main.rs`
   (the `job.submit` path). A synchronous command goes in `DomainCommands` (the same table feeds the
   CLI and the sidecar). Keep names in `crates/protocol` (`METHOD_*`, `JOB_KIND_*`). Large payloads go
   through files, not frames. Document the CLI contract (`docs/CLI-<NAME>.md`) before other apps call it.
6. **Shell.** Keep it a thin host. Register new `#[tauri::command]`s in `lib.rs` through
   `rightkit_shell::handler![...]` (it adds the shell's own commands; the full plugin needs it). Add each
   plugin's permission to `capabilities/default.json` (the gate checks the registered plugins). Set
   `assetProtocol.scope` and the CSP `connect-src` to exactly what the app needs.
7. **Updater.** Replace the `PLACEHOLDER` endpoint and `pubkey` in `tauri.conf.json` with the app's
   manifest URL and the suite updater public key (copy from a shipped app; never generate keys).
8. **Release.** In `right-release.config.mjs` choose exactly one `distribution.provider`
   (`github-releases` public, `r2-rightapps` private), set artifact names, and replace
   `scripts/package-placeholder.mjs` (Windows NSIS packaging) with GenRight's `win-nsis.mjs`, and add
   `install-macos-development.mjs`. The mac signing contract (`macos-developer-id-notarized-portable-v1`)
   is already wired through `scripts/build-mac-notarized.sh`; the win contract
   (`windows-raw-exe-authenticode-before-nsis-v1`) builds the raw exe in `prePackage`. See "Release config
   migration" in `docs/RIGHTKIT-SHARED-PLATFORM.md`. `pnpm check:release-pin` asserts the exact
   `@rightkit/release` pin matches the installed, registry-listed version. Run `pnpm release:doctor`.
   Signing/notarization is provisioned: never run `az login` or browser auth.
9. **Licensed app add-on** (ScrapeRight/ViewRight/HeardRight pattern): add exact pins for
   `@rightkit/license`, `@rightkit/legal-ui`, dev `@rightkit/legal`, and crates `rightkit-license`;
   add `@tauri-apps/plugin-http`, `tauri-plugin-http` and the `http:default` capability scoped to the
   brand host; swap `FileDeviceStore` in `lib.rs` for a keyring-backed `SecureStore`; add
   `legal/legal-manifest.json` and the `legal` block in `right-release.config.mjs`.
10. **Media app add-on**: add `@rightkit/media-ui` (listed in `rightkit-versions.json`, 0.3.2) at its exact
    version, import `@rightkit/media-ui/styles.css`, and mount the player or viewer in a view. Native
    playback uses `rightkit-playback`.
11. **E2E.** Scenarios are `tests/scenarios/*.scenario.toml` run by `rightkit-qa` (`pnpm test:e2e`): outcome-level
    assertions on real engine output; fast tier only (no models, no network, no audio). A scenario that
    cannot run fails the run unless it declares `optional = true`. Never make a billed call; paid
    providers are exercised only against local HTTP mocks. Native UI scenarios drive the hidden app
    through `rightkit-control` (add the shell `qa-native` feature; see `crates/rightkit-qa/examples`).
    There is no browser/Node UI harness and no in-page IPC mock.
12. **Adopt shared pieces as they land.** When a package or crate becomes listed in
    `rightkit-versions.json`, the gate fails until you pin it exactly, delete its stand-in, and delete its
    `placeholders.json` entry. `@rightkit/app-shell` and `rightkit-jobs` are adopted.
13. **Verify before first commit:** `pnpm check:template`, `pnpm typecheck`, `pnpm build`,
    `rightkit cargo test`, `pnpm test:e2e`.

## Verified for this checkout

Ran here: `pnpm install --frozen-lockfile=false`, `pnpm run typecheck`, `pnpm exec vite build`.
Not yet run (RightKit broker reported `RK_DATA_ROOT_INITIALIZING` for `/Volumes/D/.rightkit-managed`, so
no Cargo command could be admitted): `pnpm run check:template`, `pnpm run test:rust`, `pnpm run build`.
Rust code in `crates/` and `src-tauri/` has not been compiled yet. Not exercised: packaging, `pnpm test:e2e`
(rightkit-qa fast tier), native UI scenarios.
