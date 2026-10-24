# Desktop app template (Rust + Tauri + React)

Desktop counterpart of `swift/ios-app-template`. A minimal skeleton of the Right Suite desktop pattern: a hidden Tauri window
hosted by RightKit's shell, a sidecar engine that is also a headless CLI, and every release, QA and legal declaration a new app
owes before it can ship. It carries **no product behaviour and no real brand**.

The template **consumes** RightKit packages and crates by exact published version and never copies them. Every pin equals the
`registry` version in `packages/release/rightkit-versions.json`, and a new app starts with the whole foundation set declared (the
mandatory set, `packages/release/ownership-mandatory-set.json`), no ownership exception and no baseline.
`packages/release/desktop-template.test.mjs` instantiates the template like a new app and checks all of it (see "What is checked").

## What is in it

| Area | Files | RightKit owner |
|---|---|---|
| Frontend | `src/`, `vite.config.ts`, `index.html` | `@rightkit/react-shell` (`AppShell`), `@rightkit/platform-ui`, `@rightkit/tauri-shell` (the shell client: window controls and engine calls), `@rightkit/tauri` (device client), `@rightkit/logs` (renderer events), `@rightkit/theme` (brand tokens) |
| Shell host | `src-tauri/` | `rightkit-tauri-shell` (window host, engine supervision, renderer log commands, watchdog stall capture), `rightkit-device` with `rightkit-credentials` (device identity), `rightkit-crash` (process hook), `rightkit-daemon` (mode), `rightkit-fs` (data root) |
| Engine sidecar and CLI | `crates/engine`, `crates/protocol` | `rightkit-job-queue` (the serve contract: one command table for the stdio frames and the CLI, durable job store), `rightkit-sidecar`, `rightkit-db`, `rightkit-crash`, `rightkit-daemon`, `rightkit-fs` |
| Release | `right-release.config.json`, `switches.json`, `src-tauri/tauri.sidecar.conf.json` | `@rightkit/release`: data config (M24), sidecar staging, `winPackage` / `macPackage` recipes |
| Legal | `legal/`, `legal-snapshot.config.json`, `src/generated/legal-contract.json` | `@rightkit/legal` (`legal-snapshot`), `@rightkit/legal-ui`, `@rightkit/onboarding`; the licence texts are byte copies of the suite masters in `legal/` of RightKit |
| QA | `rightkit-qa.toml`, `journeys.toml`, `tests/scenarios/` | `rightkit-qa` (`journeys run`), `@rightkit/qa` |
| Local gate | `.githooks/pre-push`, `package.json` `prepare` | `rightkit-gate install-hook` |
| Ownership | `rightkit-mandatory.json`, `.rightkit/ownership-exceptions.json` | `right-release ownership-mandatory`, the ownership scanner |

Nothing here is app-resident release, signing, packaging or gate code: there is no `scripts/` directory, no helper crate and no
`.mjs` config. An app declares; RightKit does.

## The foundation set

Every row of the desktop profile is declared. **Wired** means the template uses it; **declared** means the dependency is pinned
and its native wiring belongs to the lane named in the comment next to it in `src-tauri/Cargo.toml`, because a native licence,
update, settings or legal-receipt service needs the app's own trust roots (the licence host and the updater key) and a host build.

| Row | Owner | State |
|---|---|---|
| M1 window host, M2 app frame | `rightkit-tauri-shell`, `rightkit-device`, `@rightkit/tauri`, `@rightkit/react-shell`, `@rightkit/platform-ui`, `@rightkit/theme` | wired |
| M3 engine supervision, M4 protocol, M5 child processes | `rightkit-job-queue`, `rightkit-sidecar`, `rightkit-child-process` | wired |
| M6 data location, M7 local services, M8 safe writes | `rightkit-daemon`, `rightkit-fs` | wired (mode and data root) |
| M9 database | `rightkit-db` | declared by the engine (its job store is SQLite through `rightkit-job-queue`) |
| M10 settings | `rightkit-app-settings`, `@rightkit/settings-ui` | declared |
| M11 secrets | `rightkit-credentials` | wired (device identity) |
| M12 licence and device, M13 entitlement | `rightkit-license`, `@rightkit/license` | declared |
| M14 legal and onboarding | `@rightkit/legal`, `@rightkit/legal-ui`, `@rightkit/onboarding` | the legal snapshot is wired; the in-app gate is declared |
| M15 logging, M16 crash | `rightkit-logs`, `@rightkit/logs`, `rightkit-crash` | wired |
| M17 HTTP | `rightkit-http` | declared |
| M18 updates | `@rightkit/app-update` | declared (no updater key or endpoint exists until the trust roots lane) |
| M19 release tooling, M20 QA | `@rightkit/release`, `rightkit-qa`, `@rightkit/qa`, `@rightkit/qa-chrome` | wired |
| M22 agent surface | `rightkit-agent-tools` | not applicable, declared with its reason in `rightkit-mandatory.json` |

## Who supervises, who hosts

- **`rightkit-tauri-shell` hosts the window and owns the engine.** `Shell::builder` installs the hardened webview, window state,
  single-instance arbitration, the watchdog and the renderer log commands. With its `engine` feature the shell starts
  `rightapp-engine --stdio` off the UI thread, answers the renderer's `rk_shell_engine_call` under a native method allowlist
  (`doctor` and `echo`; every other method is denied), restarts a dead engine on the next call, never replays a call that was lost
  in flight, and shuts the engine down on exit. Add an app method to `EnginePolicy::allow_methods` to expose it to the renderer.
- **The window starts hidden** (`visible: false`); the shell reveals it when its webview is ready. Window controls and engine calls
  reach the host through `@rightkit/tauri-shell`, so the capability file needs no window permission beyond window dragging.
- **The engine is the product's work.** Add a job kind to `run_kind` in `crates/engine/src/main.rs` (`job.submit` path) or a
  synchronous command to `DomainCommands` (the same table feeds the CLI and the sidecar). Keep names in `crates/protocol`. Large
  payloads go through files, not frames.

## Data root and test isolation

`rightapp_protocol::app_data_root()` is the one data root for the shell and the engine: `rightkit-fs` `AppDirs`, resolved from the
operating system's account (never from the environment) under `Orthic Labs/RightApp` (`RightAppDev` in a debug build), made
owner-only. There is **no app-owned environment variable** for the data root, the engine path or QA: a variable that selects a data
root or an executable is refused in a release build by the switch scanner (`switches.json` lists none), and the cargo profile contract
refuses an environment override gated by `debug_assertions`. Test isolation is the shared `test-mode` cargo feature: the engine
built with `--features test-mode` (QA only, never in `default`, refused by `right-release` in a release or update build) reads the
isolated home that `rightkit-qa` binds per scenario, and `qa-native` in `src-tauri` turns on the same feature for UI scenarios.

## Create a new app (checklist)

Work from the **primary checkout** of the new app's repository; create no branches or worktrees without Adrian. Read
`docs/rules/release-signing.md` before any signing step and `docs/rules/rightkit.md` before any Rust command.

1. **Copy and rename.** Copy this directory to the new repository root, then run `right-release init --name "<Product Name>" --id <reverse.dns.id>`
   from it (`--slug` if the lowercase name is not the slug). It rewrites the `RightApp` / `rightapp` / `RIGHTAPP` identity and the bundle id,
   re-identifies `Cargo.lock` so `--locked` accepts it, and recomputes the digests in `legal/legal-manifest.json`. Until the template pins a
   `@rightkit/release` that carries these steps, run it from the RightKit checkout: `node <rightkit>/packages/release/cli/right-release.mjs init --dir .`.
   It prints the steps below that no command can do for you.
2. **Brand.** Run `/brand <code>`, put the approved palette in `assets/brand/brand.json`, then `pnpm brand:sync` (`rightkit-theme generate`;
   `pnpm brand:check` verifies it). Add the real icon master and run `pnpm tauri icon <master.png>`.
3. **Install.** `pnpm install` (never npm or npx). `prepare` runs `rightkit-gate install-hook`, which sets `core.hooksPath` to the committed
   `.githooks`; commit them with executable mode. The pre-push hook runs the local gate; never bypass it.
4. **Pins.** Keep every `@rightkit/*` exact and every `rightkit-*` crate `=x.y.z`, both equal to the `registry` version in
   `packages/release/rightkit-versions.json`, and keep `minimumReleaseAgeExclude` in `pnpm-workspace.yaml` equal to the `@rightkit` pins.
   Never edit `Cargo.lock` by hand: after a manifest change refresh it with `cargo update -p <crate> --precise <version>` and commit it.
5. **Engine and sidecar.** `right-release stage-sidecar --target <triple>` builds the engine through the broker and stages it as
   `src-tauri/binaries/rightapp-engine-<triple>`. Release and dev builds merge `src-tauri/tauri.sidecar.conf.json`, which adds
   `bundle.externalBin`, so a plain `cargo check`, clippy or test never needs a staged binary. `targets.<os>.externalBin` in the release
   config makes `right-release` sign the sidecar with the app.
6. **Shell.** Register new `#[tauri::command]`s through `rightkit_tauri_shell::handler![...]` and grant each plugin's permission in
   `capabilities/default.json`. Set `assetProtocol.scope` and the CSP `connect-src` to exactly what the app needs.
7. **Release config.** In `right-release.config.json` choose exactly one `distribution.provider` (`github-releases` public, `r2-rightapps` private;
   init leaves the provider alone), set artifact names, and read "Release config (JSON)" in `packages/release/README.md`. The config is data:
   no environment, absolute path or script. Signing and notarization are provisioned: never run `az login` or browser auth.
8. **Legal.** Set the app's public legal host (the `publicUrl` of each document in `legal-snapshot.config.json`; it is a reserved example host
   here) and replace `legal/PRODUCT_SCHEDULE.md` and `legal/PRIVACY_NOTICE.md` with reviewed text: they are the unmodified template text and
   have not been reviewed by counsel. Refresh with `pnpm exec legal-snapshot` (add `--source-root <rightkit>/legal` to re-copy the suite masters)
   and `right-release license-notices npm`; `pnpm exec legal-snapshot --final` is the release-time proof. The material documents carry no product
   name on purpose: their digest is hashed into `src/generated/legal-contract.json`, which a rename must not change. Notices for Rust crates
   and shipped programs come from `right-release license-notices cargo` and `binaries` on a build host.
9. **Licence, updates, settings.** The packages are declared. Wire them with the per-app trust roots (licence host, updater key and endpoint)
   through the shell's `host-services`, `license` and `update` features; do not generate keys and do not hand-write a stand-in.
10. **E2E.** Scenarios are `tests/scenarios/*.scenario.toml` run by `rightkit-qa journeys run` (build in the broker, run outside it); never
    through `cargo test`. Every unit test carries `// rk-test: pure` directly above its attribute stack and touches no process, file or network
    (`rightkit-qa journeys check` freezes the untagged count at 0). A scenario that cannot run fails unless it declares `optional = true`.
    Never make a billed call. QA control never ships: `qa-native` is off by default and `right-release` refuses a seal that contains it.
11. **Register the repository on each host** (a person, not an agent). The ownership scanner refuses a release root outside its registry once
    protected enrollment is complete. Add the repository to `packages/release/ownership-repositories.json` in RightKit (the byte copy under
    `packages/build-control-policy/src` follows through `rightkit-sync`), then on the Mac and on the Dell run
    `right-release ownership trust-refresh --dry-run`, read the additions, and run `right-release ownership trust-refresh`. It needs a person with
    administrator rights at that machine (a terminal with `sudo` on macOS, an elevation prompt on Windows) and a clean tree at the committed
    revision. Details: `docs/OWNERSHIP-ENFORCEMENT.md`.
12. **Verify before the first commit.** `right-release doctor --platform win` and `--platform mac`, `pnpm exec tsc --noEmit`, `pnpm brand:check`.
    On a host: `rustright cargo test --locked`, `rightkit-qa journeys check`, `right-release dev --platform <os>`.

## What is checked

`packages/release/desktop-template.test.mjs` copies this directory into a new git repository, runs `init`, commits it, and runs against the
result: the pins (npm, crates, the lockfile in cargo's order, the release-age allowance), the mandatory set (only M22 is not applicable), the
release config through the M24 loader and the pipeline scan, the legal gate (digests equal the bytes, the material documents carry no product
name), the QA lane, the switches inventory, the cargo profile contract, the committed hook (exactly what `install-hook` writes, executable),
the journeys manifest (every test tagged, no process launched from a test), RightKit's own preflight rules with an empty baseline, and the
doctor's file rows for both release platforms.

## Verified for this checkout

Verified without a build host: the contract test above, `pnpm exec tsc --noEmit`, `pnpm exec vite build`, `pnpm brand:check`, the
regenerated `Cargo.lock` (resolved against crates.io by `cargo update -p <crate> --precise <version>`, which proves every feature name in
the manifests) and `cargo metadata --no-deps`. **Not verified:** no Rust code in `src-tauri/` or `crates/` has been compiled or run, the
Tauri runtime, the engine launch and its `rk_shell_engine_call` allowlist, window dragging, `rightkit-qa journeys check` and `run`,
installer packaging, `right-release dev` and the host rows of the doctor that need a Mac or a Dell. Those are the first runs on a host.
