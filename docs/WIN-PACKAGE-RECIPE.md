# Windows package recipe (`winPackage`)

> **In `right-release.config.mjs` (what `right-release release --platform win` runs).** `targets.win.winPackage`
> is expanded by `packages/release/package-recipe-targets.mjs` into `prePackage` (`tauri build --no-bundle`) and
> `package` (`tauri bundle --bundles nsis`, updater artifacts off), with `signingContract`
> `windows-raw-exe-authenticode-before-nsis-v1`. release.mjs then runs its existing gates unchanged: the bundle
> marker patch before signing, raw-EXE Authenticode, the NSIS embedding verification, installer signing.
> Sidecars go through `externalBin` / `mediaSidecar` (signed with the raw EXE), not `winPackage.sidecars`.
> Migration: delete `prePackage` and `package` (the build-windows-nsis.mjs hook) and add
> `winPackage: { installer: "nsis", productName, updater, perMachine? }`. The standalone `runWinPackageRecipe` below
> is for direct callers.

Tracking: bogusyogi/rightkit#345 (every product builds through one RightKit entrypoint).

A Windows target that declares `winPackage` and no `package` script is packaged by
`packages/release/win-package-recipe.mjs` (`runWinPackageRecipe`). It replaces the app-local
`build-windows-nsis.mjs`, `win-nsis.mjs` and `build-beta-installer.ps1` two-phase hook scripts.

> Everything from here to "Profile, extra Tauri config and prepare steps" describes the **standalone `runWinPackageRecipe`**
> (direct callers). The release target keys (including `profile`, `jobs`, `timeoutMs`, `cleanStale`, `cargoTargetRoot`) are in
> the sections after it. `sidecars` is standalone-only; a release target uses `externalBin` / `mediaSidecar`.

## Standalone runWinPackageRecipe: schema

```js
targets: {
  win: {
    signed: true,
    signingContract: "windows-raw-exe-authenticode-before-nsis-v1",
    sign: { prePackageFiles: ["<raw exe>"], files: ["<installer exe>"] },
    updater: { keyFile: "%APPDATA%\\ViewRight\\updater.key" },
    winPackage: {
      installer: "nsis",          // required, only "nsis"
      productName: "HeardRight",  // required, 1-64 chars, no path or reserved file-name characters
      updater: true,              // required boolean, no default
      sidecars: ["src-tauri/resources/runtime/whisper-cli.exe"], // optional .exe/.dll, relative, unique
      perMachine: false,          // optional, default false (currentUser install mode)
      rawExe: "...",              // optional; else exactly one target.sign.prePackageFiles entry
      installerFile: "...",       // optional; else exactly one target.sign.files entry
      tauriCwd: ".",              // optional, relative; where `pnpm exec tauri` runs
      triple: "x86_64-pc-windows-msvc", // optional Windows triple
    },
  },
}
```

Rules, all enforced before any effect by `validateWinPackage` (throws `WinPackageConfigError`, code
`RK_WIN_PACKAGE_CONFIG`, `.issues` lists every problem):

- unknown keys are refused; `package` and `winPackage` together are refused;
- every path is relative and stays inside the app root; sidecars are unique and are not the raw EXE;
- `config.packageManager` must be `pnpm` (or absent).

A failing phase throws `WinPackageRecipeError` (code `RK_WIN_PACKAGE_PHASE`, `.phase`, `.cause`) and
stops the recipe: nothing after it runs.

## Phases

| # | Phase | Effect |
| --- | --- | --- |
| 1 | `build` | `pnpm exec tauri build --no-bundle --target <triple>`; raw EXE and every sidecar must exist |
| 2 | `marker` | `patchTauriBundleType(rawExe, { bundle: "nsis" })` before signing, so Tauri's own rewrite changes nothing |
| 3 | `sign-raw` | one sign-windows call over `[...sidecars, rawExe]`; a signed digest is required for each file |
| 4 | `bundle` | `pnpm exec tauri bundle --bundles nsis --target <triple> --config <override>`; override forces `createUpdaterArtifacts:false` and the install mode; Tauri key env is empty so it never prompts |
| 5 | `verify` | raw EXE on disk still equals the signed digest, and `verifyNsisEmbeddedBinary` proves the installer embeds those bytes |
| 6 | `sign-installer` | sign-windows over the installer (receipt phase `installer`) |
| 7 | `updater` | only when `updater: true`: sign-updater over the signed installer, `<installer>.sig` must exist |

The updater signature is deliberately last. Authenticode signing changes the installer bytes, so a
signature made earlier (by `tauri bundle`) would no longer match the shipped file.

## Injectable effects

`runWinPackageRecipe({ appRoot, config, target, deps })`. `deps` may replace any of: `env`, `platform`,
`dryRun`, `log`, `spawn(command, args, { cwd, env })`, `signWindows(files, { phase, dryRun })`
(returns `[{ file, sha256 }]`), `signUpdater(file, { keyFile, dryRun })`, `patchMarker`, `verifyEmbedded`,
and `fs` (`existsSync`, `readFileSync`, `rmSync`, `mkdirSync`). The defaults call the real
`sign-windows.mjs`, `sign-updater.mjs`, `tauri-bundle-marker.mjs` and `nsis-payload.mjs`. The returned
receipt records the phases run, the signed digests, the marker result and the updater signature path.

## Migrating an app

1. List what the old script did beyond build / bundle: staged runtimes, legal or notice gates, engine
   builds. Anything that must run before the raw EXE builds stays in `prePackage` (or moves to a
   `before-build` step); the recipe does not run it.
2. Replace `package: { cmd: "node", args: ["scripts/...", "--bundle-only"] }` with
   `winPackage: { installer: "nsis", productName, updater, sidecars }`. Keep `sign.prePackageFiles` and
   `sign.files` (they name the raw EXE and installer) and `updater.keyFile`.
3. Move each separately shipped executable the script signed or staged into `sidecars`.
4. Delete the app script and its `package.json` entry; keep nothing that calls `tauri bundle` itself.
5. Check with a dry run: `right-release build --platform win --dry-run` prints each phase without
   spawning Tauri or signing.
6. Confirm on a real build that the installer verifies (`sign-windows --verify-only`) and, for updater
   apps, that `<installer>.sig` exists and matches the signed installer.

## Not covered by the recipe

- App-specific staging (HeardRight's DirectML and Whisper runtimes), legal-fact gates, notice
  generation, QA-control and release-switch scans (release.mjs still runs these around the package step).
- MSIX, portable and the PowerShell beta installer (`mailright`) which use other signing contracts.
- The standalone `runWinPackageRecipe` has no hung-Tauri-process recovery; it relies on `spawn` returning. The release
  target does: see `timeoutMs` and the artifact stability poll below.

## Profile, extra Tauri config and prepare steps (`right-release release`, bogusyogi/rightkit#345)

`winPackage` also takes (all optional, validated strictly; `postBuild` is refused because release.mjs signs the raw EXE
between the build and the bundle):

| Key | Effect |
| --- | --- |
| `profile` | A Cargo profile name, passed as `tauri build ... -- --profile <p>` and the same trailing form on `tauri bundle`. The raw EXE path moves with the profile, so keep `sign.prePackageFiles` pointing at it. |
| `tauriConfig` | App-relative `.json`, passed as `--config <file>` (relative to the Tauri cwd) before RightKit's own override, for resource mappings and `signCommand: null`. |
| `prepare` | `[{ cmd, args?, cwd? }]` argv commands (no shell, `cwd` inside the app) run before the Tauri build. |
| `jobs`, `timeoutMs`, `cleanStale`, `cargoTargetRoot` | see the sections below. |

`prePackage` is always `node recipe-build.mjs --plan <json>` (`right-release recipe-build`): the built-in stale cleanup, `prepare`,
the Tauri build, then the artifact stability poll, in order, stopping at the first non-zero exit.

### Build keys for hung or slow builds (CodeRight, PostRight, ViewRight reports)

| Key | Default | Effect |
| --- | --- | --- |
| `jobs` | unset | positive integer, passed to Cargo as `--jobs N` after `--` on `tauri build` (with `profile`: `-- --profile p --jobs N`) |
| `timeoutMs` | `1200000` (20 min) | positive integer set on the generated `prePackage` and `package` command objects. release.mjs `runCommand` honours `command.timeoutMs`. On expiry its `run()` calls `killProcessTree`, which is `taskkill /PID <pid> /T /F` on Windows (and a process-group SIGTERM then SIGKILL elsewhere), so the whole tree (pnpm, tauri, cargo, rustc, and a hung NSIS step) dies, then the release exits 1. The timer covers the entire `prePackage` command, including `prepare` and the stability poll |
| `cleanStale` | `true` | recipe-build deletes the stale raw EXE and installer (`sign.prePackageFiles` / `sign.files`) before anything else, so an old file can never be signed as this build's output. `false` keeps them |
| (always on) | | after the build, recipe-build waits until the raw EXE exists and its size and mtime are unchanged over 2 checks 1 s apart (120 s limit, then `RECIPE_BUILD_ARTIFACT_UNSTABLE`) so a hung or still-writing Tauri process cannot hand a half-written EXE to signing |

### Managed Cargo target roots (`cargoTargetRoot`)

The raw EXE and installer are the paths you declare, normally `src-tauri/target/<triple>/release/...`. With
`cargoTargetRoot: true` (manifest `<tauriCwd>/src-tauri/Cargo.toml`) or `cargoTargetRoot: "<app-relative Cargo.toml>"`, the
expansion asks `cargo metadata` for `target_directory` and moves every declared path beginning with `<manifest dir>/target/` there.
`CARGO_TARGET_DIR` set in the environment triggers the same lookup when `<tauriCwd>/src-tauri/Cargo.toml` exists. What works:

- a target directory inside the app (for example `CARGO_TARGET_DIR=.cargo-target`): `sign.prePackageFiles`, `sign.files`, the
  cleanup and the stability poll all use the moved paths;
- a target directory outside the app but under a managed root (`resolveManagedRoots`, the broker-managed target directory on a
  brokered host; rightkit#924, release 0.6.2): declared paths stay at their default app-relative location under
  `src-tauri/target/`. The raw EXE is copied there after the build (stale real EXE deleted first, polled until stable); the signed
  copy goes back into the real target directory before `tauri bundle` (which embeds the EXE it finds there), the real EXE is
  verified equal to the signed one (sha256) before and after the bundle, and the installer is copied out. Every copy is atomic and
  sha256-verified; nothing is written outside the app root or the managed target directory;
- a target directory outside the app and outside every managed root: fails closed with a message. Paths outside the default
  `target/` prefix are never changed;
- `RIGHT_RELEASE_ARTIFACT_DIR` is not a build output location: release.mjs only falls back to a declared file's basename there
  when the declared path is missing.

### Scans that run for you

Before the raw EXE and sidecars are signed, release.mjs runs `assertNoQaControlBinaries` on them (QA-control marker, wdio
webdriver plugin, shell dev-inspect marker, remote-debugging switches, Tauri devtools for allow-listed apps) and, on the same
files, the D4 release-switch binary layer (`assertNoReleaseSwitches`: test-mode marker, registered S1-S4 names, and
`config.releaseSwitches.forbid`). `config.releaseSwitches` also drives the source layer (feature closure, the `switches.json`
inventory) and the frontend `dist` is scanned for browser-QA mocks. That replaces an app's own `check-release-test-switches` for
everything those layers cover; it does not read the packed NSIS payload (the raw payload was scanned before packing) and does not
know app-specific string rules absent from `switches.json` or `releaseSwitches.forbid`.

`development.targets.win.build` is never touched by the recipes (the development lane does not call the expansion), so a dev build
keeps its own features such as `--features dev-control`.

### Remaining ScrapeRight asks map to existing release.mjs keys (verified in release.mjs)

- Release-switch scan between build and sign: automatic. The QA-control scan plus the D4 release-switch binary layer run on the
  raw EXE and sidecars before they are signed; add extra paths with `target.qaControlScan`. Source and feature-closure scans
  read `config.releaseSwitches` (`inventory`, `manifest`, `rust`, `features`, `forbid`) and the plan inside prePackage.
- Post-sign smoke: `targets.win.postSign` (`{ cmd, args }`), run after the installer is signed.
- Installer signing and embedding verification: `sign.prePackageFiles` / `sign.files`, unchanged.
