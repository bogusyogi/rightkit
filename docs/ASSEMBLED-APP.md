# Assembled app: one declarative definition for dev install and production packaging

One assembly definition per platform, shared by `right-release dev` (install in place) and `right-release build`
(production packaging). Code: `packages/release/assembled-app.mjs` (definition, layout, proof),
`install-assembled-app.mjs` (dev install), `assemble-app-command.mjs` (`right-release assemble-app`).

## Why these names (and where they differ from the first proposal)

* The assembly is declared ONCE under a new top-level block `assembly.<mac|win>`. The dev install descriptor and the
  production hooks only reference it. No key is repeated between dev and release.
* Dev install kind is `assembled-app` on BOTH platforms (not an extended `portable`). Reason: `portable` hard-requires
  `activation.kind: "current"` and installs at `<installRoot>/current` (portable-development.mjs); Pulse's Windows install is the
  exact folder `%LOCALAPPDATA%\Programs\Pulse` that the NSIS uninstall entry owns. One new kind, same name on both OSes, beats
  bending `portable`'s contract.
* `plugins` is a list of NAMES of embedded plug-ins that the xcodebuild product must already contain (PulseFinder.appex is built
  by the xcodegen target, inside the outer app); they are required and version-stamped, never copied from anywhere.
* The Windows `runtime`/`museHooks` keys became ordinary `payload.files` rows (membrane runtime DLLs are `remote` rows of
  `sidecars`; the four Muse hooks are four `{stage:"run-pulse",to:...}` rows) plus one generic `payload.rewrites` text rewrite.
  The module never downloads anything (no network): every external file must be a staged sidecar row (cargo-bin or remote).
* Paths in config use `/` on both platforms (converted on Windows). `to` is always relative to the bundle root (mac) or the
  payload root (win).
* Existing names are kept: `destination`, `bundleId`, `identifiers`, `entitlements`, `assert`, `assertTeamSignedNested`,
  `timestamp`, `verifyBinaries`, `targetTriple`, `kind`.

## 1. Shared definition: `assembly.mac`

```js
assembly: { mac: {
  productName: "Pulse",                  // required. <productName>.app
  bundleId: "dev.orthic.pulse",          // required. Outer CFBundleIdentifier; the assembled app must carry it
  targetTriple: "aarch64-apple-darwin",  // required. stage receipt target + tauri bundle folder
  stageReceipt: "dist/stage/stage-sidecar.json",   // default. rows are re-hashed against sha256; cargo-bin rows must carry the build profile (dev|release)
  outer: {                               // required
    tool: "xcodebuild",                  // only value
    projectSpec: "mac/Notch/project.yml",// optional: `xcodegen generate --spec <projectSpec> --project <dirname(project)>` runs first
    project: "mac/Notch/Pulse.xcodeproj",
    scheme: "Pulse",
    configuration: "Release",            // default "Release"
    product: "Pulse.app",                // default `${productName}.app` (found under <DerivedData>/Build/Products/<configuration>)
    unsetEnv: ["PULSE_QA"],              // optional: variables removed from xcodebuild's environment
    extraBuildSettings: {},              // optional NAME: value; CODE_SIGNING_ALLOWED=NO is always passed
  },
  nested: [                              // optional. Built apps nested into the outer bundle
    { built: "tauri", manifestPath: "hub/src-tauri/Cargo.toml", productName: "Pulse", to: "Contents/Helpers/Pulse.app" },
    // also accepted: bundleDirectory (relative) instead of manifestPath. Dev reads <target>/<triple>/debug/bundle/macos, release .../release/...
  ],
  helpers: [                             // optional. Loose executables, chmod 755. `to` defaults to Contents/Helpers/<name>
    { product: "PulseHelper" },          // from the xcodebuild Products folder (beside the outer app)
    { product: "pulse-elevate" },
    { stage: "pulse" }, { stage: "pulse-engine" }, { stage: "pulse-gateway" }, { stage: "membrane" },
    { stage: "smartctl", kind: "remote" },
    { stage: "ffmpeg", kind: "remote", to: "Contents/Helpers/Pulse.app/Contents/MacOS/ffmpeg", optional: true },
  ],
  plugins: ["PulseFinder.appex"],        // optional. Must exist in Contents/PlugIns of the xcodebuild product; version-stamped
  launchDaemons: [{ from: "mac/Notch/Helper/dev.orthic.pulse.helper.plist" }],  // -> Contents/Library/LaunchDaemons/<basename> (or `to`)
  launchAgents:  [{ from: "mac/Notch/Agent/dev.orthic.pulse.engine.plist" }],   // -> Contents/Library/LaunchAgents/<basename>
  resources: [                           // optional. file or directory copy; `ifStaged: "<name>"` = only when the receipt holds that row
    { from: "third_party/smartmontools", to: "Contents/Resources/ThirdParty/smartmontools" },
    { from: "plugins/muse/pulse", to: "Contents/Resources/plugins/muse/pulse" },
    { from: "mac/Notch/LICENSE", to: "Contents/Resources/codeNOTCH-LICENSE.txt" },
    { from: "third_party/ffmpeg", to: "Contents/Resources/ThirdParty/ffmpeg", ifStaged: "ffmpeg" },
  ],
  bundleVersion: "commit-count",         // optional. CFBundleVersion = `git rev-list --count <RIGHT_GIT_SOURCE_REVISION|HEAD>` on the outer app,
                                         //   every nested app and every plugin (PlistBuddy Set, else Add). A shallow clone is refused (no network, no unshallow)
  require: [ /* bundle-relative files that must exist after assembly (the reference 17-path list) */
    "Contents/MacOS/Pulse", "Contents/Helpers/pulse", "Contents/Helpers/pulse-engine", "Contents/Helpers/pulse-gateway",
    "Contents/Library/LaunchAgents/dev.orthic.pulse.engine.plist", "Contents/Helpers/Pulse.app/Contents/MacOS/pulse-hub",
    "Contents/Helpers/Pulse.app/Contents/Info.plist", "Contents/Helpers/PulseHelper", "Contents/Helpers/pulse-elevate",
    "Contents/Helpers/smartctl", "Contents/Helpers/membrane", "Contents/Resources/ThirdParty/smartmontools/GPL-2.0.txt",
    "Contents/Resources/plugins/muse/pulse/.claude-plugin/plugin.json", "Contents/Resources/plugins/muse/pulse/hooks/hooks.json",
    "Contents/PlugIns/PulseFinder.appex/Contents/MacOS/PulseFinder", "Contents/PlugIns/PulseFinder.appex/Contents/Info.plist",
    "Contents/Library/LaunchDaemons/dev.orthic.pulse.helper.plist" ],
  devMarker: "Contents/Resources/dev-only.json",   // optional. A dev assembly writes { schema:1, profile:"dev", devOnly:true, files:{dest: receipt sha256} } here.
                                         //   A release assembly refuses to run when this file exists in the output and never writes it.
  // Production only (used by `assemble-app --profile release`):
  stagingOutput: "dist/staging/Pulse.app",                 // default dist/staging/<productName>.app
  bindingFile: "dist/staging/payload-binding.json",        // default <dirname(stagingOutput)>/payload-binding.json (release binding of receipt hashes)
  prepare: { release: [                  // optional. Commands run first by `assemble-app --profile release --build` (broker-routed through resolveCommand)
    { cmd: "pnpm", args: ["--dir","hub","exec","tauri","build","--bundles","app","--no-sign","--target","aarch64-apple-darwin"] },
    { cmd: "pnpm", args: ["exec","right-release","stage-sidecar","--target","aarch64-apple-darwin"], env: { RIGHTKIT_BUILD_MODE: "release" } },
  ] },
} }
```

## 2. Dev install (mac): `development.targets.mac.install`

`build` stays as today (hub web build, `stage-sidecar`, `tauri build --debug --no-sign`). After it, `kind: "assembled-app"` runs:
xcodegen + xcodebuild (DerivedData from `rustright storage xcode-args`, the broker storage owner; fallback `.right-release/assembled-app/DerivedData`) ->
assemble in a private temp dir -> sign inside-out with the Developer ID (`signMacosDevelopmentBundle`) -> verify the STAGED bundle
(`/usr/bin/codesign --verify --strict --deep`, `require`, launchd `BundleProgram`, `assert*`) -> assert the designated requirement equals
the installed copy's -> `installApp({dev:true})` at `destination` (in place, NO backup) -> verify the installed copy again (strict deep,
requirement, assertions, `verifyBinaries` digests staged-vs-installed). A failed staged verification never touches `destination`.

```js
development: { targets: { mac: {
  build: [ /* unchanged */ ],
  install: {
    kind: "assembled-app",
    destination: "/Applications/Pulse.app",        // required, absolute; must be /Applications/<assembly.mac.productName>.app (same rule as tauri-devid)
    identifiers: {                                 // existing signing keys, unchanged semantics (install.* wins over sign.*)
      ".": "dev.orthic.pulse",
      "Contents/Helpers/PulseHelper": "dev.orthic.pulse.helper",
      "Contents/Helpers/pulse-elevate": "dev.orthic.pulse.elevate",
      "Contents/Helpers/pulse-gateway": "dev.orthic.pulse.gateway",
      "Contents/PlugIns/PulseFinder.appex": "dev.orthic.pulse.finder",
    },
    entitlements: {                                // path-or-glob -> plist (same matrix as targets.mac.sign.nestedCode)
      ".": "release/entitlements.plist",
      "Contents/Helpers/pulse": "release/entitlements.plist",
      "Contents/Helpers/pulse-engine": "release/entitlements.plist",
      "Contents/Helpers/pulse-gateway": "release/entitlements.plist",
      "Contents/Helpers/Pulse.app": "release/entitlements.plist",
      "Contents/PlugIns/PulseFinder.appex": "mac/Notch/FinderExtension/PulseFinder.entitlements",
    },
    assertTeamSignedNested: true,
    assert: [{ path: "Contents/Helpers/PulseHelper", identifier: "dev.orthic.pulse.helper", teamIdentifier: "6KLGD3LLKF" }],
    verifyBinaries: ["Contents/MacOS/Pulse", "Contents/Helpers/pulse", "Contents/Helpers/PulseHelper"],  // staged-vs-installed digests
    // timestamp: false (default)
  },
} } }
```

## 3. Production (mac): which keys, for `right-release build`

The assembly is the `prePackage` step; the existing sign matrix, notarization and DMG are unchanged and read its output.

```js
targets: { mac: {
  prePackage: { cmd: "right-release", args: ["assemble-app", "--platform", "mac", "--profile", "release", "--build"] },
  //   writes the unsigned bundle at assembly.mac.stagingOutput (= dist/staging/Pulse.app) and assembly.mac.bindingFile
  sign: { prePackageFiles: [{ file: "dist/staging/Pulse.app", entitlements, hardenedRuntime: true }], nestedCode: { /* as today */ } },
  postSign: { cmd: "right-release", args: ["assemble-app", "--platform", "mac", "--verify-staged", "--signed"] },
  //   structural proof of the signed staged app: require list, launchd BundleProgram, bundle id, no devMarker, release binding present
  notarize: { app: "dist/staging/Pulse.app", appBeforePackage: true, /* ... */ }, dmg: { spec: "release/appdmg.json" },
} }
```

## 4. Shared definition: `assembly.win`

```js
assembly: { win: {
  productName: "Pulse",
  targetTriple: "x86_64-pc-windows-msvc",
  stageReceipt: "dist/stage/stage-sidecar.json",
  payload: {
    files: [                             // required. One of stage | from per row; `to` relative to the payload root; `ifStaged` as on mac
      { stage: "pulse-windows", to: "Pulse.exe" },
      { stage: "pulse-hub", to: "pulse-hub.exe" },
      { stage: "pulse", to: "Helpers/pulse.exe" },
      { stage: "pulse-engine", to: "Helpers/pulse-engine.exe" },
      { stage: "pulse-gateway", to: "Helpers/pulse-gateway.exe" },
      { stage: "run-pulse", to: "Helpers/run-pulse.exe" },
      { stage: "membrane", to: "Helpers/membrane.exe" },
      { stage: "smartctl", kind: "remote", to: "Helpers/smartctl.exe" },            // needs a win `remote` sidecar row (see gaps)
      { stage: "onnxruntime.dll", kind: "remote", to: "Helpers/membrane-runtime/onnxruntime.dll" },
      { stage: "DirectML.dll", kind: "remote", to: "Helpers/membrane-runtime/DirectML.dll" },
      { stage: "ffmpeg", kind: "remote", to: "ffmpeg.exe", ifStaged: "ffmpeg" },
      { from: "plugins/muse/pulse", to: "plugins/muse/pulse" },
      { stage: "run-pulse", to: "plugins/muse/pulse/hooks/muse-session-start.exe" },
      { stage: "run-pulse", to: "plugins/muse/pulse/hooks/muse-prompt.exe" },
      { stage: "run-pulse", to: "plugins/muse/pulse/hooks/muse-post-tool.exe" },
      { stage: "run-pulse", to: "plugins/muse/pulse/hooks/muse-stop.exe" },
      { from: "third_party", to: "ThirdParty" }, { from: "NOTICE", to: "NOTICE.txt" }, { from: "LICENSE", to: "LICENSE.txt" },
    ],
    rewrites: [{ file: "plugins/muse/pulse/hooks/hooks.json", regex: "\\$\\{CLAUDE_PLUGIN_ROOT\\}/hooks/(muse-[a-z-]+\\.exe)",
                 flags: "g", replacement: "%CLAUDE_PLUGIN_ROOT%\\\\hooks\\\\$1", requireMatch: true }],   // applied after the copies; requireMatch fails when nothing matched
    require: ["Pulse.exe", "pulse-hub.exe", "Helpers/pulse.exe", "ThirdParty/smartmontools/GPL-2.0.txt", "NOTICE.txt", "LICENSE.txt"],
  },
  sign: ["Pulse.exe", "pulse-hub.exe", "Helpers/pulse.exe", "Helpers/run-pulse.exe", "Helpers/smartctl.exe", "Helpers/membrane.exe", "Helpers/pulse-engine.exe", "Helpers/pulse-gateway.exe"],
                                         // payload-relative; signed with sign-windows (Azure Artifact Signing) then `--verify-only`. Used by the dev install; production keeps targets.win.sign.prePackageFiles
  devMarker: "dev-only.json",            // as on mac, payload-relative (dev only; refused in a release payload)
  // Production only:
  stagingOutput: "dist/staging/windows/Pulse", bindingFile: "dist/staging/windows/payload-binding.json",
  prepare: { release: [ { cmd: "pnpm", args: ["--dir","hub","run","build"] }, { cmd: "pnpm", args: ["exec","right-release","stage-sidecar","--target","x86_64-pc-windows-msvc"], env: { RIGHTKIT_BUILD_MODE: "release" } } ] },
  nsis: { script: "scripts/release/windows/pulse.nsi", output: "dist/releases/windows/Pulse-Setup-x64.exe",
          defines: { STAGE: "{payload}", OUT: "{output}", VERSION: "{version}" } },   // makensis found like the reference (PATH, Program Files, tauri/NSIS); {payload} {output} {version} substituted
} }
```

## 5. Dev install (win): `development.targets.win.install`

```js
development: { targets: { win: {
  build: [ /* unchanged: web-build, stage-sidecar, tauri build --debug (hub) */ ],
  install: {
    kind: "assembled-app",
    installDirectory: "%LOCALAPPDATA%/Programs/Pulse",  // required. Absolute after expanding %LOCALAPPDATA% / %PROGRAMFILES%; never a drive root
    stop: { scheduledTask: "Pulse background agent", process: "Pulse.exe", timeoutMs: 15000 },
                                                        // ends the task, stops every process whose image is inside installDirectory (re-stopped while waiting), waits until none runs
    restart: { scheduledTask: true, launch: "Pulse.exe" },   // `schtasks /run` the stop.scheduledTask again; launch only when stop.process was running before
    carryOver: ["Uninstall.exe"],                       // installer-owned files carried from the old folder when the payload lacks them
  },
} } }
```

Install-root safety (G7): in an agent shell on the Dell, LOCALAPPDATA and USERPROFILE point into a Claude profile, so a root derived from
`%LOCALAPPDATA%` could swap a profile copy and stop the wrong scheduled task while the real install stays. Fail-closed rules (Windows-shaped
paths, checked before anything is touched):
- A `%LOCALAPPDATA%`-derived root is refused unless LOCALAPPDATA is exactly `C:\Users\<name>\AppData\Local` with no `ClaudeProfiles` segment; the
  message names LOCALAPPDATA and the derived path. Give an absolute `installDirectory` (e.g. `C:\Users\<name>\AppData\Local\Programs\Pulse`) to
  bypass derivation; an absolute path inside a `ClaudeProfiles` profile is refused too.
- A derived root is also refused when another `C:\Users\*\AppData\Local\Programs\<Product>` exists and either the running `stop.process` image
  lives there and not in the derived root, or the derived root has no install.
- Not implemented: resolving the real root from the uninstall registry key. HeardRight's portable path does not use this resolver (no other callers).

Order (Windows): assemble the payload in `<installDirectory>.incoming` -> sign the `sign` files -> prove the incoming copy (every payload file
present and identical to its source, `sign-windows --verify-only`) -> take the install lock -> stop task/processes -> rename
`installDirectory` -> `.replaced`, `.incoming` -> `installDirectory` (retried on EBUSY; the old folder is put back on failure) -> carryOver ->
restart -> delete `.replaced` (no backup is kept).

Note: the reference restarts the agent with `schtasks /run /tn <task>` (windows-dev-install.mjs restartAgent); `/IT` only exists on
`schtasks /create`. The module follows the reference.

## 6. Production (win)

```js
targets: { win: {
  prePackage: { cmd: "right-release", args: ["assemble-app", "--platform", "win", "--profile", "release", "--build"] },   // payload -> assembly.win.stagingOutput
  sign: { prePackageFiles: [ "dist/staging/windows/Pulse/Pulse.exe", /* ... as today */ ] },
  package: { cmd: "right-release", args: ["assemble-app", "--platform", "win", "--package"], timeoutMs: 1200000 },        // makensis from assembly.win.nsis, then sign-windows on the installer
  postSign: { cmd: "right-release", args: ["assemble-app", "--platform", "win", "--verify-staged"] },
} }
```

## 7. CLI

`right-release assemble-app --platform mac|win --profile dev|release [--build] [--out <dir>] [--config <file>]`
`right-release assemble-app --platform mac|win --verify-staged [--signed]`
`right-release assemble-app --platform win --package`

The dev install does not call the CLI: `development.mjs` dispatches `kind: "assembled-app"` to the same module.

## 8. What the Pulse config can drop

`development.targets.*.install` today is absent; with these keys `pnpm dev:mac`/`dev:win` become the gate's `--to dev` stage (docs/GATE-CONTRACT.md §1).
The production `prePackage`, `package`, `postSign` hooks stop pointing at the removed `scripts/release/*` files.
Pulse-specific checks that `candidate.mjs` ran (bundled `pulse scan` smoke, Apple Events entitlement scan, camera/mic entitlement refusal)
are not part of the shared module; the existing `identity-scan`/`assert` keys cover identity, the rest is Pulse's own check command if wanted.
