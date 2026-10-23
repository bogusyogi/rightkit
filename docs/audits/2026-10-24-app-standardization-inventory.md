# App standardization and enforcement inventory, 2026-10-24

Read-only inventory of the nine app repositories and RightKit, from local clones: rightkit `a55fbc5`, CutRight `da2dd09`,
CodeRight `b5c8295`, GenRight `5068597`, HeardRight `89568a9`, MailRight `2751430`, PostRight `e6088ef`, Pulse `ffd1799`,
ScrapeRight `c9c411f`, ViewRight `5ce8404`. Nothing was built or run; "inferred" marks a claim drawn from Tauri, Cargo or
webview behaviour rather than read in source. Each gap maps to an invariant of [`PIPELINE-ARCHITECTURE.md`](../PIPELINE-ARCHITECTURE.md).

## 1. QA builds and dev servers

The recipe (`crates/rightkit-qa/README.md:197-206`, example `examples/tauri-custom-protocol/rightkit-qa.toml:26-35`):
build the frontend into `build.frontendDist`; build the app with `--features qa-native,custom-protocol` (without
`custom-protocol` a debug binary loads `devUrl`); launch it hidden (`control::launch`, `ui_launch`, or `journeys run`).

| App root | Dev server in the QA path | `custom-protocol` in the QA build | Launcher | e2e under `cargo test` | Fix |
|---|---|---|---|---|---|
| HeardRight `tauri-app-next` | **yes**: `qa:native` is `tauri dev --features qa-native,test-mode` (package.json:18; Vite on :1421, tauri.conf.json:7-8); `qa:native:app` (:19) and `qa:macos-installed:build` (:16) build without it, so the binary loads :1421 (inferred) | no (no feature, src-tauri/Cargo.toml:19-30) | `control::launch` (qa-e2e/src/lib.rs:169) | yes (3 files) | add `custom-protocol = ["tauri/custom-protocol"]`; `pnpm build &&` before both builds, features `qa-native,test-mode,custom-protocol`; retire `qa:native`; update `scripts/test-macos-installed-ax-contract.mjs:70` |
| CodeRight `apps/coderight-tauri` | **yes**: `qa:native`/`qa:visual` (package.json:93-94) build without it and without a frontend build, so the app loads `http://127.0.0.1:1432` (inferred); `qa:installed:*` (:106-107) start Vite | no (src-tauri/Cargo.toml:22-23) | `run_suite` (qa-e2e/src/main.rs:50) | runner itself runs inside the broker | add `tauri/custom-protocol` to `qa-native`; `pnpm build` first; run the runner outside the broker |
| GenRight | web only (`qa:browser`); default binary path `target/debug/genright.exe` and skip text point at a plain build (ui_real_path.rs:83-99, inferred to load devUrl :5174) | via `tauri build` (inferred) | `control::launch` | yes (`rustright cargo test -p genright-e2e`) | resolve the binary with `binary::resolve`; fix the skip text |
| MailRight | web (`qa:journeys`, `qa:browser` run Vite with mocked IPC) | yes (`qa-native` enables it, Cargo.toml:20-26) | `control::launch` | one file, no script runs it | name the web lanes as web; add a script for `qa/native-e2e` |
| ScrapeRight | no | yes (`qa-native` enables it) | `control::launch` | yes | script the documented frontend + app build (docs/QA.md:350-351) |
| Pulse `hub` | no | yes (gate.sh:376) | `control::launch` | yes, outside the broker (gate.sh:427) | plain cargo (stated exception) |
| PostRight | no | yes (`qa-native` enables it) | `run_suite` | built `--no-run`, run outside the broker | none for the dev server |
| CutRight `apps/cutright` | no | via `tauri build` (inferred) | `control::launch`; Windows drives the installed copy | yes (40 tests, inside the broker) | build through the broker |
| ViewRight | no for native | yes (`qa-native` includes it) | `control::launch` | built `--no-run`, run outside the broker | none material |

No app has a `journeys.toml`; none runs `rightkit-qa journeys run`.

What lets a dev-mode binary through `rightkit-qa` (P5, P7):
- `guard` never reads tauri.conf.json or checks `custom-protocol`, and skips scripts named `qa`, `qa:*`, `test`, `dev`
  (src/guard.rs:394-399), so `qa:native: tauri dev` passes.
- `control::launch` treats any non-blank, loaded page as ready (src/control.rs:1679-1687): a dev-mode binary times out
  after `startup_timeout` instead of failing at once by name.
- `journeys check` refuses dev servers only for adapter journeys, only for argv words `vite`/`vitest`/`storybook`
  (src/journeys/manifest.rs:486-500); `tauri dev` and `pnpm dev` pass. The `[build]` table has no frontend step
  (manifest.rs:131-140), so recipe step 1 cannot be declared.
- The recipe is contradicted in RightKit itself: `examples/genright/rightkit-qa.toml:25` builds with `qa-native` only;
  `desktop-app-template/src-tauri/Cargo.toml:75-76` defines neither `qa-native` nor `custom-protocol`.

## 2. Enforcement matrix

| Rule (where written) | Enforced by, at which stage | Apps violating |
|---|---|---|
| Release config is data (PIPELINE §6, P8) | not enforced: `release.mjs:186` imports and runs each `.mjs` config | all 9, the template |
| No app release, signing or packaging code (AGENTS, PIPELINE §6) | not enforced (ownership fingerprints for codesign/notarytool are `advise`) | CutRight, HeardRight, Pulse, CodeRight, ScrapeRight, template |
| Published `@rightkit/*` only, no git/link/file/workspace specs | `release.mjs:1045-1049`, `cargo-contract.mjs:81`; build, doctor | none |
| `switches.json` required | `prebuild-gates.mjs:114-117`; build, `doctor --gates`; stops at the first finding | missing in 5 apps; invalid in PostRight and Pulse |
| DevTools only through `devtools: "content-only"` | `devtools-policy.mjs:80`; build, `doctor --gates` | ViewRight (declaration missing) |
| Line endings honour `.gitattributes` | `scripts/preflight.mjs` eol-policy, RightKit only (`--repo` runs it anywhere) | CodeRight, GenRight (55 files), HeardRight, MailRight, ScrapeRight |
| Check before build, dev and doctor (P1) | not for apps: `rightkit-gate` has no `check` profile; hooks are opt-in and absent | all 9 |
| Every finding in one run (P3) | not enforced: the doctor and the build exit at the first problem | all 9 |
| Compile only through the broker | the host shim and the agent policy hook | Pulse, MailRight, CutRight, CodeRight-iOS use plain cargo in scripts |
| One client name, `rustright` (P11) | preflight `retired-alias`, the agent policy hook, `rightkit-qa` build validation, `pipeline-admission` (this branch) | none after the sweep on `claude/hopeful-albattani-loe9db` |
| No e2e under `cargo test` (P7) | lint rules exist (`rightkit-qa` src/journeys/lint.rs:18-25) but run only through `journeys check`, which no app has | 7 fully, PostRight and ViewRight partly |
| Release QA admission refuses (P5) | `qa-admission-policy.json:3-4` lists no apps and no approved runners: warn only | all 9 |
| Legal manifest | checked only when `config.legal` exists (`release.mjs:269-271`) | PostRight, Pulse, CodeRight-iOS, GenRight (manifest, no `legal` block) |
| Explicit `distribution.provider` | at upload only (`publish-update.mjs:50`, `github-upload-gate.mjs:33`) | PostRight (invalid value), Pulse (public provider, private repo) |
| One suite updater key from the OS store | `sign-updater.mjs:23` refuses `--key-file`, but `release.mjs:630` still passes it: the Windows build fails mid-signing | CodeRight, MailRight, ScrapeRight, ViewRight |
| No QA, test or dev code in a release build | feature and binary scans (`prebuild-gates.mjs:47,72`); `sidecars[].features` and `RIGHTKIT_BUILD_MODE=dev` are not read (inferred) | GenRight, MailRight, Pulse, ScrapeRight, PostRight |

## 3. Per-app deviations

- **CutRight**: config computes version, absolute paths (`resolveTargetRoot`), `process.arch`, `LOCALAPPDATA`, and stages
  from outside the app (`../../tools/ffmpeg-editing/stage.mjs`); hand-rolled DMG and NSIS signing (scripts/mac-dmg.mjs,
  sign-macos-bundle.mjs, win-nsis.mjs); no `switches.json`; plain cargo in scripts/gate.sh:122-132.
- **CodeRight**: config imports an app script, reads `LOCALAPPDATA`, points its updater key at ViewRight's folder; a second
  headless config with a hard-coded version; `RIGHT_RELEASE_MAC_SIGN_ONLY=1` in package.json; no `switches.json`; iOS config
  uses bare cargo and inputs outside the app root.
- **GenRight**: `RIGHTKIT_BUILD_MODE` and `RIGHTKIT_CARGO_PROFILE` in config; dev builds turn on `test-mode`; legal manifest
  without a `legal` block; no `switches.json`; pins behind (`@rightkit/license` 0.2.1, `@rightkit/settings-ui` 0.6.1).
- **HeardRight**: environment-overridable absolute paths outside the repo (`HR_FFMPEG_ROOT`, `WHISPER_VULKAN_DIR`,
  `../vendor/ffmpeg-lgpl`); hand-rolled notarize/sign (mac-dmg.mjs) and the updater key read in scripts; dev server in QA (§1).
- **MailRight**: config writes `VITE_MAILRIGHT_BETA_BUILD` at load and lets `MAILRIGHT_CORE_TEST_MODE` select `test-mode`;
  no `switches.json`; `src/views/SettingsView.tsx` committed with mixed line endings against `eol=lf` (every checkout is dirty).
- **PostRight**: absolute paths from `resolveTargetRoot`; invalid provider `r2-postrights`; no `legal` block or
  `.gitattributes`; placeholder updater values; two executable switches without `compiledOut`.
- **Pulse**: version from package.json; dev builds keep the QA control; public provider on a private repository; hand-rolled
  NSIS payloads; empty `switches.json`; no legal manifest; `projectType: 'swiftpm'` with `Package.swift` only at `mac/`
  (`release-invocation.mjs:245` requires it at the app root).
- **ScrapeRight**: `notarytoolAuthArgs()` reads `APPLE_API_*` at config load (right-release.config.mjs:232); updater key in
  ViewRight's folder (:261); dev builds use an undefined `release-iterate` profile.
- **ViewRight**: version from package.json; `content-devtools` enabled without the `devtools: "content-only"` declaration
  (the shared policy already lists ViewRight); updater key file; no `switches.json`; bare `tauri build` for Linux.
- **desktop-app-template**: computed config with `LOCALAPPDATA`; hand-rolled `build-mac-notarized.sh`; the retired
  `release-iterate` profile; `@rightkit/release` pinned to 0.2.151; e2e through `rightkit-qa run --build`; no legal manifest,
  `switches.json` or `.gitattributes`. Every new app inherits these.

All nine: release pins behind the registry (`@rightkit/release` 0.6.2 in CutRight, CodeRight, MailRight, PostRight; 0.6.3
elsewhere); no hooks, no CI, no `journeys.toml`.

## 4. The ten gaps that affect the most apps, with the smallest RightKit change

1. Release configs are code (P8): validate the loaded object with the existing `release-config.mjs` checks now; move to data.
2. No app check before build, dev or doctor (P1): `rightkit-gate check` for apps, called first by `right-release`.
3. The doctor and the build stop at the first problem (P3): collect every finding; never exit before the report.
4. e2e under `cargo test` (P7): run the existing lint rules inside `assertQaAdoption`, no manifest needed.
5. QA admission only warns (P5): fill `approvedRunners` and switch apps to refuse (Adrian, ADR-0001 item 10).
6. App signing and packaging code (P8): raise the codesign/notarytool fingerprints to block; add hdiutil, makensis,
   `TAURI_SIGNING_PRIVATE_KEY`.
7. Plain cargo in app scripts (P1): extend the check to refuse bare cargo outside a broker shim.
8. Line endings (P2): run eol-policy for apps; ship a standard `.gitattributes` in the template.
9. Data errors found late (P3, P8): missing `switches.json` and legal manifest fail in plain `doctor`; refuse `updater.keyFile`;
   check `distribution.provider` at load.
10. Dev-mode binaries in QA (P5, P7): `guard` and `journeys check` require `custom-protocol` for a Tauri QA build;
    `control::launch` fails at once on a dev-mode binary; `[build]` gains a frontend step.

## 5. Docs that contradict the code

- `crates/rightkit-qa/README.md:29` tells apps to run e2e through `cargo test` (contradicts P7).
- `packages/release/README.md:417` still shows `updater.keyFile`, which `sign-updater.mjs:23` refuses.
- `release-invocation.mjs:155,180` requires bare `cargo` for iOS prepare steps, which `pipeline-admission.mjs` refuses.
- `AGENTS.md:140-142` points at docs not in `rightkit/docs` (RIGHT-SUITE-RELEASE-PIPELINE.md, RIGHTKIT-SHARED-PACKAGES.md,
  RIGHT-SUITE-UPDATE-TIER-ROUTING.md).
- `packages/release/rightkit-versions.json` `registry` lists `@rightkit/release` 0.6.3 and `rightkit-qa` 0.8.0 as published
  while the changelog and `PIPELINE-ARCHITECTURE.md` call them unpublished (not checked against the registries: no network).
