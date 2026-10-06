# Desktop app template (Rust + Tauri + React)

Desktop counterpart of `swift/ios-app-template`. A minimal, buildable skeleton of the Right Suite
desktop pattern as it exists in the mature apps (ScrapeRight is the reference; ViewRight, HeardRight,
GenRight and CutRight agree on the shape). It carries **no product behaviour and no real brand**.

The template **consumes** shared RightKit packages and crates by exact published version. It never
copies them. Anything that cannot be consumed yet is a clearly marked `PLACEHOLDER` and is listed
once in [`placeholders.json`](placeholders.json).

## What is in it

| Area | Files | RightKit capability consumed |
|---|---|---|
| Frontend | `src/`, `vite.config.ts`, `index.html` | `@rightkit/platform-ui` (platform + QA bootstrap), `@rightkit/logs` (renderer events), `@rightkit/updates` (patch/update tier policy), `@rightkit/tauri` (typed host adapter; add when wiring license/device) |
| Shell | `src-tauri/` (`tauri.conf.json`, capabilities, `build.rs`, supervisor, diagnostics) | `rightkit-process` (owned child + bounded exit), `rightkit-logs` (JSONL + redacted diagnostics), `rightkit-tauri` (`DeviceService`) |
| Engine sidecar + CLI | `crates/engine`, `crates/protocol` | one binary: `--stdio` framed sidecar and one-shot CLI over one command table |
| Release | `right-release.config.mjs`, `package.json` scripts | `@rightkit/release` (`right-release build|upload|dev|doctor`) |
| Brand | `assets/brand/brand.json` -> `src/brand.css` | token file generated into Tailwind v4 `@theme` |
| E2E | `rightkit-qa.toml`, `tests/scenarios/`, `crates/engine/tests`, `src-tauri/tests` | Rust `rightkit-qa` engine black-box scenarios with content assertions (real binary, isolated data dir, muted audio) plus real-process Rust tests of the supervisor; no Node harness |
| Helpers + gate | `crates/tools` (`rightapp-tools brand-sync|build-engine|check|init`) | Rust replacements for the former Node helpers; `check` also rejects Node harnesses, loose pins and stale placeholders |

Gated placeholders (staged in `rightkit-versions.json` but not yet on the registry, so not consumable by exact pin): `@rightkit/app-shell`, `@rightkit/media-ui`, `rightkit-framed-sidecar` (crates/protocol/src/framed.rs), `rightkit-jobs`, `rightkit-db`, plus packaging scripts and updater endpoint/pubkey. The gate fails the moment one is published while its stand-in remains.
Unlicensed: `@rightkit/license`, `@rightkit/legal`, `@rightkit/legal-ui` are an add-on (below).

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
   workspace defaults, then `pnpm install` (never npm/npx). Commit `pnpm-lock.yaml` and `Cargo.lock`.
4. **Pins.** Keep every `@rightkit/*` exact (no `^`, no git/link/file/workspace) and every
   `rightkit-*` crate `=x.y.z`. Align them with `packages/release/rightkit-versions.json`
   (`pnpm check:template` verifies this when run inside the RightKit checkout). When bumping an npm
   pin also bump `minimumReleaseAgeExclude` in `pnpm-workspace.yaml`.
5. **Engine.** Add product commands to `dispatch()` in `crates/engine/src/main.rs` (one table feeds
   CLI and sidecar). Keep schemas in `crates/protocol`. Large payloads go through files, not frames.
   Document the CLI contract (`docs/CLI-<NAME>.md`) before other apps call it.
6. **Shell.** Keep it a thin supervisor. Register new `#[tauri::command]`s in `lib.rs` and add each
   plugin's permission to `capabilities/default.json` (the gate checks the registered plugins).
   Set `assetProtocol.scope` and the CSP `connect-src` to exactly what the app needs.
7. **Updater.** Replace the `PLACEHOLDER` endpoint and `pubkey` in `tauri.conf.json` with the app's
   manifest URL and the suite updater public key (copy from a shipped app; never generate keys).
8. **Release.** In `right-release.config.mjs` choose exactly one `distribution.provider`
   (`github-releases` public, `r2-rightapps` private), set artifact names, and replace
   `scripts/package-placeholder.mjs` with the packaging pair used by GenRight
   (`mac-dmg.mjs`, `win-nsis.mjs`, `sign-macos-bundle.mjs`, `install-macos-development.mjs`).
   Run `pnpm release:doctor`. Signing/notarization is provisioned: never run `az login` or browser auth.
9. **Licensed app add-on** (ScrapeRight/ViewRight/HeardRight pattern): add exact pins for
   `@rightkit/license`, `@rightkit/legal-ui`, dev `@rightkit/legal`, and crates `rightkit-license`;
   add `@tauri-apps/plugin-http`, `tauri-plugin-http` and the `http:default` capability scoped to the
   brand host; swap `FileDeviceStore` in `lib.rs` for a keyring-backed `SecureStore`; add
   `legal/legal-manifest.json` and the `legal` block in `right-release.config.mjs`.
10. **Media app add-on**: add `@rightkit/media-ui` once it is in `rightkit-versions.json`
    (import `@rightkit/media-ui/styles.css`); native playback uses `rightkit-playback`.
11. **E2E.** Scenarios are `tests/scenarios/*.scenario.toml` run by `rightkit-qa` (`pnpm test:e2e`): outcome-level
    assertions on real engine output; fast tier only (no models, no network, no audio). A scenario that
    cannot run fails the run unless it declares `optional = true`. Never make a billed call; paid
    providers are exercised only against local HTTP mocks. Native UI scenarios drive the hidden app
    through `rightkit-control` (add the shell `qa-native` feature; see `crates/rightkit-qa/examples`).
    There is no browser/Node UI harness and no in-page IPC mock.
12. **Adopt shared pieces as they land.** When `@rightkit/app-shell`, `rightkit-jobs`,
    `rightkit-framed-sidecar` (published) or `rightkit-db` appear in `rightkit-versions.json`:
    pin exactly, delete the stand-in named in `placeholders.json`, delete the entry, run the gate.
13. **Verify before first commit:** `pnpm check:template`, `pnpm typecheck`, `pnpm build`,
    `rightkit cargo test`, `pnpm test:e2e`.

## Verified for this checkout

- `rightkit cargo test --manifest-path Cargo.toml`: supervisor deadline/restart/no-replay and restart budget
  (real shell-script engines), engine CLI+sidecar contract, gate negative cases (Node harness, `^` pin,
  loose script) and `init` producing a passing renamed app.
- `rightkit-qa run --tier fast --engine <built engine>` passes `tests/scenarios/engine-blackbox`.
- Not exercised here: `pnpm install`, `tsc`, `vite build`, packaging, native UI scenarios.
