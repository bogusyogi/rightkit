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

## Schema

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
- Hung-Tauri-process recovery (artifact stability polling and process-tree kill) that heardright and
  viewright carry; the recipe relies on `spawn` returning.
