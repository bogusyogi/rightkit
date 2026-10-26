# @rightkit/release

Portable release CLI and SDK for Right Suite products: builds, signs, and publishes to public GitHub Releases or to private R2 with RightApps distribution. It also runs the ownership catalog and lockfile checks and the release gates.

| Item | Value |
|---|---|
| Where it runs | Build and release tooling. Native signing runs on the build host. |
| Rust pair | None. The Rust build broker files in this package (`managed-cargo.mjs`, `managed-roots.mjs`) are moving to the separate RustRight product. Do not extend them here. |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

Signing and publication rules are in the workspace file `docs/rules/release-signing.md`. Public releases use RightKit CI. Private builds run on the native host.

## No test-automation surface ships

Release and update builds (`right-release build`, `publish-update`) must not contain any test-automation or debugging surface; the development lane may. Layers, in the order they run:

1. **Source (before anything is built; `right-release build --dry-run` only loads the config and does not reach this layer)** - `qa-build-features.mjs` refuses a build command, a package.json script it runs, or an app `Cargo.toml` that enables `devtools`, `qa-native`, `qa-control`, `dev-native` or `dev-inspect` (also `--all-features`, and `devtools` on the `tauri` dependency, `qa-control` on a non-optional `rightkit-qa-driver` (or `control` on a non-optional `rightkit-qa`; `rightkit-qa/control` is also refused as a feature item), `dev-inspect` on a non-optional `rightkit-tauri-shell`, a non-optional `tauri-plugin-wdio-webdriver`, or any of them reachable from the default features).
2. **Pre-pack payload scan** - `qa-control-scan.mjs` over the raw EXE (Windows NSIS and portable), the macOS signing set and `notarize.app` (the `.app` going into the DMG; `dmg build` also scans its staging directory), the MSIX `stageDir` before packing, and `target.qaControlScan`. Mach-O, PE and ELF files are matched against five rules: `qa-control-server` (marker `RIGHTKIT-QA-CONTROL-SERVER-PRESENT`), `shell-dev-inspect` (marker `RIGHTKIT-SHELL-DEV-INSPECT-COMPILED`, written by rightkit-tauri-shell's `dev-inspect` feature; no allow-list lifts it), `wdio-webdriver` (2 of 3 of `tauri-plugin-wdio-webdriver`, `tauri_plugin_wdio_webdriver`, `wdio-webdriver:default`), `remote-debugging` (`--remote-debugging-port`, `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`) and `tauri-devtools` (`plugin:webview|internal_toggle_devtools`, `toggleDevtoolsHotkey`); each signature also as UTF-16LE. A scan that finds no native binary fails.

   Web-bundle mock scan: `release` also runs `assertNoWebBundleMocks` over the frontend dist (`target.webBundleScan`, else `build.frontendDist` of `src-tauri/tauri.conf.json`) before packaging when it already exists (also under `--dry-run`) and after the package command, where a missing or empty dist fails. `right-release bundle-scan` (Node/Python runtime roots) and `identity-scan` (needs the app's declared cdhash/team/identifier) are not automatic: both need app-specific inputs, so each app runs them from its own `checks`.
3. **Packed MSIX** - the finished `.msix` is read through its ZIP central directory (stored and deflate entries, nested ZIPs to depth 3, 1 GiB per entry, 8 GiB inflated per container; encrypted, unsupported-method or oversized entries are findings).
4. **Hardening targets** (final artifacts) and **every sealed file** (`verifySealedRelease`, used by every seal and upload path) run the same rules, including ZIP containers.

Not coverable after packing: an NSIS exe (LZMA payload) and a compressed DMG. They are covered only by layer 2, so the release fails when no pre-pack scan saw a native binary, and the log names each unreadable installer. Not coverable at all: assets embedded compressed in the app, resources an NSIS installer packs that were never in the scanned set (list them in `target.qaControlScan`), and any surface without a rule in `QA_RULES`. The `wdio-webdriver` rule is weaker than the marker rule (signatures chosen without the plugin source). Full list: `crates/rightkit-qa-driver/README.md`, "QA control never ships".

### DevTools policy (owner's fixed design)

Every Right app has absolutely NO access to DevTools; the single exception is ViewRight, which may inspect the HTML content it opens, never its own UI or shell. The rules above therefore refuse `devtools` (and rightkit-tauri-shell's `content-devtools`) for every app, with one narrow allowance (`devtools-policy.mjs`): an app whose right-release config declares `devtools: "content-only"`, **and** that is listed in `devtools-policy.json` (`contentOnlyApps`, today only `viewright`), **and** whose Cargo manifest depends on `rightkit-tauri-shell` with `content-devtools`, **and** whose `build.rs` declares an application ACL manifest. Then the source layer accepts `devtools` / `content-devtools`, and the binary layer accepts the `tauri-devtools` signatures only in a binary that also carries `RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED` (written by rightkit-tauri-shell when `content-devtools` is compiled). `--all-features`, `qa-native`, `qa-control`, `dev-native`, `dev-inspect` and every other rule stay refused. The allowance is recorded in the sealed manifest (`devtools`) and re-checked against the allow-list when a sealed file is verified. Any other app declaring it is refused (`devtools-policy.test.mjs`).

## Shell contract rules (`shell-contract.mjs`, preflight SC-R1..SC-R9)

How an app wires `rightkit-tauri-shell` is checked from committed files alone, before any build. `shell-contract.mjs` exports `SHELL_CONTRACT_RULES` (`{ id, gap, run({ root, files }) }`, findings `{ file, subject, problem, fix }` like the G13-G21 rules); `scripts/preflight.mjs` runs them on any repository with a `tauri.conf.json` and ratchets their findings in `ci/app-check-baseline.json` (the nine rules are the SC-R rows of the table in "The app check") (identity `rule|file|subject`, no line numbers). Run one with `node scripts/preflight.mjs --repo <app dir> --only shell-csp`. Each app is a tracked `tauri.conf.json`; a `tauri.<platform>.conf.json` is merged into it by JSON Merge Patch (arrays replace) and judged as its own target only when its patch restates the checked key.

The module imports node builtins only (preflight runs with no `node_modules`), reads Rust and TSX with a comment- and string-aware scanner instead of a parser (an expression it cannot resolve is skipped, never guessed), and reports a rule that fails on one app as a `rule-error` finding instead of throwing. Its content-devtools Cargo test is a copy of `selectedContentDevtoolsCrate` in `devtools-policy.mjs`; `shell-contract.test.mjs` holds the two together.

## Release switch scan (M21/M23, owner decision D4, ratified)

`release-switch-scan.mjs` proves that release code cannot use an environment variable, argument, settings key or build flag to select a test mode, data root, executable, library, model or identity. The runtime half is `rightkit_daemon::mode`: a build without the `test-mode` cargo feature reads no override selector at all. Registry: `release-switch-registry.json` (forbidden features, the `RIGHTKIT-TEST-MODE-COMPILED` marker, deny names with their class). Apps add their own deny names under `releaseSwitches.forbid`. Selector classes: S1 test activation, S2 authority or identity override, S3 executable/library/model, S4 internal state or data root (S1-S4: forbidden, no allow-list), S5 user-content location (allowed, user gesture), S6 diagnostics/tuning (allowed, bounded value).

1. **Feature closure (authoritative)** - `release.mjs` runs `cargo tree -e features,no-dev --locked --target <triple>` in the app repository (`resolveFeatureClosure`; forwards the build command's `--features`/`--no-default-features`; manifest resolves `targets.<os>.cargoManifest` -> `releaseSwitches.manifest` -> `src-tauri/Cargo.toml` -> `Cargo.toml` and must exist; features are also read from package.json scripts the hooks run (up to three levels) plus `releaseSwitches.features`; no manifest fails with `release-switch-manifest-missing` unless the app sets `releaseSwitches.rust: false` (the only skip); a cargo failure fails a real release, a dry run reports it) and passes it to `featureClosureFindings(...)` and `commandSwitchFindings(argv)` refuse `test-mode`, `qa-native`, `dev-native`, `qa-control`, `dev-inspect` anywhere in the resolved graph (finding id `forbidden-feature <name>`). The binary marker below is secondary evidence only. `qa-build-features.mjs` also lists `test-mode` as a forbidden feature, including on a non-optional `rightkit-daemon` dependency. Two daemon host features are declarations rather than bans: `rightkit-daemon` `remote` is refused (`remote-feature-undeclared`) unless the app sets `releaseSwitches.remoteHost: true`, and the loopback feature `remote-loopback` (CodeRight's loopback HTTP + bearer host) is refused (`remote-loopback-undeclared`) unless the app sets `releaseSwitches.remoteLoopback: true`. Neither declaration covers the other; a `remote` reached only through `remote-loopback` in the `cargo tree` indentation is judged by the loopback rule, any other path to `remote` (a direct request, `testing`) still needs `remoteHost`, and a tree without indentation counts every `remote` as direct. Both values must be booleans.
2. **Inventory completeness** - `sourceSwitchFindings` walks app source (Rust, TS/JS, Swift, C; not tests, fixtures, `node_modules`, `target`) and rejects an environment read whose name is not in `switches.json` (`unregistered-read`), a non-literal read in a file not listed under `dynamicReads` (`dynamic-read`), an S1-S4 entry without `compiledOut` (`not-compiled-out`), and a malformed inventory (`inventory-invalid`). Argument flags are enforced for deny-listed flags only.
3. **Binary** - `scanSwitchPaths` / `bufferSwitchFindings` over Mach-O, PE and ELF files: the marker (`test-mode-capability`) and any deny name from S1-S4 (`release-switch-names`), each as Latin-1 and UTF-16LE. The receipt holds the sha256 and size of every scanned file; the caller adds source commit, target triple and resolved features.

`assertNoReleaseSwitches({ treeText, commands, sourceRoot, inventoryText, binaryPaths, appForbid })` runs every layer it is given input for. Limits: packed NSIS/DMG and compressed MSIX members are covered only by the pre-pack scan of the staged payload, symlinks are skipped, and browser JS is checked at the source layer only. `release.mjs` runs the source layer once before packaging (inventory at `releaseSwitches.inventory` or `switches.json`; a missing inventory fails a non-dry-run release) and the binary layer wherever it runs the QA-control binary scan; a finding stops the release. Trust model: a release build reads no override switch at all; the test capability is compiled out (`test-mode` cargo feature, refused by `qa-build-features.mjs`), never hidden behind a flag.

### `switches.json` (one per app)

```json
{
  "schema": 1,
  "app": "viewright",
  "switches": [
    { "name": "HEARDRIGHT_ENGINE_TEST_MODE", "class": "S1", "channel": "env",
      "anchor": "src/test_mode.rs:97", "compiledOut": "feature:test-mode", "note": "" },
    { "name": "LIGHTCRAFT_LOG", "class": "S6", "channel": "env", "valueType": "enum" },
    { "name": "--library", "class": "S5", "channel": "argv", "userGesture": true }
  ],
  "dynamicReads": [ { "file": "src/x.rs", "reason": "why a literal name is impossible" } ]
}
```

- `name` (required, unique): environment variable, argument flag, settings key or `VITE_*` name.
- `class` (required): `S1`..`S6`. `channel` (required): `env`, `argv`, `settings`, `preference` or `frontend`.
- S1-S4 need `compiledOut`: `feature:<name>`, `cfg:test`, `cfg:debug_assertions` or, for Swift, `swift:DEBUG` (the read sits inside `#if DEBUG`; the read exists only there). The source walk skips test directories, including Swift bundles named `Tests`, `UITests`, `<App>Tests` and `<App>UITests`. S5 needs `userGesture: true`. S6 needs `valueType` of `bool`, `int` or `enum`.
- `anchor` and `note` are informational. Fixtures: `fixtures/release-switches/` (`app/` is a clean example).

#### Non-shipped tooling: `releaseSwitches.nonShipped`

On desktop (non-iOS) targets the source walk covers the whole app root, so app tooling that never ships (a bakeoff harness) can produce `unregistered-read` findings that cannot truthfully be declared in `switches.json`. Declare such directories in the release config:

```json
"releaseSwitches": { "nonShipped": [ { "dir": "bakeoff", "reason": "bakeoff harness; not compiled or bundled" } ] }
```

- It is an exclusion list: any directory not listed is still scanned, so an omission fails safe. `dir` is relative to the app root; `reason` is required and non-empty. iOS is unchanged (it already walks only the Xcode project and the local crates in its cargo tree).
- Every entry is validated before the walk and a bad one fails the release with `inventory-invalid`. Refused: a dir that is missing, not a real directory (a symlink), absolute, escaping the root, or the root itself; an empty `reason`; a dir that contains or sits inside a local crate of the resolved release cargo tree (a crate at the app root protects `src/`), `src-tauri`, or the Tauri `frontendDist` bundle; a dir that contains a `package.json` or sits inside a directory below the root that has one; when the root has a `package.json`, `src/`, `public/` and every existing directory named by the root `index.html` (`src`/`href`), a quoted string in `vite.config.*` (root, publicDir, build input) or `include`/`files` of `tsconfig.json`, plus anything inside or containing them; and any dir holding a path named by `src-tauri/tauri.conf.json` `beforeBuildCommand`/`beforeDevCommand`/`beforeBundleCommand` a root `package.json` `build*` script, or the release recipe's own commands (`targets.<os>.prePackage`, `package`, `postSign`, `packageHook`, the finalizer; followed through `pnpm|npm|yarn run <script>`), because those run during the release build.
- Not protected: the release code does not otherwise know the frontend source layout. A frontend input that none of the rules above names (for example one referenced only from a nested config, or built by a script not reachable from those commands) is not refused, so list only directories you know never ship.

## Commands

`right-release` maps these subcommands to the modules in this package:

| Group | Subcommands |
|---|---|
| Build | `release`, `build`, `package`, `build --web` / `web-build` (web frontend only) |
| Build (SwiftPM apps) | `swift-package` (`windows-stage`, `msix`, `mac-app`) (config schema 1: `windows.{exe,out,searchDirs,include,dependencies,publisher,assets}`, `mac.{executable,bundleId,out,frameworks,resources,entitlements}`) |
| Install | `install-app` |
| Publish | `publish`, `upload`, `publish-update`, `github`, `mirror-root-artifact` |
| Signing | `sign-windows`, `sign-updater`, `signing-agent`, `create-mac-updater`, `sign-dev-artifact`, `sign-candidate`, `sign-qa-bundle` |
| Engine locator | `engine-locate --engine cutright\|genright [--sha256 <hex>] [--json]`: the verified route to the installed CutRight or GenRight engine, also importable as `locateEngine` from `engine-locator.mjs`. The path is fixed (`<LocalAppData or ~/Library/Application Support>/Orthic Labs/<App>/current/bin/<engine>`), must be a regular file (no symlink or junction), and must pass signature and identity checks (macOS: strict codesign, Developer ID Team ID, fixed Identifier, hardened runtime, not ad hoc; Windows: valid Authenticode with the Orthic Labs signer). An optional sha256 pins the exact build. There is no environment variable, path option or PATH search, so release builds never trust a caller-supplied engine path. Exit 1 with a typed `ENGINE_*` code on any failure |
| Candidate signing | `sign-candidate --platform win\|mac --artifact <dir> --candidate-receipt <receipt.json> --out <binding.json> [--manifest <sign.json>] [--publisher <subject>] [--dry-run]`: signs the exact unsigned candidate bytes in place instead of rebuilding from source. The candidate receipt (`{ schema: 1, files: [{ path, sha256 }] }`) is checked first and any mismatch refuses; Windows signs through Azure Artifact Signing (`--publisher` pins the MSIX signer subject), macOS through Developer ID with fixed identifiers from `--manifest`. Writes a binding receipt (`rebuilt: false`, candidate sha -> signed sha per file). Never builds, notarizes or publishes |
| Dev artifact signing | `sign-dev-artifact --artifact <dir> --manifest <sign.json> [--metadata <payload.json>] [--evidence <path>] [--dry-run]`: Developer ID signs an existing CI dev artifact per file with fixed identifiers (macOS). It verifies the recorded hashes in the payload metadata (default `payload.json`, `{ schema: 1, files: [{ path, sha256 }] }`) first, signs each sign-manifest entry (`file` relative to the artifact, `identifier` required, nested code before its bundle) through the dev-signing identity (missing Developer ID: `DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED`, no ad hoc fallback), verifies with `codesign --verify --strict`, reads back Identifier and TeamIdentifier, refreshes the metadata hashes, and writes evidence JSON. Never builds, notarizes or publishes. The payload metadata must sit outside every signed entry (`SIGN_DEV_ARTIFACT_METADATA_IN_SIGNED_ENTRY`). An optional declarative `refresh` in the sign manifest recomputes digests that other JSON files record about signed bytes: `{ "refreshRoot": "<dir inside the artifact>", "refresh": [{ "file", "pointer" (JSON pointer to an existing string), "compute": "sha256-file" \| "sha256-tree-v1" \| "verify-sha256-file", "of", "format"?, "currentMatches"? }] }` (`file` and `of` relative to `refreshRoot`; `sha256-tree-v1` = sha256 over the sorted relative paths, each as path, NUL, 8-byte big-endian length, bytes; `verify-sha256-file` writes nothing and refuses a digest that changed; `format` embeds the digest in a locator string through one `{value}`; `currentMatches` refuses to overwrite a value of another kind, such as a signed attestation locator). Order is enforced: operations run after the entries they read are signed and before the first entry that contains a refreshed file, so that bundle (the wrapper) signs last over the final bytes; an operation may not read what a later one writes. JSON is rewritten in place with key order kept, and a file the writer would have to reformat is refused. Evidence gains a `refresh` list |
| Checks | `deps`, `hardening`, `harden`, `hardeningscan`, `cargo-target` |
| Update | `update check\|apply\|gate\|land\|discard\|run\|schedule [--repo <path>]`: runs the `right-update` bin of the `@rightkit/outdated` this package pins (an exact dependency), because pnpm links only an app's direct dependencies' bins and no app has `right-update` on its path. It reads `.rightkit/update.json`; `pnpm exec right-release update run` is what a stale-pin refusal tells an app to run. A missing updater exits 2 |
| Ownership | `ownership`, `ownership-approval`, `ownership-report`, `ownership-mandatory`, `adoption-evidence` |
| Test | `test` (the app's declared suites) |
| Legal | `license-notices npm`, `license-notices cargo`, `license-notices binaries` |
| Other | `dev`, `doctor`, `suite-doctor`, `qa`, `dmg`, `cache`, `disk` (see "Disk hygiene on the agent volume"), `apple`, `inventory`, `model`, `addon`, `hooks`, `lsclean`, `generate-dmg-background`, `publish cargo`, `publish npm`, `publish swift` |

## The app check: deterministic first, no flag skips it

`right-release check [--repo <dir>] [--config <file>] [--commit <rev> | --worktree] [--only <rule,...>] [--list] [--print-baseline] [--baseline <file>]`
is the offline app check, the same as `rightkit-gate check`: it reads files only (the committed tree of `--commit`, default HEAD, or with
`--worktree` the tree on disk, which is what a compile would see), runs every rule and prints every finding in one run. Exit 0 passes,
1 has findings, 2 could not run (a repository with no commit yet says `the repository has no commit yet; commit first`). The repository
is the git top-level of `--config`'s folder, else of `--app-root` (`recipe-build`), else of the working directory.

The commands that compile, package, sign, ship or inspect a release run it first, in the same process, and start nothing when it is
red or cannot run (outside a git repository it is red). There is no flag that skips it: `--help`, `--dry-run`, `--check` and `--list`
only change what a command does after the check.

| Mode | Commands |
|---|---|
| `commit` (HEAD: what is built, packaged, signed and uploaded) | `release`, `build` (also `build --web` and the direct-flag form `right-release --platform ...`), `package`, `web-build`, `swift-package`, `recipe-build`, `qa`, `upload`, `publish`, `publish-update`, `github`, `sign-windows`, `sign-updater`, `create-mac-updater`, `sign-candidate`, `addon build` |
| `worktree` (the tree on disk, as a compile sees it) | `dev`, `doctor`, `test`, `stage-sidecar`, `sign-dev-artifact` |

Not guarded: the report forms of guarded commands (`publish cargo|npm|swift`, `qa audit`, `doctor --all`) and every other command.
Each of those is listed with its reason in `UNCHECKED_COMMANDS` (`app-check.mjs`), and `app-check.test.mjs` fails a command that is in
neither list. `dmg` takes `--app`/`--out` paths, compiles nothing and has no repository; `addon` is checked only as `addon build` (it
runs the add-on's own build command), its `doctor`, `upload` and `adopt` forms compile nothing.

The rules (`right-release check --list` prints their ids and gaps; `scripts/preflight.mjs` adds RightKit's own):

| Rule | Gap | Finds |
|---|---|---|
| `eol-policy` | G04 | a committed blob with CRLF where `.gitattributes` says `eol=lf` |
| `file-url` | G12 | the `.pathname` of a URL built from `import.meta.url` used as a path (wrong on Windows; use `fileURLToPath`) |
| `retired-alias` | P11 | an invocation of `rightkit`, the retired name of the RustRight client, outside history |
| `qa-platform` | G13 | a Rust `"platform:<x>"` requirement that is not `darwin` or `windows` (the test skips forever) |
| `test-harness` | G14 | a Cargo `[[test]]` whose file defines `fn main(` but lacks `harness = false` (libtest runs 0 tests) |
| `scenario-skip` | G15 | a `.scenario(` test that prints SKIP and returns (a skip that reports green) |
| `release-config` | G16 | `right-release.config.json`, read as data (or a legacy `right-release.config.mjs`): a provider `publish-update.mjs` refuses, an updater `keyFile`, an undefined Cargo profile; JSON that `right-release` cannot load is a finding |
| `skip-checks` | G17 | a `package.json` script that runs `right-release` with `--skip-checks` |
| `qa-dev-server` | G18 | a `package.json` `qa*` script that runs `tauri dev` (QA would test the dev server, not the app) |
| `ffmpeg-release-escape` | G19 | `allow_in_release_builds` in non-test Rust outside `crates/rightkit-ffmpeg` |
| `switches-inventory` | G20 | a directory with a `right-release.config.json`, read as data (or a legacy `right-release.config.mjs`), and no `switches.json` (or its `releaseSwitches.inventory`) |
| `speech-link` | G21 | `import Speech` under `swift/RightKitSwift/Sources` or `Tests` |
| `e2e-under-cargo-test` | G22 | `cargo test`, `cargo run` or `cargo nextest` (plain, `rustright cargo`, or the retired `rightkit`) that runs an end-to-end crate in the broker: it holds the compile slot for the whole run |
| `suite-key-literal` | C3-04 | the Right Suite licence public key pasted as a byte array, 64 hex digits, base64 or `\x` escapes, or the updater key text outside a `tauri.conf.json`, in a non-test file; take it from the contract's generated constants |
| `suite-updater-key` | C3-14 | a `tauri.conf.json` `plugins.updater.pubkey` that is not a copy of a key in `contracts/right-suite-api.v1.json` (missing, a placeholder, another key or an unparseable file); a platform override may leave it to the base config |
| `engine-record-store` | C3-05 | a call to `from_record_store` outside `crates/rightkit-license`: the owner binary hands the engine a snapshot through `rightkit_credentials::handoff` instead |
| `shell-window-hidden` | SC-R1 | a window created at startup (desktop targets) without `visible: false`; a platform patch that restates `app.windows` must restate it |
| `shell-plugin-order` | SC-R2 | the shell plugin installed from `setup`, or behind a non-QA plugin (the shell's `single_instance_plugin` and QA plugins may come first; a shell built with `single_instance(false)` may follow the plugin that arbitrates) |
| `shell-handler` | SC-R3 | an `rk_shell_*` command the renderer invokes (a literal, the body of an `@rightkit/tauri-shell` import, or the members it reaches on its `createShell()`: `shell.window.close()` is `rk_shell_window`) that is neither listed in `generate_handler!` nor covered by `handler!` (the command map of `@rightkit/tauri-shell` ships in the package as `shell-contract-bridge.json`, generated by `scripts/generate-shell-contract-bridge.mjs`); a shell it cannot follow needs at least one listed |
| `shell-csp` | SC-R4 | no CSP, or one without `default-src 'self'`, `frame-ancestors 'none'` or `connect-src`, or with `unsafe-inline` / `unsafe-eval` in `script-src` (`default-src` when there is none); `style-src 'unsafe-inline'` and `ipc:` are fine |
| `shell-grants` | SC-R5 | a `core:window:allow-*` grant that `rk_shell_window` mirrors (minimize, maximize, unmaximize, toggle-maximize, set-fullscreen, close, hide, show) while that command is registered; start-dragging and verbs the shell has no command for stay allowed |
| `qa-window-label` | SC-R6 | QA `Control` bound to a label other than the shell's `main_label` (a bare `Control` binds the launcher's `[ui] window_label`, which defaults to `main`, so an app whose window is not `main` names it with `.window(..)`), or a main label no config or code window carries |
| `devtools-declaration` | SC-R7 | the five sites (release config `devtools: "content-only"`, `devtools-policy.json`, `content-devtools` feature, `build.rs` ACL manifest, `.devtools_policy(ContentOnly)`) disagree once any of the declaration, the feature or the policy is present |
| `shell-lifecycle-external` | SC-R8 | `lifecycle_only` / `lifecycle_plugin` beside a `WebviewUrl::External` window; `host_windows()` keeps hardening and the navigation policy |
| `legal-receipt-native` | SC-R9 | `<Onboarding>` (`legalMode` not `none`/`notice`) with no `legalReceipt` or a hand-written one, or a `LegalGate` that stamps the acceptance with the webview clock while the app has no `createNativeLegalReceiptPort` |
| `release-config-identity` | CA-01 | a `right-release.config.mjs` whose default-exported object declares `schema` but whose top-level `app` or `version` is missing, `undefined`, `null`, empty or not a string (read as text, never run); a value it cannot prove from a string literal or a top-level `const` string in the same file is an UNSUPPORTED finding, not a pass; a JSON config needs strings, or a `$fromJson`/`$fromToml` node whose selected value (resolved from the committed file as `release-config.mjs` does) is a non-empty string |
| `pnpm-node-floor` | CA-05 | a workflow job that runs pnpm and sets up a Node below what the pinned pnpm needs (pnpm 11: Node 22.13, `consumer-contracts.json`), or a `.node-version`/`.nvmrc` below it |
| `rust-dependency-msrv` | CA-05 | a `Cargo.lock` crates.io entry whose `rust_version` (the pinned crates.io index snapshot in `consumer-contracts.json`) is above the concrete channel of the nearest `rust-toolchain(.toml)`; `stable` and versions the snapshot lacks are not judged |
| `release-age` | CA-06 | a `pnpm-lock.yaml` package published (pinned npm snapshot in `consumer-contracts.json`) after the check's clock less `minimumReleaseAge` (explicit, or the pnpm 11 default of 1440 minutes) and not in `minimumReleaseAgeExclude` |
| `notice-lock-digest` | CA-14 | a notice or licence file whose `<lock> sha256=<hex>` (right-release license-notices, @rightkit/legal npm appendix) or `<lock> sha256 <hex>` (legal-snapshot notices) differs from the committed lock (a legal-snapshot `notices.hashes` entry, else the nearest lock of that name); a lock digest in another form, or for a lock the tree does not hold, is UNSUPPORTED; history folders (evidence, fixtures, plans, archive) are not judged |
| `doc-comment-bare-cr` | CA-15 | a carriage return that ends no line inside a Rust `///`, `//!`, `/** */` or `/*! */` doc comment (rustc: bare CR not allowed in doc-comment); CRLF line ends and plain comments are fine |
| `checkout-step-input` | CA-07 | a workflow step that reads a tracked path (a script, a source file, a `working-directory`) its job's cone-mode sparse checkout does not materialize; git pathspecs, globs and comments are not inputs, and a full checkout reads everything |
| `source-relation` | CA-09 | a value in one committed file that names a row another committed file does not have: the relations RightKit knows (`SOURCE_RELATIONS`: SwiftRight's `hand-entries.json` `statusDocBlock` to a `docs/sr1-status.md` table row) plus an app's own `.rightkit/source-relations.json` (`{ "schema": 1, "relations": [{ id, source, select, target, adapter: "markdown-table-first-cell" }] }`) |
| `checkout-longpaths` | CA-11 | a job on a GitHub-hosted Windows runner whose checkout holds tracked paths over the Win32 limit under `D:\a\<repo>\<repo>\` (`<repo>` from the GitHub `origin` remote, never the checker's directory; file 259, directory 247 characters) with no step before it that provably runs `git config --system\|--global core.longpaths true` as its whole script (or `GIT_CONFIG_COUNT` covering a `KEY_n`/`VALUE_n` pair); a conditional, echoed, custom-shell or action form, or an unknown repository name, is an `UNSUPPORTED:` finding, never a pass; long paths outside the sparse cone, off Windows or on a self-hosted runner are fine |
| `workflow-action-pair` | CA-04 | one job of a workflow runs `github/codeql-action` `init`, `autobuild`, `analyze` or `resolve-environment` from different refs (a floating tag beside a commit counts); separate jobs, `upload-sarif` and other repositories keep their own refs |
| `referenced-input` | CA-07 | a `package.json` script or workflow `run:` that runs `node`/`sh`/`bash`/`python <file>` absent from the commit or outside the repository, a `pnpm --dir`/`-C`/`npm --prefix` directory, a workflow `working-directory` or `actions/setup-*` `cache-dependency-path` the commit lacks; ignored (`.gitignore`) and build-output paths, `${...}`/globs, directories an unconditional `actions/checkout` or `actions/download-artifact` step earlier in the same job writes (`path:`), and `fixtures/` trees are not judged; an input only a later step, another job, an `if:`/`continue-on-error` step or an `actions/cache` restore could produce is reported UNSUPPORTED |
| `caddy-global-options` | CA-08 | a Caddyfile whose first block is the global options block (no address) holding a site directive (`encode`, `reverse_proxy`, ...), or whose first address is only `{$VAR}` placeholders with no default (an empty variable turns it into the global options block); an explicit global block first or `{$VAR:default}` passes, unknown names (plugin options) are not judged |
| `legion-schema-projection` | LG-02 | in a tree with Legion's schema generator, a schema it byte-compares that the checked commit's own `.gitattributes` leaves to line-ending conversion (info/attributes, global and system attributes do not count) (Windows checks it out as CRLF and `--check` reports SCHEMA DRIFT), or an `enum` that is not the code-owned Rust `&[&str]` list it projects |
| `legion-legacy-tokens` | LG-03 | in a tree with Legion's naming ledger, a legacy token (`seer`, `nemesis`, `forge`, `sentinel`, `sorcerer`) in a path or file that `src/config/naming-legacy-allowlist.json` does not classify, or classifies with another exact count; immutable qualification evidence is exempt as Legion's producer exempts it |
| `legion-retirement-ledger` | LG-04 | in a tree with Legion's retirement ledger, a skill, script, hook, doctrine, agent-rule or native command file deleted after `c1d80c9d` (bound to the full id `c1d80c9d190e0f45dfd630d4d65e8bed748381ce`) with no `docs/provenance/retirements.md` row; a shallow clone fails as unverified |
| `legion-authority-body` | LG-05 | in a tree with Legion's roster, an agent card without its doctrine body verbatim (a variant also its two roster bodies), a card naming `doctrine/` or `src/roster/` outside its method, or a council-seat body that differs from doctrine |
| `retired-token` | CA-13 | a token the app retired (declared in `.rightkit/consumer-policy.json` `retiredTokens`: tokens, scoped globs, extensions) in a scoped file, outside a whole-file (`allowFiles`) or exact-fragment (`allowFragments`: text, `identifier` or `string`) exception, each with a reason |
| `route-deny-policy` | CA-13 | the app's declared deny set (`routeDenyPolicies`: a policy JSON of `denied`, `exceptions` and `irreversible` segments and fragments) disagrees with its committed route table or OpenAPI document: a denied or excepted route that is not a route, a DELETE not denied, an irreversible-looking mutation neither denied nor excepted, a route both denied and excepted, or a denied route the irreversible marks do not cover |
| `layer-reach` | CA-13 | a value import chain (read lexically, never run) from a declared `from` file reaches a `forbid` file of the same `layerBoundaries` entry; a `stop` file is a reviewed API, accepted and not walked; a load with a non-literal specifier is a finding |
| `spec-route-parity` | CA-13 | the committed API spec and the committed route table disagree (`specRouteParity`): a documented route the spec lacks, a spec operation that is not a route, or a declared undocumented route the spec documents |
| `source-closure` | SF-01 | in any committed `*.json`, a sealed record `{ sourceClosure: { sources: { "<path>": "sha256:<hex>" }, digest } }` whose sources are missing from the tree or changed since sealing, or whose `digest` is not sha256 over the canonical JSON of `sources`; a digest over a projection is declared in `.rightkit/source-closures.json` by naming a RightKit-approved transform identity (`APPROVED_TRANSFORMS`: source path, families, dropped keys and input schema are RightKit's), applied to the current bytes and compared; the repository never supplies pointers. Membership a producer would now add is not visible (it is computed by repository code) |
| `mandatory-foundation` | OW-05 | an app (root `rightkit-mandatory.json`, or a `right-release.config.(mjs\|json)` at the root or beside a `src-tauri/tauri.conf.json`) without its declaration; or whose profile requires a foundation owner of `ownership-mandatory-set.json` that it does not declare as a dependency (missing or dev-only), or that selects an unknown or unratified profile; or a `notApplicable` row that is not a ruling ratified for the declared `app` in `appAmendments`, reason verbatim (the row then still applies). Owner-pending rows never fail |
| `ownership-exceptions` | OW-06 | a `.rightkit/ownership-exceptions.json` with an unknown key, or an entry without `capability`, `package`, `reason`, `owner`, `approvedBy`, a real `date`, a real `expires` and an `issue` naming a RightKit issue; an entry past its `expires` date fails with no identity, so no baseline silences it; an entry no ratified ruling (`appAmendments.<app>.exceptions`: capability, package or crate, path when named, RightKit issue, expiry no later than the ruling) covers |

The CA-13 rules check contracts an app declares about itself in `.rightkit/consumer-policy.json` (schema 1; the shape is in the
header of `consumer-policy.mjs`). An app without the file has nothing to check. They read JSON and source text only: no repository
script, generator or config runs, so a contract that can only be judged by running the app's code (an OpenAPI body generated at
runtime, for one) stays with the app's own test.

`e2e-under-cargo-test` (G22): the broker compiles; it does not run end-to-end tests. An end-to-end crate is a Cargo package that depends on
`rightkit-qa` not optionally (the optional dependency is an app's in-app control server) and has a `.rs` file outside `examples/` that names
a driver module of it (`harness`, `control`, `suite`, `scenario`, `engine`, `in_app`, `interactive`, `page`, `browser`, `android`); a name
like `*-e2e` is not evidence. A command selects it by `-p`/`--package`, `--manifest-path`, `--workspace`/`--all` (the members of the
workspace in the working directory, less `--exclude`), `--test`, or for `cargo run` `--bin`; and by naming nothing at all in a virtual workspace (a
`[workspace]` with no `[package]`): `cargo test` and `cargo test --manifest-path Cargo.toml` test every member, or the `default-members` when it declares
them, less `--exclude` (a root that holds a package of its own tests that package only, so a bare run selects nothing there). It looks in `package.json` scripts, shell,
PowerShell, batch, Node and Python scripts, Makefile and justfile recipes, workflow `run:` steps, git hooks, and code in `README.md`,
`AGENTS.md`, `CLAUDE.md` and `docs/` (history documents excluded). `--no-run`, `rightkit-qa run-tests`, `rightkit-qa journeys run` and
`--test qa_surface_guard` are not findings. A code span in a runbook's running text that is only the command (`` `cargo test` ``, `` `rustright cargo test` ``, no argument) is a command line only where an unnegated imperative (run, use, execute, invoke, call, type) stands right before it in its clause ("Run `cargo test` before you push"); "`cargo test` unit tests", "`cargo test` would hold the compile slot", "only CI runs `cargo test`", a list label "`cargo test`: Rust unit tests" and "do not run `cargo test`" name the command and are not findings. With an argument, in a fenced block, a script, a `package.json` script, a recipe, a workflow step or a hook, the same command is judged whatever stands around it; a PowerShell `<# ... #>` block comment is a comment. The identity is `e2e-under-cargo-test|<file>|<script or crate>`. Fix: compile with `--no-run`, then
`rightkit-qa run-tests -p <crate> [--test <target>] -- <libtest args>`; a whole-workspace run must also leave the crate out (`--workspace --exclude <crate>`),
since `--no-run` on the workspace compiles it all and runs none. `cargo run` has no `--no-run`: the finding says to build the program in the broker
(`rustright cargo build -p <crate> --bin <bin>`, with `--manifest-path` for a crate outside the workspace in the working directory) and run the built binary
outside it, or `rightkit-qa journeys run`.

Known findings are tolerated only through a baseline: `--baseline <file>`, else the checked tree's `.rightkit/app-check-baseline.json`,
else (from a RightKit checkout only) `ci/app-check-baseline.json`. The app's own file is always `{ "schema": 1, "known": [{ "id", "issue",
"added", "sunset" }] }`; the central `{ "known": { "<repo>": [...] } }` shape in an app's file is refused (exit 2). Every app entry needs
an issue, an `added` date (YYYY-MM-DD, UTC, not later than today) and a `sunset` that is not before `added` and at most 90 days after
it. Past its sunset an entry stops tolerating its finding (the finding fails with `its baseline entry expired`) and is reported as
`EXPIRED`; a listed identity that no finding has any more is `STALE` and fails, so the list only shrinks. A rule the build refuses
whatever a baseline says (`switches-inventory`) fails even when listed.

## Rule tiers: compile and release

Adrian, 2026-10-26: the apps lost every build route on 2026-10-25 and build again through the one door on the rule set they were last green on, before any newer rule is asked of them. `app-check.mjs` `TIERS`:

| Tier | Rules | Owed by |
|---|---|---|
| `compile` (default) | the 13 rules @rightkit/release 0.6.6 published (`COMPILE_TIER_IDS`) | compile admission: the RustRight broker, the pre-push hook, `rightkit-gate check`, `--to compile\|package`, `right-release build\|dev\|test\|doctor` |
| `release` | every rule, the ones added since 0.6.6 included (C3, SC, CA, LG, SF-01, OW-05, OW-06), plus the `shellcheck` scan and actionlint's shellcheck requirement | `rightkit-gate --to qualify` (it runs `check --rule-tier release` before installed QA and records `ruleTier` in the receipt) and `right-release release\|qa\|upload\|publish\|publish-update\|github\|sign-*\|create-mac-updater\|sign-candidate` (`RELEASE_TIER_COMMANDS`) |

`rightkit-gate check --rule-tier compile|release` selects the tier (`--tier patch|update` is the upload tier); `--list` names each rule's tier; `--only` is a view over the whole set. A rule is promoted to the compile tier by moving its id into `COMPILE_TIER_IDS` in RightKit, never per app and never through a baseline. RightKit's own preflight runs every rule.

## The pre-push gate on a host without the broker: the Rust gate is deferred, the HOST RECEIPT gates the merge

The gate chooses its executor itself (`gate-executor.mjs` `resolveExecutor`), never the caller: the RustRight broker where the machine has it (`rustright` on PATH, or its managed cargo shim), a refusal on a broker-managed machine whose broker cannot be reached, and direct `cargo` on any other host (a cloud session, a CI runner). Every host runs the same gates in the same order (app rules, class A, then the compile scans); an unmanaged host is never sent to a broker and nothing is deferred to one. Every pushed ref is judged in any position (git's stdin is split on single ASCII spaces, so a Unicode space in a ref name cannot hide one), and a stdin line that is not four fields refuses the push on every host and in every repository, because git sends the local field as typed and a rev expression such as `HEAD^{/one thing}` holds spaces: such a line is never dropped, so the pushed commit is always compared with HEAD.

`scriptBrokerCall` reads `package.json` scripts only: it follows `pnpm <script>`, `pnpm run`, `npm run`, `yarn` and the wrapper words, not `node -e`, a node script, `sh -c`, or a run in another package (`pnpm --dir`, `--filter`, `-r`). A unit whose test reaches the broker that way says so in the app's own `ci/gate.config.json`, in the `npm` entry the unit's `quietTests` already uses (the key is the unit name: the package name, or the directory with `/` as `-` for a unit of a pnpm root below the repository root):

```json
{ "npm": { "heardright": { "requiresBroker": { "reason": "the root test runs tauri-app-next/scripts/verify-local.mjs, which spawns rustright cargo test" } },
           "tauri-app-next": { "requiresBroker": { "reason": "test runs scripts/run-contract-tests.mjs, which spawns rustright cargo test" } } } }
```

The `reason` is required and is printed. The file is read from the gated commit, so the declaration is a reviewed change in the diff that adds it. Where the Rust gate is deferred, the unit's `test:npm:<unit>` scan is deferred with it and named `declared in ci/gate.config.json npm.<unit>.requiresBroker: <reason>`, never reported as passed; on a host with the broker, in CI, on a protected ref and with the opt-out it runs as it did. A declaration of any other shape (`true`, a string, an empty or blank `reason`, an array) is ignored with a note and the test runs where it can: a malformed entry never skips a test. `false` or `null` is no declaration, and a call the scripts do show is reported from the scripts. The gate does not read node scripts for a literal `rustright` or `cargo`: a literal cannot tell a spawn from a command table, a mention or a test fixture, so the declaration is the way.

## The pre-push hook finds the gate in a repository whose root installs nothing

`rightkit-gate install-hook` writes `.githooks/pre-push` (and sets `core.hooksPath`). The hook runs the repository's own `.git/hooks/pre-push`, then `rightkit-gate check --commit HEAD`, then the gate, and finds the gate binary in `node_modules/.bin/rightkit-gate` of the repository root, else of any workspace package up to three levels down, else through `pnpm exec`, else with `node` at a path written into the hook. That path is always relative to the repository, the same for every version and a plain name, never absolute, never `../` out of the repository, never a `node_modules/.pnpm/@rightkit+release@x.y.z/` store path, and it is decided by what the tracked `package.json` files declare, never by what is installed: the same repository gets the same hook on a fresh clone, from an exports checkout, after `pnpm install` and after a version bump, so `install-hook` as a `prepare` script never meets a hook it wrote earlier as one that "already exists and differs". When the root `package.json` declares `@rightkit/release` (alone, or together with packages: Pulse declares it at the root and in `hub/`), the path is the root's `node_modules/@rightkit/release/bin/rightkit-gate.mjs` and the hook is the single-root form. When only packages declare it (CutRight's `apps/cutright`, HeardRight's `tauri-app-next`), the path is the same link in the first of them in sorted order, whichever is installed, the hook also tries each declaring package's `node_modules/.bin/rightkit-gate` directly (deeper than the glob reaches) and it runs the `pnpm` fallback as `pnpm --dir <package> exec rightkit-gate`, because `pnpm exec` at a root that does not declare the package has no `rightkit-gate` to run. When no `package.json` declares it, the gate's own path inside the repository if it runs from there and not from a `node_modules` (a checkout of the package itself), else the root's link. A declaring package whose directory is not made of letters, digits and `_ - . @ +` is named in a warning and left out. A root-install repository gets, byte for byte, the hook that 0.6.4 and 0.6.5 wrote (`fixtures/published-gate-hooks.json`); a multi-root repository whose committed hook still has a baked path regenerates it once with `rightkit-gate install-hook --force`, and the result no longer changes with the version or the install.

## The rustfmt scan reads each file's edition from its crate

The class A `rustfmt:<edition>` scans group the changed `.rs` files by the edition of the crate they belong to: the nearest `Cargo.toml` at or above the file decides. A literal `edition = "NNNN"` in its `[package]` table is that edition. `edition.workspace = true` (or `edition = { workspace = true }`) takes the `[workspace.package] edition` of the owning workspace root, the nearest `Cargo.toml` at or above the crate that declares `[workspace]` (the crate's own manifest, for a root package that is also the workspace). When the inheritance cannot be followed (no workspace root, a root with no literal `[workspace.package] edition`, a manifest in the chain that cannot be read, or an `edition` entry in a spelling the scan does not recognise such as `edition = 2021`) the gate fails with a `path-policy:*` finding that names the manifest and the reason, once per manifest, and those files are not checked under a guessed edition. Fix the manifest (declare `[workspace.package] edition`, or write the edition in the crate). A manifest that names no edition at all, and a `.rs` file below no manifest, are checked as 2021.

## The repository secret scan: reviewed records, not guesses

The class A `secret-scan` scan reads every file of each changed crate and package. Crate packaging, the package archive and the local-publish batch scan are strict: every `.env*` file, key file (`*.pem`, `*.p8`, `*.p12`, `*.key`) and env template is refused by name, and content that matches the credential shapes (`SECRET_PATTERNS`: a PEM private key header, `AKIA...`, `gh[pousr]_...`, `npm_...`) is refused. The repository gate (`scanCrateForSecrets(dir, { envTemplates: true, repoRoot })`) is the only mode that reads the repository's records, `.rightkit/secret-scan-records.json` of the gated commit:

```json
{ "schema": 1, "records": [
  { "path": ".env.example", "sha256": "<hex of the bytes, CRLF read as LF>", "kind": "env-template", "note": "local development defaults only, no credential" },
  { "path": "engine/src/redaction.rs", "sha256": "<hex>", "kind": "fixture", "note": "redaction test vectors, fake values" }
] }
```

| Finding | Admitted by | Never admitted |
|---|---|---|
| An env template (`.env.example`, `.env.<name>.example`, `.env.sample`, `.env.template`) | a record of kind `env-template` whose `sha256` is the file's | a template that is not UTF-8 (UTF-16 byte-order mark, NUL bytes, invalid sequence); one that holds a PEM private key header, `github_pat_...`, `sk_live_...`, `xox[abprs]-...`, `AIza...`, a Slack webhook URL or a `SECRET_PATTERNS` shape; one inside a publishable crate (its `[package]` does not say `publish = false`) or package (`package.json` not `"private": true`) |
| Any other file whose content matches `SECRET_PATTERNS` (a redaction or screening test vector) | a record of kind `fixture` whose `sha256` is the file's | |
| `.env`, `.env.local`, any other `.env.*` that is not a template, `*.pem`, `*.p8`, `*.p12`, `*.key` | nothing: refused by name, and a record naming one is itself a violation | any |

A violation reads `<path>[:<line>]: <reason>`. For a template or a fixture it carries the file's sha256 and the exact record to add, with a placeholder note: review the file, write the note (what it holds and why it is safe; the placeholder is refused), and commit the record. Any edit to a recorded file changes its sha256, so the file fails again until its record is replaced, and the change of the record is the review in the pull request diff. A violation names the line and the kind of shape a hit matched, never the matched text. A malformed records file is a violation: bad JSON, a schema other than 1, an unknown key or kind, a missing, empty or placeholder note, a `sha256` that is not 64 hex characters, a `path` that is not a normalised repository-relative path, a duplicate path, a kind that does not fit the path (`env-template` for a file that is not a template, `fixture` for a template). A record with a problem admits nothing, and the file must be a regular file inside the repository (a symbolic link is refused). Records are keyed by the repository path, so a moved file needs a new record, and the records file is scanned like any other file, so a note must not quote a credential.

## The npm scans the gate plans

The gate takes the npm layout from the tracked files. Every pnpm root is discovered: the repository root, and any other directory outside `node_modules` (at most 6 levels deep, at most 64 roots) that holds a `pnpm-lock.yaml` or a `pnpm-workspace.yaml` beside a `package.json`. CodeRight has two roots, `apps/coderight-tauri` and `apps/coderight-web`, and no package at the repository root. Each root gets `pnpm-lockfile` and `pnpm-audit` run in its own directory (`pnpm-lockfile:<root>` and `pnpm-audit:<root>` below the repository root), planned when a unit of the root is in scope or its own package.json, lock or workspace file changed; each unit gets `tsc:<unit>`, a linter and `test:npm:<unit>`. A unit of the repository root keeps its package name in those ids; a unit of any other root is named from its directory (`apps-coderight-web`), so its ids carry the root path. A repository with one root plans what it always did. A unit is linted by the tool it configures: `eslint:<unit>` where an ESLint config (`eslint.config.*`, `.eslintrc*`) exists in its directory or any directory above it, `biome:<unit>` (`pnpm exec biome lint .`) where a `biome.json` or `biome.jsonc` does, both where both do and neither where neither does.

The scans that read the live checkout (cargo, pnpm and the tests) run only when the checkout equals HEAD where they read it: any change or untracked file there blocks the gate (`BLOCKED: the checkout differs from HEAD where broker scans read it`). The paths checked are the Rust units and workspaces and their shared build inputs, every npm unit's directory, each pnpm root's `package.json`, lock, workspace file, `.npmrc` and linter configs, and, for each npm unit, every ESLint and Biome config name in every directory between the repository root and the unit (`cleanCheckoutPaths`), because those are the files `eslint:<unit>` and `biome:<unit>` are planned from: an uncommitted `apps/eslint.config.js` above `apps/cutright` blocks the gate like one inside it.

The class B pnpm scans (`tsc:<unit>`, `eslint:<unit>`, `biome:<unit>` and `test:npm:<unit>`) run with `pnpm_config_verify_deps_before_run=false`. pnpm 11 verifies before every `pnpm run` and `pnpm exec` that `node_modules` matches `pnpm-lock.yaml` and, if not, runs `pnpm install`, which wants to purge the modules directory and aborts without a TTY (`[ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY]`) before the tool or the test starts. The gate never installs into the checkout it reads; it verifies the lockfile itself (`pnpm-lockfile` in the clean export) and runs the scans against the tree as installed, so keep the tree installed (`pnpm install`) before a push: a tree that lacks a dependency fails in the tool or the test with its own error. The setting is part of each scan's environment (shown by `--plan`, in the cache key), and a failed scan's detail leads with the first `[ERR_PNPM_...]` line pnpm printed.

## Declared test suites, web build, npm notices and artifact mirroring

These replace per-app wrapper scripts. Each is declared in `right-release.config.mjs` and runs without a shell from the app
root (the config file's directory). A step is `{ cmd | node, args?, cwd?, env?, nodeFlags?, nodeOptions? }`: `node` is an
app-relative script run by the Node that runs `right-release`, `cmd: "node"` means the same Node, `nodeFlags` go before the
script, `nodeOptions` are merged once into `NODE_OPTIONS`. `cwd` and `node` must stay inside the app; unknown keys are refused.

### `right-release test`

```js
tests: {
  eventPrefix: "e2e",                       // optional, default "test": events are <prefix>_suite_started / _completed
  runners: {                                // optional named bases
    vitest: { node: "node_modules/vitest/vitest.mjs", nodeFlags: ["--no-experimental-webstorage"], nodeOptions: ["--no-experimental-webstorage"] },
  },
  suites: [
    { name: "tooling", runner: "vitest", args: ["run", "scripts/tooling.test.mjs", "--no-file-parallelism", "--maxWorkers=1"] },
    { name: "runtime-config", node: "scripts/runtime-config.test.mjs", nodeFlags: ["--test"] },
    // Expands to journeys-1, journeys-2, ...: the names in the JSON list run `chunk` at a time, one process per chunk.
    { name: "journeys", node: "scripts/qa-browser-functional.mjs", args: ["--keep-going"],
      journeys: { file: "scripts/qa-e2e-journeys.json", key: "journeys", chunk: 12, flag: "--journey" } },
  ],
}
```

`right-release test` runs every suite in order and stops at the first failure, exiting with that suite's code; each suite prints
a JSON line before and after (`{"event":"e2e_suite_completed","suite":"tooling","exitCode":0,"durationMs":1234}`). `--suite a,b`
(repeatable) selects suites; a group name such as `journeys` selects all its chunks. `--runner vitest -- run --project unit`
runs a runner once with the arguments after `--` (watch mode, one project); `-- <args>` after `--suite` is allowed when exactly
one suite is selected. `--list [--json]` prints the resolved commands and runs nothing. Wrap it as the repository needs, for
example `rustright storage run --app <app> -- right-release test`.

### `right-release build --web` (or `web-build`)

```js
web: { build: [
  { node: "node_modules/typescript/bin/tsc", nodeFlags: ["--disable-warning=DEP0205"] },
  { node: "node_modules/vite/bin/vite.js", args: ["build"], nodeFlags: ["--disable-warning=DEP0205"] },
] }
```

Runs only the `web.build` steps: no Tauri, no signing, no packaging. `--dry-run` prints the commands. It is guarded by the same
GitHub-Actions-only execution policy as the other build commands.

### `right-release license-notices npm`

```js
legal: { npmNotices: { output: "legal/THIRD_PARTY_NPM_LICENSES.html", productName: "ViewRight", exclude: ["@rightkit/"] } }
```

Writes the third-party npm license appendix for the production closure of a pnpm app (`pnpm licenses list --prod --json`),
with each package's own LICENSE / COPYING / NOTICE text. A package with no license file gets the standard published text for
its declared MIT or ISC license, labelled as a fallback; any other license without a file fails the run. The page records the
app version, the `pnpm-lock.yaml` sha256 and `generated-by=right-release license-notices npm`, so equal inputs give equal bytes.
`--check` writes nothing and exits 1 when the file differs. `--output` and `--product-name` override the config.

### `right-release license-notices cargo`

```js
legal: { cargoNotices: {
  output: "legal/THIRD_PARTY_CARGO_LICENSES.html",
  template: "legal/about.hbs",
  config: "legal/about.toml",                 // optional cargo-about config
  manifestPath: "src-tauri/Cargo.toml",       // optional Cargo manifest
  features: ["desktop"],                    // or allFeatures: true
} }
```

Runs `cargo about generate --locked --fail` through managed Cargo, with declared config, manifest, features & template.
Inputs & output stay inside app root. Missing inputs, unresolved licenses, command failure or empty output fail generation.
`--output` overrides config; `--check` writes nothing & exits 1 when generated bytes differ.
`rightkit-legal rust-licenses` delegates to this route.

`license-notices binaries` generates notices for declared native binaries; `--receipt` checks staged non-catalog programs
against declared coverage. Both routes accept `--config`, `--output` & `--check`.

### Mirroring an artifact (`mirror-root-artifact`)

```js
targets: { win: {
  mirror: [{ file: "src-tauri/target/x86_64-pc-windows-msvc/release/bundle/nsis/App_1.0.0_x64-setup.exe", pruneSuffix: "-setup.exe" }],
  postSign: { cmd: "right-release", args: ["mirror-root-artifact", "--config", "right-release.config.mjs", "--platform", "win"] },
} }
```

Copies each `targets.<platform>.mirror` entry (an app-relative path, or `{ file, name?, pruneSuffix? }`) atomically into
`RIGHT_RELEASE_ARTIFACT_DIR`. The directory must be outside the repository and the command refuses to run without it, so it
never writes into the repository root. `pruneSuffix` then removes the other files with that suffix from that directory, so it
holds only the current installer. The one-file form (`--file <path> --package-root <dir> [--name <name>]`) is unchanged.

### Broker-managed Cargo target directory (`cargoTargetRoot`)

With `cargoTargetRoot: true` (or an app-relative Cargo.toml path) in `winPackage` / `macPackage`, declared artifact paths under `<cargo dir>/target/` follow `cargo metadata`'s target directory. On a RustRight-managed host that directory is owned by the broker and lies outside the app (for example `D:\.rightkit-managed\rightkit-build-control\ws\<hash>\g\<n>\t`); release.mjs needs app-relative paths, so the recipe keeps the declared paths at their default location under the app's ignored `src-tauri/target/` and plans verified copies (`managed-target-stage.mjs`, the `stage` section of the recipe-build plan):

- after `tauri build`, the raw EXE (win) or the built `.app` (mac) is copied from the real target directory to the app-relative path; every later step (sign, package, seal) sees app-relative paths unchanged;
- win: `tauri bundle` embeds the EXE it finds in the real target directory, so the signed app-relative EXE is copied back there first, and the installer is copied out afterwards (its source is deleted before the bundle step, so a leftover can never be copied);
- every copy deletes a stale destination first, writes a temp file beside the destination, verifies sha256 (a directory: a digest of every entry, symlinks kept) against the source before and after the copy, then renames; a source the build did not produce fails the step with `STAGE_SOURCE_MISSING` and no stale copy is left to package.

Safeguards on the managed Windows package step (a temporary bridge until the broker exposes artifacts into the app; a RustRight issue tracks that): the real raw EXE must equal the signed in-app EXE by sha256 right after the copy-back and again after `tauri bundle` returns (`STAGE_REAL_CHANGED`: another cargo job in the same workspace may have relinked it), and the installer is copied out only after both checks. A copy whose destination is in the broker tree never creates its parent (`STAGE_PARENT_MISSING`: the generation was rotated or evicted). The recipe re-resolves the Cargo target directory through cargo metadata before and after the step and refuses when it differs from the directory planned at expansion (`STAGE_TARGET_DIR_CHANGED`; docs/RIGHTKIT-THIN-BUILD-CONTROL.md §5 and §15).

Only a target directory under a managed root (`resolveManagedRoots`: `RIGHTKIT_DATA_ROOT`, `RIGHTKIT_MANAGED_ROOT`, `RIGHTKIT_EXTERNAL_ROOT`, `RIGHTKIT_CONTROL_ROOT`, the legacy `D:\.rightkit-managed` / `/Volumes/D/.rightkit-managed`) is staged. Any other directory outside the app still fails closed: `the Cargo target directory <dir> is outside the app, so <path> cannot be addressed by an app-relative path; point CARGO_TARGET_DIR inside the app or declare the artifact paths yourself`.

### macOS updater tarball from `macPackage`

`targets.mac.macPackage.updater: true` (or an app-relative `.tar.gz` path) adds a `create-mac-updater` command at the front of
`postSign`: it packs the signed, stapled `.app` and signs the tarball. The default output is `<App>.app.tar.gz`, named after the
app in `notarize.app`. Declare the file in `updater.artifacts` as before. See `docs/MAC-PACKAGE-RECIPE.md`.

## QA receipts and admission (warn by default, refuse per app)

`right-release qa --stage source|installed --platform mac|win` runs `rightkit-qa` and writes a MAC'd receipt. Admission is a per-app policy in `qa-admission-policy.json`:

```json
{ "schema": 1,
  "apps": {
    "postright": { "mode": "refuse", "since": "2026-11-02", "set_by": "adrian" },
    "genright": { "mode": "warn", "since": "2026-10-24", "set_by": "adrian",
                  "stages": { "source": { "mode": "refuse", "since": "2026-11-09", "set_by": "adrian" } } } },
  "approvedRunners": [ { "sha256": "<64 hex>", "version": "0.9.0", "platform": "mac", "set_by": "adrian", "since": "2026-10-24" } ] }
```

**Per-stage modes.** An app entry is one mode for both stages, or carries `stages: { source?, installed? }` entries that override it for that stage (`source` is the build; `installed` is upload, publish-update and github). The `installed` stage cannot pass until the installed lane exists, so move an app to `refuse` for `source` first: `qa policy set --app <app> --stage source --mode refuse --set-by <name>`. Without `--stage` one mode governs both stages and any `stages` object is dropped. A stage that was `refuse` and is no longer (removed, or `warn`) needs `set_by: "adrian"`, checked per stage on write and on load.

- **`warn` (default; an app with no entry).** A missing or invalid receipt does not block. `build`, `upload`, `publish-update` and `github` print `[right-release] QA warning: ...` to stderr, continue, and record the outcome (below).
- **`refuse`.** The same refusal throws `QA admission refused for <app> (policy refuse since ... by ...)`; nothing is sealed, uploaded or published. Switch an app on with `right-release qa policy set --app <app> --mode refuse --set-by <name> [--stage source|installed]` (or `node release-qa-policy.mjs set ...`). Moving an app back to `warn` (or deleting its entry) is refused unless `set_by` is `adrian`; a hand edit that does so is caught on every load by comparing with the copy committed at `RIGHT_RELEASE_QA_POLICY_BASELINE_REF` (default `HEAD`; CI should use `origin/main`). `set_by` is a recorded claim, not an authenticated identity: the file must be owner-protected in the repository (CODEOWNERS and branch protection), and a downgrade that was already committed becomes the new baseline. An unreadable or malformed policy file is a hard error, never a fall back to warn. When a baseline ref is requested (`RIGHT_RELEASE_QA_POLICY_BASELINE_REF` or the `baselineRef` argument) and git cannot show the policy at that ref, the load fails closed instead of running without a baseline; with nothing requested a checkout without that history (an app's `node_modules`) still loads with no baseline.

**Approving a runner.** `right-release qa policy approve-runner --runner <file> --version <v> --platform mac|win --set-by adrian [--note <text>]` digests the file, runs `<file> --version` (the output must be exactly `rightkit-qa <v>`), requires `<v>` to be the published `rightkit-qa` in `rightkit-versions.json` (`registry.cargo`), re-digests the file to catch a swap while it ran, and appends `{ sha256, version, platform, set_by, since, note? }`. Only `adrian` may approve (a runner entry is new admission authority; a hand-added entry by anyone else is refused on load against the baseline). An approval is per platform because a runner's digest differs per host and OS; an entry with no `platform` (older files) approves the digest on either. Running the approved binary is the caller's job: set `qa.runner` in the release config to the managed path, since a bare `which rightkit-qa` can resolve to a different build.

Hardening, applied in both modes (a failure is a warning in `warn` and a refusal in `refuse`):

1. **Platform and release id.** The sealed receipt must be for the platform and the release id being sealed or published.
2. **Approved runner.** The `rightkit-qa` executable's sha256 recorded in the receipt must be listed in `approvedRunners` for the platform being sealed or published (an entry without `platform` approves either). An empty list approves nothing. The binary is digested before the run and again after it; a swap during the run is refused.
3. **Every release-critical journey.** With `qa.journeys` in the release config (`{ manifest?: "journeys.toml", receipt?: "<path>", args?: { source?, installed? } }`) `right-release qa` runs `rightkit-qa journeys run` (or reads `receipt`), validates the `rightkit.journeys.receipt/1` through `rightkit-qa journeys validate-receipt --manifest .. --tree ..` (exit code 0 and a final `ACCEPTED` line), and seals a `journeys` summary. `release-qa-floor.json` maps the `journey` floor id to "all release-critical journeys passed": at admission the required set is recomputed from the `journeys.toml` committed at the receipt's commit (every `release_critical` journey that lists the platform), and each must be `passed`. Zero critical journeys is never a pass. One matching check no longer satisfies the floor.
4. **Signing outside test execution.** Checked: the receipt records the test processes' pids and the window they ran in; admission refuses a receipt sealed before that window ended, signing that began before it ended (`signingStartedAt`), a sealing or admitting process that is a test process, `RIGHT_RELEASE_QA_STAGE` set at admission (admission or signing inside a QA run), and `right-release qa` itself refuses to seal inside a QA run. Not checkable here: that test processes cannot reach the signing key (OS credential store and signing agent ACL) and that CI runs QA and signing as separate jobs (the carried receipt, rightkit#183, is the hook).

`right-release qa` keeps the older behavior for evidence checks and verification of the receipt itself; the outcome is recorded:

- Source stage: sealed into the manifest as `qa: { status: "passed", sourceReceiptDigest, floorDigest }` or `qa: { status: "missing" | "rejected", reason }`.
- Installed stage: written to `.right-release/state/<releaseId>/<platformDir>/qa-installed.json` as `{ status, reason?, receiptDigest?, checkedAt, provider }`. It is written when publication starts, so a later upload failure still leaves a record.

Audit ad hoc or on a weekly schedule with `right-release qa audit [--app <dir>] [--json] [--since <YYYY-MM-DD>]`. It reads the app repo's sealed manifests and state and lists source and installed QA status per release and platform (passed, missing, rejected, not-uploaded). It exits 0 when every uploaded build has passing QA and 1 when any uploaded build lacks it. An installed receipt can only chain to a build sealed with a passing source receipt, so a build sealed with `qa.status: "missing"` needs source QA and a rebuild before installed QA can pass.

## Local publish (`publish npm|cargo ... --local`, Mac or Windows)

`right-release publish cargo --crate <name> --local` (and the batch form `--crates a,b,c | --crates-file <file>`) publishes from the Mac **or from Windows** under the committed local-build grant (`.rightkit-local-build.json`, `allow: "build-test-publish"`, naming this repository); `publish npm --package @rightkit/<name> --local` stays Mac only (the npm token comes from the macOS Keychain and `@rightkit/release` ships a signed helper), and `--local` refuses any other host (Linux) and any host without the grant. The gate is the same on both hosts: the unit's own `ci/gate.config.json` commands run with zero failures and no skipped evidence, the artifact sha256 is recorded, and the existing `enforcePublicationGate` and publisher run, including the registry readback. The evidence carries one receipt for the publishing host: `hostPolicy: "darwin-only"` with a `darwin` run on the Mac, `hostPolicy: "win32-only"` (plus a `hostPolicyReason`) with a `win32` run on Windows; the gate requires that host's receipt and `isDarwinOnlyPolicy`, `isWin32OnlyPolicy` and `singleHostOfPolicy` in `publication-gate.mjs` are the one definition. A crate whose `ci/gate.config.json` `os` list lacks the publishing host (`macos` or `windows`) is refused. Windows publishes with the credential Cargo stores itself (`~/.cargo/credentials.toml`): no `--token` is passed, read or logged. A runtime-budget crate needs the measurement of the publishing host only, under the same evidence policy that the budget file may not contradict.

`ci/gate.config.json` `rust.<crate>.consumerRows` adds gate rows that check the crate through a separate Cargo workspace: `{ "manifestPath": "ci/consumers/<name>/Cargo.toml", "args": [...], "kind": "check", "targets": ["<triple>"], "locked": true }` runs `cargo check --manifest-path <manifestPath> --locked --target <triple> <args>` once per target, after the crate's own checks, and records each as an evidence step (so `--from-evidence` requires it). Only `manifestPath` is required; unknown fields are refused. Clippy with `-D warnings` runs on every `featureSets` entry, not only the first.

## Local app install (macOS)

Agents must not run `osascript`, `open`, `screencapture`, or `notifyutil -p` directly. Install a signed build through:

```sh
right-release install-app --dmg <path.dmg> [--app-name Pulse.app] [--bundle-id dev.orthic.pulse] [--dest /Applications] [--relaunch|--no-relaunch] [--json]
```

It mounts the image read-only and hidden (`-nobrowse`), verifies the app (`codesign --verify --deep --strict`, `spctl --assess`, `stapler validate`, bundle id, and Team ID against the installed copy; a Team ID change is refused) before touching the installed copy, stages a `ditto` copy beside it in the destination and verifies that copy, quits the running instance with SIGTERM (15 s, then SIGKILL), swaps the staged copy in (one atomic exchange when the `rightkit-swap` helper is found, otherwise two journaled renames; the old bundle is restored on failure), re-verifies the installed copy, only then moves the old bundle to `~/.Trash/<App> <timestamp>.app`, and relaunches only if it was running or `--relaunch` is given, using `open -g -j -b` (background, hidden, never activated). `--json` emits one lifecycle event per line (`start`, `verified`, `quit`, `replaced`, `relaunched`, `done`/`failed`, among others). `right-release install-app --recover [--dest <dir>] [--app-name <App.app>] [--json]` rolls an interrupted install forward or back from its journal; every install runs that recovery first. Win32 exits with "not implemented on win32". Exit codes: 0 ok, 1 failed, 2 usage, 3 unsupported platform.

The shipped `bin/darwin/rightkit-swap` is Developer ID signed. Local publish adds it via `right-release publish npm --package @rightkit/release --local` on the Mac (managed `rustright cargo build --release -p rightkit-fs --bin rightkit-swap`, `codesign --options runtime --timestamp`, strict verify). CI qualification adds it via `publish.yml` build-swap → sign-swap → pack (`docs/CI.md`); sign-swap needs `APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_KEYCHAIN_PASSWORD` & `APPLE_DEVELOPER_ID` in the `release` environment, & stays blocked until root provisions the certificate pair. Exporting that identity requires an Aqua login session: SSH export fails `errSecInteractionNotAllowed` (-25308) & the Aqua signing agent exposes only codesign/updater/notarize/staple, no key export — do not retry either route. `install-app` refuses a helper that exists but is not validly signed (`SWAP_HELPER_UNSIGNED`, naming the fix); with no helper at all it uses the journaled two-rename fallback and logs it (`swap-fallback`, `replaced {swap}`).

Development builds (from `@rightkit/ci` `devArtifact`, Developer ID-signed but not notarized) install with `--dev` and exactly one of `--dmg`, `--zip <path.zip>` (extracted with `ditto -x -k`), or `--app <path.app>`: `right-release install-app --dev --zip dev-mac.zip`. `--dev` still requires `codesign --verify --deep --strict`, a Team ID, no Team ID change, and a bundle-id match; it skips only `spctl` and `stapler`, removes `com.apple.quarantine` from the new bundle only, prints `DEV BUILD (not notarized)`, and sets `dev: true` on every JSON event.

## One fixed path, one identity; release keeps one `.previous`, dev keeps none (every app)

macOS and Windows tie privacy grants (mic, screen, Files and Folders, Removable Volumes, Full Disk Access) to the path an app runs from and to its code identity. So every app, helper and dev or QA binary installs and updates IN PLACE at ONE fixed path under ONE identity; there is no per-version, per-digest, per-pid or per-build install folder or bundle id. A RELEASE install may keep one rollback `<name>.previous` beside the fixed path (rename it back). A DEV or QA install keeps NO old copy of any kind (Adrian, bogusyogi/rightkit#348: "when installing dev, old copy must never be backedup"; reinstall from an older DMG): it replaces the path in place through a short-lived staging name and removes any stale `.previous`, and a failed dev install leaves the path absent or partial with a clear error. Nothing ever runs from `.previous` or from a staging copy.

- `install-app` (macOS) renames the replaced app to `<App>.app.previous` (never the Trash) for a release install only; `install-app --dev` removes the old app and keeps nothing. `install-app --installer <Setup.exe> --dest <folder> --exe <App.exe> [--payload <file>]... [--payload-manifest <json>]` runs the NSIS installer in place. It NEVER copies or restores the whole install folder, because that folder can hold user data (CodeRight keeps `coderight.db`, its WAL, state and models there). Its backup and rollback are payload-only, from an explicit allowlist: the `--exe` file plus `--payload` entries and a `--payload-manifest` JSON of relative file paths (files, never directories); with no `--payload` it backs up and rolls back nothing. `right-release dev` (mac) installs to `install.destination` (required) with no `.previous` and fails if its designated requirement differs from the installed copy; Windows tauri dev runs the silent installer without copying `<exe>.previous`; a portable dev payload installs as `<installRoot>/current` with no `current.previous` (data beside `current` is never touched); the Windows direct installer installs to `current` with `current.previous`.
- `right-release dev` (mac) signs with the same Developer ID Application identity as release (an app that needs a provisioning profile uses a Developer ID profile); Apple Development certificates are refused with `DEV_MAC_SIGNING_DEVELOPER_ID_REQUIRED`.
- **Cargo target folders are not stable paths.** The broker's target tree is `<managed>/rightkit-build-control/ws/<hash>/g/<generation>/target/...` and the generation changes; `cargo-guard` uses a hashed `dev-targets/<project>-<hash>` folder. They are build output only, so nothing may run from them. Dev and QA binaries run from a fixed staged path: list them in `development.targets.<platform>.stage` (`[{ source } | { manifestPath, bin, targetTriple?, profile?, name? }]`) and `right-release dev` copies each in place to `<app root>/.rightkit-dev-bin/<triple-or-host>/<name>` (no backup of the old copy); `rightkit-qa` launches from its `.rightkit-qa-bin` stash on every path.

## The door check for an app's own build steps

An app's payload assembler or installer packager is a step its release config declares and `right-release` runs inside a stage. `door.mjs` (0.6.8) gives that step the check `right-release` makes for itself: `requireDoor(command)` exits 2 with the refusal unless a live `rightkit-gate --to <stage>` run for HEAD launched the process, so `node scripts/release/<step>.mjs` by hand is refused instead of becoming a side door around class A. `doorRefusal(options)` returns the message (or `null`) for a step that wants to report rather than exit. Refused: by hand, an ended run, a run for another commit, a forged nonce, outside a git checkout.

```js
import { requireDoor } from "@rightkit/release/door.mjs";
requireDoor("scripts/release/mac-payload.mjs"); // first statement; everything below runs only inside the door
```

## Runtime budget gate (crates)

`publish cargo` runs the `runtime budget` step (`runtime-budgets.mjs`) right after the secret scan. Full contract, schema and refusal table: `docs/RUNTIME-BUDGETS.md`.

What the gate enforces, exactly:
- **Three classes, fail-closed.** always-on (measured budget at publish), `continuous = "held-resource"` with a `resources` table (structural check at publish, installed-app measurement at adoption via `appRuntimeBudgetAdoptionCheck`, wired into `collectPreflight`), and `continuous = false` (bounded per call).
- **Membership is fail-closed.** `Cargo.toml` `[package].name` must equal `--crate`. Every crate needs `runtime-budget.json`, `[package.metadata.rightkit] continuous = false` with a `continuous_reason`, or `continuous = "held-resource"` with a `resources` table; a crate with none of them is refused. The 11 crates in `RUNTIME_BUDGET_CRATES` cannot declare `continuous = false`. A `continuous = false` crate is refused when its non-test `src` matches a pattern in `CONTINUOUS_PATTERNS` (`thread-spawn`, `task-spawn`, `runtime-builder`, `sleep`, `timer`, `listener`, `watcher`, `event-loop`, `audio-stream`, `onnx-session`, `ggml`, `conn-pool`, `resident-static`, `detached-child`) and the reason does not contain that id.
- **A continuous crate needs a measured budget.** Declared ceilings and `requiredScale`; a status pointer to a `pub` item exposing resident memory; per-host measurements of the published version, each backed by raw bench output under `runtime-budget.evidence/` (`runtime-budgets.mjs record`) whose sha256 and `RESULT budget` line the gate re-checks; every observed value within its ceiling and non-zero resident memory; a fresh, in-ceiling, at-scale measurement on each required host (macOS and Windows, or macOS alone only when the release evidence records `hostPolicy: "darwin-only"` with a reason, which the budget file may not contradict); a README section equal to the one generated from the JSON.
- **Freshness** covers the crate's `src`, `benches`, `Cargo.toml`, `build.rs`, the `src`, `Cargo.toml` and `build.rs` of every workspace `path` dependency, new untracked files there, and the crate's `Cargo.lock` dependency closure, against the measured commit (which must be in the checkout and an ancestor of HEAD).

What it does not do: run the bench, prove the bench ran, detect `[profile]`, `[patch]` or toolchain changes, or find continuous work outside the scanned patterns. `runtimeBudgetAdoptionCheck` adds the need for a fresh `installed` measurement naming the app; crates with `publish = false` never reach `publish cargo`, so their adoption must call it.

A real `publish cargo --crate <name>` refuses with `RUNTIME_BUDGET_MISSING` listing every problem; `--dry-run` prints `[FAIL] runtime-budget: ...` and continues.

## QA adoption (mandatory)

RightKit QA is a mandatory component: every Orthic Labs app adopts it. `assertQaAdoption` (`qa-contract.mjs`, next to the native-QA backdoor contract) runs in `release.mjs` beside the package and Cargo contracts on every release, `--dry-run` and `doctor` included, SwiftPM apps without a `package.json` too.

- **Refused:** the app has no QA lane. A lane is the native wdio lane (`right-qa.config.mjs`, validated by `validateQaBackdoorContract`), `@rightkit/qa` in any non-`node_modules` `package.json` (dependencies or devDependencies), or `rightkit-qa` in any `Cargo.toml` (dependencies, dev-dependencies, build-dependencies, `[workspace.dependencies]`; path and workspace declarations count). Message: `RightKit QA is mandatory for every Orthic Labs app, and <app> has no QA lane ... Adopt it: ...`.
- **Warned:** a `@rightkit/qa`, `@rightkit/qa-chrome` or `rightkit-qa` pin behind the latest published version in `rightkit-versions.json` (`[right-release] QA warning: <app> runs RightKit QA behind the latest published version: <pin> -> <latest> (<file>)`). **Refused** when the lag is a breaking bump (`breakingBump` of `rightkit-drift`: a minor bump below 1.0, a major from 1.0). `@rightkit/qa-chrome` is lag-checked but its presence is not enforced.
- **Exempt:** only a repository that is not an app. `qa-adoption-exemptions.json` (`nonAppRepositories`: `{ repositoryIdentity, kind: "library" | "tooling", reason }`, matched against the git `origin`) is RightKit-owned, so a change needs a RightKit release; the RightKit source tree is recognised by `packages/release/package.json` named `@rightkit/release`. There is no per-app opt-out, config key or environment switch.

## Ownership commands

`right-release ownership [--app <dir>] [--json]` scans app copies of shared capabilities; `--staged` scans staged index.
`ownership enroll`, `approve-key` & `revoke-key` manage native signing keys. `ownership trust-sync` advances protected trust
to committed key log through authenticated approval; `trust-sync --check` previews required approval & `trust-compare --json`
reads current digest.

`right-release ownership-mandatory [--workspace <dir>] [--app <name>]... [--amendment v2.1] [--enforce] [--json]`
checks Table 1 owner declarations & minimum versions. Default mode reports findings; `--enforce` applies current mandatory
policy. M21 and M23-M25 (issue tracker `@rightkit/issues-tracker`, end-to-end journeys via `rightkit-qa` / `@rightkit/qa`; both unpublished; M24 applies to every profile, M25 to desktop and apple) remain pending until qualified owner receipts authorize signed policy activation through authenticated
trust-sync. Their `pendingOwners` & M22 `{ role, entrypoints }` applicability records are report metadata; neither changes
enforcement. D4 is ratified; pending activation does not mean pending ratification.

## Adoption evidence (`adoption-evidence`, PK17-6)

`right-release adoption-evidence --manifest <published-release-manifest.json> [--packets <dir>] [--app <dir> [--adoption <file>] [--target <target>]] [--json]` validates the handoff manifest (packet 01 section 6) and, with `--app`, an app's adoption record. Exit 0 no findings, 1 findings, 2 usage error. CHANGELOG and deletion-list prose never satisfy a check; every check reads a hash, a lockfile, a saved `cargo tree` or a JSON receipt.

- **Manifest:** schema; package field rules per event (`publish`, `qualify-existing`); capability `satisfied` recomputed from its packages (`fetchedConsumer.ok`, passing qualification rows, evidence paths); contract rollups; every task `ready` and `readyForAdoption` re-derived (an N/A or unresolved task is never ready, a deferred slice is never satisfied); the selected snapshot hash and the evidence snapshots when the manifest's directory is present; duplicate JSON keys are refused; and the AP id set must equal the ids extracted from the frozen packets `02-*.md`..`07-*.md` (`- [ ] AP-XX-nn` lines) exactly (`ap-missing`, `ap-extra`, `packet-duplicate-task`; no packets is `packets-missing`). `--packets` defaults to the directory above the manifest's directory.
- **Adoption record** (`<app>/rightkit-adoption.json`, schema 1): `app`, `manifestSha256` (the manifest file's hash; a stale record is `adoption-stale`), `target`, optional `locks`, `cargoTree { file, sha256 }` and `tasks.<AP-id>.adopted[] { capability, packages, entrypoints, receipt { file, sha256 } }`. A task must be `ready` in the manifest and every capability it requires must be adopted. Each adopted package must be pinned by the lockfile at exactly the manifest version and integrity from the crates.io registry (`Cargo.lock`), the npm registry (`pnpm-lock.yaml` v9) or the tag commit (`Package.resolved`), with no other version of the name in the lock. For cargo, the saved `cargo tree -e features` must show that exact version, every declared feature active and published, and the declared feature set equal to a passing qualification row for the target's host. The active target must be published and qualified.
- **Receipt** (JSON only, schema 1): `kind` `test` or `run`, `app`, `capability`, one of the adopted `entrypoint`s (a real file inside the app, optional `#symbol` that must appear in it), `packages` naming the pinned `<id>@<version>`, `target`, a `command`, `exitCode: 0`, `finishedAt`, and `lockfileSha256.<cargo|pnpm|swift>` equal to the current lockfile (a receipt from before the last lock change is refused).

## QA-only ad hoc bundle signing (`sign-qa-bundle`)

`right-release sign-qa-bundle --app <abs .app> --qa-root <abs dir> [--entitlements <abs plist>] [--out <evidence.json>]` ad hoc signs an assembled QA bundle (for example an Accessibility QA copy) that lives inside a QA scratch root. It exists because `codesign --force --deep --sign -` sent through the signing agent (SSH or non-Aqua session) is refused by design; those refusals are unchanged and this command never calls the agent.

- macOS only. Refused before any command runs: a relative path, `/Applications` or `~/Applications` (lexical or resolved), a bundle whose real path is not inside the real QA root (links are resolved first), a QA root that is `/`, the home directory or an install location, and a target that is not a `.app` directory.
- Nested code is signed inside-out, deepest first (`Contents/Frameworks/*` frameworks and dylibs, other `*.dylib`, `*.xpc`, nested `*.app`, `Contents/Library/LoginItems`, extra `Contents/MacOS` executables), then the outer bundle, each as `/usr/bin/codesign --force --sign - [--entitlements <reduced>] <item>`. Never `--deep`, never a real identity. `codesign --verify --strict` runs on the bundle last.
- `--entitlements` is reduced like ad hoc development signing (only `com.apple.security.cs.*` and `com.apple.security.device.audio-input` survive) and applies to the outer bundle only.
- The receipt (returned, and written with `--out`) records `signature: "adhoc"`, `releaseArtifact: false`, `qaOnly: true` and the items signed in order. The result is never notarized, sealed, published or installed; use the Developer ID paths for anything that ships.

## Entry points

Bins: `right-release` (`cli/right-release.mjs`) and `rightkit-gate` (`bin/rightkit-gate.mjs`). The CLI is the supported surface.

`package.json` carries an explicit `exports` map (plan item 2.10, C4-16): one `"./<file>"` entry per public file, no `"."` index, no wildcard. It lists the two bin files, `./package.json`, the modules this README documents as SDK (`./release-dependencies.mjs`, `./engine-locator.mjs`, `./pe-content.mjs`, `./dmg-build.mjs`, `./door.mjs`) and the modules that product repositories and RightKit's own tooling import by deep path today. Every other file is private: `import "@rightkit/release/release.mjs"` fails with `ERR_PACKAGE_PATH_NOT_EXPORTED`, and so does `import "@rightkit/release"`. `docs/RELEASE-EXPORTS.md` in the RightKit repository is the inventory (every consumer import with file:line) and `release-exports.test.mjs` fails when the map, that inventory or a checked-out product repository disagree. A new public subpath needs an inventory row or a README SDK section in the same commit; the signing, notarization and packaging modules in the map are a shrinking baseline, not an invitation. A file read by path (`node_modules/@rightkit/release/<file>`) is outside the map and is listed in the inventory too.

## Dependencies

| Dependency | Range | Note |
|---|---|---|
| `@rightkit/outdated` | `0.2.0` (exact) | The updater behind `right-release update` and the stale-pin refusals (plan item 2.10, C4-17). Kept in step with the workspace version of `packages/outdated` by `rightkit-registry-resolver.test.mjs` |
| `smol-toml` | `1.9.1` (exact) | Used to parse lock and manifest files |

Peer dependencies: none.

## Example

Illustrative. Dependency checks, as described in the shared-packages doc:

```sh
pnpm add -D @rightkit/release
pnpm exec right-release deps --check
```

## MSIX target and Swift app type

Swift (SwiftPM) apps are a first-class project type. `projectType: "swiftpm"` (config or target; auto-detected from a `Package.swift` with no Cargo manifest) replaces the cargo/rustc requirement with `swift`, and skips the Rust target-bridge, sccache and Perl checks. Tauri/Rust targets are unchanged. `makeappx` and `signtool` are required only when `targets.win.msix` is configured.

`targets.win` with `signingContract: "windows-msix-signed-v1"` packages an MSIX (schema in `msix-config.mjs`): `prePackage` builds and stages a flat payload (`right-release swift-package windows-stage`), then the msix step runs in the immutable order: verify the declared runtime-DLL inventory and PE import closure (fails closed), sign inner DLLs then EXEs through `sign-windows`, `makeappx pack`, sign the `.msix`, verify the signature and that the signer subject equals `Identity/@Publisher`, write the App Installer route, and write `msix-seal.json`. `msix.virtualization.fileSystemWrite|registryWrite` (`enabled|disabled`) are mandatory and emitted as `desktop6` manifest properties. List the `.msix` under `installer.artifacts` so it seals.

`updater.kind: "appinstaller"` (default remains the Tauri minisign flow) generates `msix.appInstaller.out` (https `uri`/`msixUri`); trust is the MSIX Authenticode signature, so no `.sig` is produced and `publish-update` records the sealed package digest.

Portable macOS targets (`macos-developer-id-notarized-portable-v1`) whose `notarize.file` is an archive of bare executables set `notarize.staple: false`: the archive is notarized (Accepted) and recorded, nothing is stapled. Portable Windows and macOS targets with no installer or updater block seal their archives from `targets.<platform>.artifacts`.

Never cancel an MSIX deployment from app or updater code. Cancelling `AddPackageAsync` while it is in `RegistrationChanged` leaves the package `Modified, NeedsRemediation` (0x80073CF8, AppX event 751); the app then cannot activate, so the OnLaunch feed check normally does not run and the package does not self-heal (in the tuple 08 runs one launch attempt in five was followed by a feed update to the newer version; the trigger was not isolated). Measured recovery (SwiftRight SliceApp R5 W08 tuple 08, 2026-10-10, one run per method, user data intact in every recovering case): Settings › Apps › Advanced options › Repair recovers a feed install (with or without a newer version on the feed) and a path install with no feed; `Add-AppxPackage -Path <same-version .msix>` recovers; `Add-AppxPackage -AppInstallerFile <feed uri>` recovers with or without a newer version on the feed. `Reset-AppxPackage`, `-RegisterByFamilyName`, the platform's own `RegisterByPackageFullName` (runs on every launch attempt, reports success, package stays NeedsRemediation) and relaunching do not. Not measured: Repair with the install source gone.

macOS Swift apps use `right-release swift-package mac-app` (Info.plist from config, resources, frameworks, entitlements on the bundle only; nested code signed inside-out before the app).

Build a SwiftPM app with `right-release build --platform mac|win` (add `--config <path>` when the config is not `right-release.config.mjs`). Build is tier-neutral and never uploads, so it rejects `--tier` and takes no `--upload`/`--no-upload`; the old `release --tier patch --no-upload` form no longer works. Pick the tier only at upload: `right-release upload --release <sealed-id> --platform mac|win --tier patch|update` (`--tier` is required; it never builds). `upload` (and `publish`, its alias) requires `distribution.provider: "github-releases"` and `distribution.repository` in the release config; a product distributed through R2 and RightApps publishes with `right-release publish-update --release <sealed-id> --platform mac|win --tier patch|update` (or `RIGHT_RELEASE_TIER`), which requires `distribution.provider: "r2-rightapps"`.

Set `targets.mac.notarize.appBeforePackage: true` (with `notarize.app` naming the `.app`) to notarize and staple the app itself right after signing and before the package step copies it into the DMG, so an app copied out of the DMG passes `stapler validate` offline. The DMG named by `notarize.file` is still notarized and stapled afterward; the app ticket receipt is `macos-app-notarization.json`. Without the flag, behavior is unchanged.

For `projectType: "swiftpm"` on mac with `appBeforePackage: true` and no `targets.mac.package`, right-release builds the DMG itself (same `dmg-build.mjs` as Tauri) at the package step, after the `.app` is signed, notarized and stapled: it runs `xcrun stapler validate` on the app (refusing to pack an unstapled one), builds the DMG at `notarize.file` (must end `.dmg`), Developer ID signs it (receipt `macos-dmg-signing.json`), then notarizes and staples the DMG as before. Options go in `targets.mac.dmg`: `{ volname, background, iconSize, appPosition, applicationsPosition, window, volumeIcon, out }` (`volname` defaults to the app name). A prePackage hook must no longer build the DMG: the config is refused (at build time and by `doctor`) when its command or arguments mention `dmg` or `hdiutil` (heuristic), because that DMG would carry the unstapled app. Keep a custom DMG flow with `targets.mac.dmg: false` or a `targets.mac.package` step (both run after the staple and skip the automatic build).

`sign.prePackageFiles` entries may be `.app` bundles (directories). Nested code (Mach-O files, frameworks, XPC services, plug-ins) is signed first, inside-out and without entitlements, then the bundle is signed; the receipt records a tree digest of the bundle. Any other directory is rejected with a clear error.

### Per-file signing matrix (macOS)

By default every code item is signed with the hardened runtime and no entitlements, so a bundle that needs entitlements on some of its code (an app, a sandboxed extension) declares them per file. Both forms below are optional and backwards compatible; a config of plain path strings signs exactly as before.

- `sign.prePackageFiles[]` entries are a path string or `{ file, entitlements?, hardenedRuntime?, options?, identifier? }`. `identifier` is the codesign `--identifier` (letters, digits, `.`, `-`): set it on loose tools (for example `legion`, `legion-hook`, `legion-mcp`) so each keeps one stable code identity across builds; it is carried through the plan and signing manifest and checked against the receipt. `entitlements` is a plist path relative to the release config directory. For a `.app` entry the settings apply to the bundle signature.
- `sign.nestedCode` is a map from a path inside the bundle (`Contents/...`, forward slashes) to `{ entitlements?, hardenedRuntime?, options?, identifier? }`. It needs exactly one `.app` entry in `sign.prePackageFiles` to resolve against. Nested code the map does not name keeps the defaults.
- `options` are extra codesign `--options` names (`expires`, `hard`, `host`, `kill`, `library`; `runtime` is always on). `hardenedRuntime` may be `true` or omitted: `false` is a config error because a notarized target cannot ship code without it.

Signing order stays inside-out: all nested code, deepest path first (a nested bundle after everything inside it), then the `sign.prePackageFiles` entries in declared order. A `nestedCode` path is signed once, with its own settings, in that sequence; it must exist, must not be a symlink or leave the bundle, and must not be the bundle's main executable (that is signed with the bundle). Every configuration problem found is reported at once, before anything is signed.

`right-release doctor` validates the matrix (unknown keys, malformed paths, unsupported option names, entitlement files that are missing, outside the config directory or not property lists); `right-release build --dry-run` only loads the config and does not. Whether a `nestedCode` path exists inside the built bundle is checked at sign time, before the first codesign call. The receipt records, per signed item, the entitlements file digest and the `--options` value, and the build refuses a receipt that disagrees with the plan. Steps that carry settings reach `sign-macos.mjs` as a JSON manifest (`--manifest`, written beside the receipt); steps with only defaults keep the positional form.

Worked example, the Pulse bundle (hardened-runtime helpers with no entitlements, a sandboxed Finder Sync extension with its own entitlements, the app with the app entitlements; replaces `@electron/osx-sign` `optionsForFile`):

```js
sign: {
  prePackageFiles: [
    { file: "dist/staging/Pulse.app", entitlements: "release/entitlements.plist", hardenedRuntime: true },
  ],
  nestedCode: {
    "Contents/Helpers/PulseHelper":   { hardenedRuntime: true },
    "Contents/Helpers/pulse-elevate": { hardenedRuntime: true },
    "Contents/Helpers/smartctl":      { hardenedRuntime: true },
    "Contents/PlugIns/PulseFinder.appex": { hardenedRuntime: true, entitlements: "mac/Notch/FinderExtension/PulseFinder.entitlements" },
  },
},
```

Nested code that the map does not name (frameworks, other helpers) is signed first with the defaults; the appex's own `Contents/MacOS` executable is signed before the appex, and the appex signature then carries the extension entitlements. The Finder extension's entitlements never reach the app and the app's Apple Events entitlement never reaches the extension.

### Signing the built DMG (`dmg build --sign`)

`right-release dmg build ... --sign [--identifier <id>] [--receipt <path>]` Developer ID signs the finished image with the shared Developer ID Application identity (secure timestamp, no hardened runtime, as for any disk image; optional stable codesign `--identifier`; strict verification and identity/timestamp readback), so an app whose `package` step builds its own DMG needs no direct `sign-macos.mjs` call before `notarize`. `--receipt` writes the same signing receipt `sign-macos.mjs` writes. A signing failure removes the unsigned image (`DMG_SIGN_FAILED`). It never notarizes or uploads; there is no ad hoc or Apple Development fallback. `buildDmg({ ..., sign: true, identifier, signReceipt })` is the SDK form. `targets.mac.macPackage` and SwiftPM apps already sign the DMG themselves.

### DMG from an appdmg-style spec

`right-release dmg build --app <.app> --out <dmg> --spec <appdmg.json>` (and `targets.mac.dmg.spec` for SwiftPM apps that right-release packs itself) renders an appdmg-style JSON file through the same headless hdiutil pipeline; explicit flags and `targets.mac.dmg` options win over the spec, and the spec's `title` is the volume name (on the command line, `--volname` is required when the spec has no `title`). Relative `background` and `icon` paths resolve against the spec file's directory. Pulse's `release/appdmg.json` works unchanged:

```json
{ "title": "Pulse", "icon-size": 128, "window": { "size": { "width": 640, "height": 420 } },
  "contents": [ { "x": 180, "y": 210, "type": "file", "path": "Pulse.app" },
                { "x": 460, "y": 210, "type": "link", "path": "/Applications" } ] }
```

Supported: `title`, `icon` (`.icns` volume icon), `background` (PNG or TIFF), `icon-size`, `window.size`, `window.position`, `format: "UDZO"`, `filesystem: "HFS+"`, and `contents` limited to the `.app` (`file`, its name must match `--app`) and a `link` to `/Applications`, where `x`/`y` are icon centres. Unsupported fields (`background-color`, `code-sign`, `license`, other formats or filesystems, extra files or links, `position` entries, anything unknown) fail with `DMG_SPEC_UNSUPPORTED` instead of being ignored. `code-sign` is refused because the release pipeline signs the DMG in its own step (receipt `macos-dmg-signing.json`).

## One gate for GitHub uploads

`right-release upload` and `right-release github --release` share one gate (`github-upload-gate.mjs`): both require `--tier patch|update`, the primary checkout with a clean tree, a sealed release built from the current HEAD with matching ownership evidence, and a release config that declares `distribution.provider: "github-releases"` and `distribution.repository` (`--repo` may only repeat it; the `origin` remote is never consulted). The gate makes no network call and also runs on `--dry-run`. Private products publish with `publish-update` and `distribution.provider: "r2-rightapps"`. `github --addon` publishes headless add-ons and takes no tier. Not gated the same way: `github --addon` (no tier), `addon upload` (no provider requirement), and the direct R2 uploaders `upload-large.mjs` and `pub-upload.mjs` (no tier, provider or clean-tree check).

## GitHub Releases updater lane (`publish-update`, `github-releases`)

A public app with an in-app Tauri updater publishes with `right-release publish-update --release <sealed-id> --platform win|mac --tier update` and `distribution: { provider: "github-releases", repository: "<owner>/<repo>", updaterKeyId?: "29395F9FF466261D" }`. It is the r2-rightapps lane with a different destination, not a second signer: the updater bundles are signed by `sign-updater.mjs` with the Right Suite updater key (the one every suite app uses; no per-app key or trust root, see "Updater trust roots" below), sealed with their `.sig` files, and re-verified by `verifyPublicationRelease` before anything is uploaded. The lane then:

- builds the same `platforms` map the R2 lane builds (`{ <target>: { signature, url } }`), with each `url` the GitHub asset URL `https://github.com/<owner>/<repo>/releases/download/v<version>/<asset>`;
- uploads each sealed bundle and `.sig` to release `v<version>` (created when missing, never a draft), byte-verifying every asset by download, and refusing, not overwriting, a remote asset whose bytes differ;
- writes a Tauri v2 static `latest.json` (`version`, `notes`, `pub_date`, `platforms`) and uploads it last, merged with the other platform's entries already on that release (the win and mac runs share one `latest.json`; a different version on the release is refused), so `https://github.com/<owner>/<repo>/releases/latest/download/latest.json` resolves.

A sealed signature made by any key other than the resolved `updaterKeyId` refuses the publish (`PUBLICATION_SIGNATURE_KEY_MISMATCH`); a Right Suite app resolves it from the contract even when the config names none (below). Notes come from `RIGHT_RELEASE_NOTES`. `--dry-run` prints the upload plan and the manifest with no network call. Uploads use `gh` (already authenticated); no RightApps token is read.

Updater artifacts are declared as for R2 but need no `key`; `platform` must be a Tauri updater target (`windows-x86_64`, `windows-aarch64`, `darwin-aarch64`, `darwin-x86_64`, `linux-x86_64`, optionally with an installer suffix such as `-nsis`, `-msi`, `-app`, `-appimage`) and file names must match `[A-Za-z0-9._+-]` so GitHub does not rename them:

```js
targets: {
  win: {
    updater: { artifacts: [
      { file: "src-tauri/target/release/bundle/nsis/Ember_1.2.3_x64-setup.exe", signature: "src-tauri/target/release/bundle/nsis/Ember_1.2.3_x64-setup.exe.sig", platform: "windows-x86_64" },
    ] },
    artifacts: ["dist/Ember-1.2.3-portable-win.zip"], // portable zips seal alongside; they are not in latest.json
  },
  mac: { updater: { artifacts: [
    { file: "src-tauri/target/release/bundle/macos/Ember.app.tar.gz", signature: "src-tauri/target/release/bundle/macos/Ember.app.tar.gz.sig", platform: "darwin-aarch64" },
  ] } },
}
```

Updater signing keys are never named in the config: `sign-updater` reads the minisign key from the OS credential provider (`rightkit-credentials`, namespace `rightsuite-updater-key`; see `protected-credentials.mjs`). `targets.<platform>.updater.keyFile` is refused at config validation (`RELEASE_CONFIG_UPDATER_KEY_FILE`, listed by `right-release doctor` and the build with every other finding) and is never forwarded to the signer, so remove it from apps that still set it.

### Updater trust roots (Right Suite apps)

A Right Suite app (`coderight`, `cutright`, `genright`, `heardright`, `mailright`, `postright`, `pulse`, `scraperight`, `viewright`) takes its updater trust from `contracts/right-suite-api.v1.json` (`updaterPublicKeys`, `updaterSigningKeyId`), never from its release config:

- Do not declare `distribution.updaterPublicKey` (or `updaterPubkey`, `targets.<platform>.updater.pubkey`, `publicKey`). The seal (`build`) and `publish-update` verify every `.sig` against the contract's key text, so a suite app publishes with no key declaration. A declared key that is not exactly a contract key text refuses with `PUBLICATION_SIGNATURE_KEY_MISMATCH` (`suite apps do not declare their own`), and the config check reports it as `RELEASE_CONFIG_UPDATER_PUBLIC_KEY`.
- `distribution.updaterKeyId` defaults to the contract's `updaterSigningKeyId` and is always enforced: the signature's key id must equal it, at seal and at publish. It must be a key the contract lists whose `notAfter` has not passed; any other id refuses with `PUBLICATION_SIGNATURE_KEY_MISMATCH` (the config check reports a malformed or unlisted id as `RELEASE_CONFIG_UPDATER_KEY_ID`). Name it only to publish with a listed key other than the signing key.
- An app outside the suite (Ember has its own updater key) keeps declaring `distribution.updaterPublicKey`; without one a sealed publish or seal fails closed (`PUBLICATION_SIGNATURE_UNVERIFIABLE`). Its `updaterKeyId` stays optional, and when set it is pinned at seal as well as at publish.
- `sign-updater --key-id <16 upper-case hex digits>` reads each produced `.sig` back and refuses a signature made by any other key, naming the file, the expected id and the found id, so a credential store holding the wrong key fails at signing rather than at seal. `--dry-run` prints the expected id and signs nothing. Without `--key-id` the signature is not read back.

Rotation order: (1) add the new key to `updaterPublicKeys` in the contract and regenerate the projections; (2) ship a release signed by the current key (`updaterSigningKeyId` unchanged), so the installed apps now know both keys; (3) change `updaterSigningKeyId` to the new key id and sign with the new key (`sign-updater --key-id <new id>`). Give the old key a `notAfter` to stop accepting it. The full runbook is `docs/SUITE-TRUST-ROOTS.md`.

NSIS, MSI, `.app.tar.gz` and AppImage bundles all work the same way (any file signed by `sign-updater`). With no sealed installer the installer platform-trust check is skipped (reported in the output); installer-bearing seals still run it. The app side: `plugins.updater.endpoints` is the `latest.json` URL above and `plugins.updater.pubkey` is the suite updater public key (see `@rightkit/app-update` README).

## Android (AAB build, Play internal track, dev install)

The Android lane is native Kotlin with Gradle plus Rust `.so` libraries per ABI (`docs/design/android-minimum.md` section 4). It follows the same split as the desktop lanes: `build` never uploads, `upload` never builds.

| Step | Command | Config |
|---|---|---|
| Build | `right-release build --platform android` | `targets.android`: `gradle` (`bundle<Variant>Release` only), optional `ndk`, `aab`, `artifacts`, `signing` (see `android-release.mjs`) |
| Upload | `right-release upload --platform android --release <sealed-id> [--apply]` | top-level `distribution: { provider: "google-play", packageName, track: "internal", releaseStatus?, releaseName?, serviceAccount }` |
| Dev | `right-release dev --platform android --serial <adb serial>` | `development.targets.android`: `ndk?`, `build` (debug tasks), `install: { kind: "adb", apk, package, activity? }` |

- Build: `targets.android.ndk = { abis?: ["arm64-v8a"], jniLibs: "android/app/src/main/jniLibs", manifestPath?, package?, features?, apiLevel? }` runs `cargo ndk -t <abi> ... -o <jniLibs> build --release` before Gradle. Release defaults to `arm64-v8a`; add `x86_64` only for an emulator-installable bundle. `jniLibs` is generated and emptied before each real build. Gradle then runs `bundleRelease` with the upload keystore passwords handed over only as the path of a transient 0600 properties file. The AAB is scanned and must be signed by the pinned upload certificate.
- Upload: dry run is the default; `--apply` performs `edits.insert`, `bundles.upload`, `tracks.update` (internal) and `edits.commit`. A debug-signed, unsigned or unpinned bundle is refused. Only the internal track and a non-test package can be named. There is no `--tier`.
- Secrets: the keystore passwords and the Play service-account JSON live in the OS credential store (macOS Keychain `rightkit-android-<name>`; a protected token file on Windows and Linux, so store the JSON as one line) or in a 0600 file outside the repository, selected by `{ credential: "<name>" }` or `{ file: "<path>" }`. They never appear in the environment, on argv, in config, or in a log; key material found in the environment refuses the command.
- Dev: a debug build installed by serial (never "the only device") with a package ending in `.dev`, `.qa`, `.test` or `.debug`; release tasks and signing properties are refused.

## iOS (archive, export, TestFlight, simulator and device install)

One declarative recipe for every iOS app (bogusyogi/rightkit#349); full reference and example config in `docs/IOS-PACKAGE-RECIPE.md`. Same split as Android: `build` never uploads, `upload` never builds, and the upload is a dry run unless `--apply`.

| Step | Command | Config |
|---|---|---|
| Build | `right-release build --platform ios` | `targets.ios`: `iosPackage` (`project`, `projectSpec?`, `scheme`, `teamId`, `bundleId`, `archive`, `ipa`, `extraBuildSettings?`, `prepare?`, `cleanup?`, `checks?`) and `artifacts` listing the IPA |
| Upload | `right-release upload --platform ios --release <sealed-id> [--apply]` | top-level `distribution: { provider: "app-store-connect", appId, bundleId, track: "testflight", groups (required), submitExternalBetaReview? (default false), credentials? }` |
| Credentials, once | `right-release ios-credentials store --key-id <KEYID> --issuer <uuid>` (`.p8` on stdin), `ios-credentials check` | Keychain service names, optional |
| Dev | `right-release dev --platform ios [--serial <device id>]` | `development.targets.ios`: `project`, `projectSpec?`, `scheme`, `teamId?`, `install: { kind: "simulator" \| "device", device, bundleId, ... }` |

- Build: `xcodegen generate` (required when `projectSpec` is named; a committed xcodeproj is never a fallback), `xcodebuild archive` with automatic signing, `-allowProvisioningUpdates`, the configured team and a date build number `YYYYMMDDNN` (the marketing version stays as the app sets it), then the archive checks (one version across the app, appex and Watch plists, QA/release-switch scan, forbidden strings), `xcodebuild -exportArchive`, the IPA checks (entitlements, distribution profile, code signature) and a receipt over the final IPA bytes. A failure deletes the archive, the IPA and the receipt. macOS only, in the GUI signing session; on Windows and Linux only config validation and `--dry-run` run.
- Credentials (D4): the App Store Connect `.p8`, key id and issuer id are read from the macOS login Keychain (`security find-generic-password -s <service> -a <account> -w`; defaults `rightkit-asc-api-key`, `rightkit-asc-key-id`, `rightkit-asc-issuer-id`). A key path, key id, issuer or key in config, and key material in the environment, are refused; the ASC environment variables are refused on managed hosts. Key material never reaches a log.
- Upload: the sealed IPA is uploaded in process through the App Store Connect build-upload resources (`buildUploads`, `buildUploadFiles`, pre-signed part uploads, commit, poll to `COMPLETE`), then the exact build is assigned to the TestFlight beta groups (in-process ES256 JWT). Idempotent calls are retried with bounded backoff (the create POSTs never), an interrupted upload for the same version and build is resumed or finished, pre-signed URLs are limited to `*.blobstore.apple.com` and may not carry credential headers, and the IPA is streamed in ranges. A per-release lock, a GUI-session check before the Keychain read and marker identity checks guard a rerun. No altool and no file ever holds the key. No App Store submission. Apps must git-ignore `.right-release/` (see `docs/IOS-PACKAGE-RECIPE.md`).
- Dev: simulator (`simctl boot/install/launch`) or physical device (`devicectl install` and `process launch`), chosen by id, built Debug with generated commands.


## Bundled FFmpeg decode sidecar (Ember and other media apps)

An app that decodes video through `rightkit-ffmpeg` ships the verified LGPL `ffmpeg` and `ffprobe` programs as a Tauri `externalBin` sidecar. right-release adds no new bundling mechanism: Tauri bundles the files (mac `Contents/MacOS`, Windows next to the app EXE), and right-release plans them from the catalog the app compiled against, ships the LGPL text and source offer, signs the binaries with the app, and seals their hashes. Set `targets.mac.mediaSidecar` and `targets.win.mediaSidecar`:

```js
mediaSidecar: {
  arch: "aarch64",                        // catalog arch, one per build (mac aarch64 | x86_64; win x86_64). Default: the build host.
  variant: "decode",                      // catalog variant (default "decode")
  tools: ["ffmpeg", "ffprobe"],           // default both
  stageDir: "src-tauri/binaries",         // where Tauri finds binaries/<tool>-<triple>[.exe] (default)
  bundleDir: "Contents/MacOS",            // mac only; "Contents/Resources" if the app looks there instead
  notices: { dir: "src-tauri/resources/third-party/ffmpeg", sourceOfferContact: "https://example.com/ffmpeg-source" },
  stage: { cmd: "<runner>", args: ["run", "--bin", "stage-ffmpeg"] },   // optional: your call to rightkit_ffmpeg::stage_external_bin
  catalog: "path/to/artifacts.json",      // optional; default: the rightkit-ffmpeg crate the app's Cargo graph resolves (metadata, offline)
}
```

The app still declares `"externalBin": ["binaries/ffmpeg", "binaries/ffprobe"]` and lists `notices.dir` under Tauri `bundle.resources`; `LIGHTCRAFT_FFMPEG_DIR` and the other lookup places are the app's own. Order of a build:

1. Plan: one approved catalog row per tool for the variant, OS and arch. A missing, pending or stale row, a row without a content digest, or an ambiguous match refuses the release (`MEDIA_SIDECAR_NO_ROW`, `MEDIA_SIDECAR_NOT_APPROVED`, `MEDIA_SIDECAR_AMBIGUOUS_ROW`).
2. Before the app is built: the row's notice files (the LGPL text and build notes) are copied into `notices.dir` together with a generated `FFMPEG-SOURCE-OFFER.txt` (licence, exact FFmpeg source URL and archive SHA-256, build recipes, component notices, `sourceOfferContact`); then `stage` runs if configured. Staging must use `stage_external_bin`, which checks the catalog content digest (signature-independent) as it copies. right-release does not recompute that digest; it checks that each `<tool>-<triple>[.exe]` exists, is a non-empty file, and records its hash.
3. Signing: macOS adds `Contents/MacOS/ffmpeg` and `ffprobe` to `sign.nestedCode` (hardened runtime, no entitlements, signed inside-out with the bundle and notarized with the app; the nested signing receipt must show each strict-valid and the app must contain `FFMPEG-SOURCE-OFFER.txt`). Windows signs the staged `.exe` files through `sign-windows` in the same raw-EXE step as the app EXE, before Tauri packs them into the installer, so they land in the `raw-exe` receipt.
4. Receipt and seal: `media-sidecar.json` (state root) records the catalog row ids and content digests, the pre-sign and signed SHA-256 and size of each binary, the notice digests and the signing mode. `right-release build` refuses to seal a release that configures `mediaSidecar` without it, and on Windows checks each signed digest against the sealed raw-EXE receipt. The sealed `release-manifest.json` carries it as `mediaSidecars`, and publication re-validates its shape.

Linux has catalog and planner support only: the planner maps `linux` to the `*-unknown-linux-gnu` triples and works as soon as the crate catalog carries approved Linux rows, but there is no Linux release target in right-release yet, and the catalog has no Linux rows (they need a Linux build and the crate's identity check; see the rightkit-ffmpeg README). Until then an app ships without video on Linux. Tests (fakes only, no catalog download, binary, signer or notary): `media-sidecar.test.mjs`.

## Generic Tauri externalBin sidecars (any app)

Any app that ships more than the ffmpeg sidecar (MailRight: `mailright-engine`, `mailright-core`) sets `targets.mac.externalBin` / `targets.win.externalBin`: `{ names: [...], arch?, stageDir? ("src-tauri/binaries"), bundleDir? (mac: "Contents/MacOS" | "Contents/Resources"), stage?: { cmd, args? } }`. It is the generic form of the ffmpeg staging above and reuses its target-triple naming (`<name>-<triple>[.exe]`), staged-file checks and signing evidence checks (`external-bin.mjs` over `media-sidecar.mjs`); it has no catalog, notices or manifest seal. `stage` is the app's own build of the files. Windows signs the staged files in the raw-EXE step; macOS signs `<bundleDir>/<name>` as nested code (hardened runtime) and checks the nested receipt. `external-bin.json` (state root) records pre-sign and signed digests. Names that `mediaSidecar` stages (`ffmpeg`, `ffprobe`) are refused here. Tests: `external-bin.test.mjs`.

## Sidecar staging: source builds, prebuilt checks, archives, mixed variants, universal macOS

`stage-sidecar` (the `sidecars` list in `right-release.config.mjs`, receipt `stage-sidecar.json`) covers these cases without an app script. Every download is https and digest-pinned, nothing runs through a shell, and a failed check stages nothing.

- **`cmake` (Orthic-Labs/rightkit#489):** `{ kind: "cmake", name: "whisper-cli", repo: "https://github.com/ggml-org/whisper.cpp", commit: "<40 hex>", target?: "whisper-cli", output?: "bin/whisper-cli", cmakeArgs?: ["-DBUILD_SHARED_LIBS=OFF"], platformArgs?: { mac: ["-DGGML_METAL=ON"], win: [...] }, generator?, smoke?: ["--help"], sha256?, dest?, clean?, platforms? }`. It fetches exactly that commit into the fixed `.right-release/cmake/<name>/src` (`git init`, `fetch --depth 1 origin <commit>`, detach; a checkout already at the commit is reused, one with local edits or at another commit is refused with `STAGE_SIDECAR_SOURCE_MISMATCH`), configures and builds `target` in `.right-release/cmake/<name>/build` (`CMAKE_OSX_ARCHITECTURES` is set from the target; `universal-apple-darwin` builds `arm64;x86_64` and checks `lipo -archs`), finds the executable (`output`, then `Release/` layouts, `.exe` on Windows), optionally pins its sha256, stages it as `<name>-<triple>[.exe]` and smoke-runs it when the target can run on this host. `cmakeArgs` and `platformArgs` accept only `-DNAME=value` definitions. Toolchain controls (#691, all optional, defaults unchanged): `cmakeBin: "<name or path>"` runs that cmake (a bare name is looked up on PATH, a path with separators is resolved against the app root); `generator` is per entry and `platformGenerators: { win: "Visual Studio 17 2022" }` overrides it for one platform (`-G`); `architecture: "x64"` (any `-A` platform: `x64`, `Win32`, `ARM64`) adds `-A <arch>` after `-G`. With no `cmakeBin`, a Windows host uses `cmake` from PATH (`where cmake`) when there is one, else the Visual Studio-bundled cmake (`vswhere.exe` under `%ProgramFiles(x86)%\Microsoft Visual Studio\Installer`, `-latest -products * -requires Microsoft.VisualStudio.Component.VC.CMake.Project -property installationPath`, then `Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe`), else plain `cmake`. The receipt row carries `cmakeBin` (when not plain `cmake`) and `generator`. There is no environment-variable override: the staging recipe has none for any other tool.
- **`prebuilt` (#490):** verifies a host-built binary already staged at `<stageDir>/<name>-<triple>` (or copied from `from`) against an app manifest, never building or downloading: `manifest: { file, entry: "binaries.{platform}", sha256?: "sha256", approvalSha256?: "approval_sha256", approvalAlgorithm?: "approval_hash_algorithm" }` (`{platform}` is `macos-aarch64`, `windows-x86_64`, ...). The raw sha256 must match; when an approval hash is named it must match too, with `raw-sha256` or `macho-codesign-strip-linkedit-v1` (`macho-approval-hash.mjs`: the sha256 of a thin Mach-O with its code signature removed and `__LINKEDIT` normalised, so a signed and an unsigned copy of one build agree; macOS only, `codesign --remove-signature` on a private copy). A missing file, entry or digest fails closed. With `from: { aarch64: "<path>", x86_64: "<path>" }` the entry also works on `universal-apple-darwin` (#692): each arch is verified against its own manifest entry (`macos-aarch64`, `macos-x86_64`; the approval algorithm default is the macOS one), then both are lipo-merged by the same helper `ffmpeg`, `onnxruntime` and `cmake` use into `<name>-universal-apple-darwin` (both archs required, `STAGE_SIDECAR_NOT_UNIVERSAL` otherwise; nothing is merged after a failed slice). With `from` omitted the inputs are the staged `<name>-aarch64-apple-darwin` and `<name>-x86_64-apple-darwin`; a plain string `from` is refused for universal. On a thin target an object `from` picks the entry for that arch. The row carries `platform: "macos-universal"` and per-arch `slices`.
- **`remote` archives (#491):** `extract: "<member file name>"` (with `memberSha256?`, `executable?`) unpacks one member of a zip or tgz after the archive digest passes, so a Windows zip needs no `command` entry. A bare name must be the base name of exactly one member; a name with `/` (`bin/x64-win/DirectML.dll`) must equal exactly one member path (case-sensitive), for archives that repeat a base name in several directories. Absolute, drive-prefixed, backslash, empty, `.`/`..` segment and glob-character paths are refused, and the output stays at `dest`.
- **`ffmpeg` mixed variants, fetch and probes (#491):** `variants: { ffmpeg: "export-ass" }` lets one entry take the export-ass ffmpeg with the decode ffprobe (a tool not named uses `variant`). `sources: { ffmpeg: { url, sha256, member }, ffprobe: {...} }` downloads the bytes the catalog only pins: the archive is digest-checked, the member is unpacked and its sha256 must equal the APPROVED catalog row's content digest, so a swapped archive can never stage. A source may instead be a local path (#692): `sources: { ffmpeg: { path: "../vendor/ffmpeg-lgpl", member?: "ffmpeg" } }` (also per arch: `{ aarch64: { path }, x86_64: { path } }`). `path` is relative to the app root and may step out of it; it names the binary itself or a directory holding `<tool>[.exe]` or `bin/<tool>[.exe]` (`member` renames the file). Nothing is downloaded and there is no archive digest, but the file must still equal the APPROVED catalog row content digest (`STAGE_SIDECAR_HASH_MISMATCH` otherwise); notices, probe and universal lipo are unchanged; a missing file is `STAGE_SIDECAR_BINARY_NOT_FOUND`. `probe: true` runs `-version` and `-encoders` on a staged binary this host can run: the configuration must say `--disable-gpl` and `--disable-nonfree` and must not enable GPL, nonfree, libx264, libx265, libxvid or libfdk-aac, and ffmpeg may not list those encoders (`STAGE_SIDECAR_FFMPEG_PROBE_FAILED`; another architecture or OS is recorded as `skipped-cross-target`). The catalog has no Windows `export-ass` row yet (only `export-ass-zimg`); that row belongs to the rightkit-ffmpeg crate.
- **`native-library` (plan item 2.8):** `{ kind: "native-library", library: "onnxruntime" | "directml" | "pdfium" | "whisper-cpp" | "ggml" | "sherpa-onnx", version, catalog, resourceDir?, licenseDir?, cacheDir?, as?, files?, companions?, source?: { dir }, platforms? }` stages the `approved` or `pinned` rows of `native-libraries.json` for the target (every serving row, or the `files` named) into `resourceDir`, with their notices and a receipt row per file. The bytes come from the row's digest-pinned archive or, for a row with no archive (the macOS whisper.cpp, ggml and sherpa-onnx builds), from `source: { dir }`; an archive file must match the whole-file sha256 and the content digest, a `source.dir` file the content digest (a re-signed copy passes, changed code does not, and a thin row refuses a universal Mach-O), and the staged bytes are checked again after the copy. A row's `companions` (the Windows onnxruntime row names DirectML) are staged beside it with a `companionOf` receipt row; `companions: false` opts out, and a file another entry also writes into `resourceDir` is refused before anything is written. A row with `hostProof: "never-run"` stages with a warning (stderr on the CLI) and carries the marker into the receipt. `kind: "onnxruntime"` stays the alias of `native-library` with `library: "onnxruntime"`. See `docs/STAGE-SIDECAR.md`.
- **`ffmpeg` `resourceDir`:** `{ kind: "ffmpeg", resourceDir: "src-tauri/resources/runtime", ... }` stages `<resourceDir>/ffmpeg[.exe]` and `ffprobe[.exe]` (no target triple, not an externalBin; an already-staged file that matches its row is made executable) from the same approved rows, notices, `sources`, probe and universal lipo merge as the externalBin layout; `bundleDir` and `resourceDir` cannot be combined.
- **`universal-apple-darwin` for `ffmpeg`, `onnxruntime` and `native-library` (#492):** each arch row is staged thin under `.right-release/slices/<triple>/` and the files are lipo-merged into `<stageDir>/<tool>-universal-apple-darwin` (onnxruntime, native-library and an ffmpeg `resourceDir` entry: into `resourceDir`), requiring both archs and every row approved (onnxruntime and native-library also accept `pinned`). A macOS `universal` native-library row is staged once and checked with `lipo -archs`, with no merge. For `ffmpeg`, `{arch}`, `{triple}` and `{stageDir}` in the `stage` command's arguments name the slice (it runs once per slice), `sources` need the per-arch form `{ ffmpeg: { aarch64: {...}, x86_64: {...} } }`, one source offer lists both slices, and the receipt rows carry `slices`. A merged file identical to the staged one is not rewritten.

## Declared Rust audit, dist-scan and the Windows Swift toolchain

- `right-release deps --audit` also runs `cargo audit` in every folder named by `deps.cargoWorkdirs` (app-relative, each must hold a `Cargo.lock`), through the managed cargo client (the RightKit broker on managed hosts; there is no bare-cargo fallback on a managed Mac), and fails when any workdir fails or cannot start. An app with no `deps.cargoWorkdirs` audits no Rust. Orthic-Labs/rightkit#674.
- `right-release dist-scan --dist <dir> --forbid <string>... [--forbid-file <path>] [--forbid-name <regex>]... [--ext js,css,...]` fails a built web `dist/` that carries a forbidden marker string in a text file, or a chunk named `devSource-*`, `mockDataSource-*`, `fixtures-*` or matching `--forbid-name` (exit 1; exit 2 on a usage error, a missing `dist/` or a run with no markers). It replaces per-app copies of a release-bundle checker. Orthic-Labs/rightkit#484.
- `right-release toolchain swift --version 6.4.0 --sha256 <pin> [--github-env] [--json]` (Windows) downloads the swift.org installer, refuses unless its sha256 equals the pin (before anything runs; a bad download is deleted), skips the install when that Swift version is already on PATH, otherwise runs it silently (`/quiet /norestart`, exit 0 and 3010 pass), checks `swift --version`, finds the runtime bin (`Runtimes\<ver>\usr\bin` with `swiftCore.dll`) and, with `--github-env`, appends only the variables the installer added or changed (names printed, values never) to `GITHUB_ENV`, and the new PATH entries, the swift directory and the runtime bin to `GITHUB_PATH`. Orthic-Labs/rightkit#436 (the `@rightkit/ci` `swift-toolchain` step calls this and lives in the ci package).

## Keeping agent writes off the internal drive (`disk agent-env`, `disk probe`)

No agent may write to the internal drive (C: on Windows, the internal disk on macOS); only installed apps go there.

`right-release disk agent-env [--root <dir>] [--format sh|ps1|cmd|json] [--check] [--include-agent-homes]` prints the environment that sends agent and child-tool writes to the external root (default `D:\` on Windows, `/Volumes/D` on macOS; default format `ps1` on Windows, `sh` on macOS). It fails closed: if the root is missing, not writable, on the system drive, or (macOS) not under `/Volumes`, it prints nothing on stdout and exits 1. It never falls back to the internal drive. It creates the target folders under the root. `CARGO_TARGET_DIR` is deliberately not set because RustRight owns cargo targets. `CODEX_HOME` and `CLAUDE_CONFIG_DIR` appear only with `--include-agent-homes`, because moving them requires restarting every chat. `--check` compares the current process environment with the map and lists each variable that is on the internal drive or unset; exit 1 when any is. The paths are the ones RustRight applied to the user environment, in one exported table (`AGENT_ENV_TABLE`, imported by the RustRight broker), relative to the root and the same on both platforms:

| Variables | Path under the root |
| --- | --- |
| `TEMP`, `TMP`, `TMPDIR`, `CLAUDE_CODE_TMPDIR` | `agent-cache/tmp` |
| `XDG_CACHE_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CONFIG_HOME` | `agent-cache/xdg`, `xdg-data`, `xdg-state`, `xdg-config` |
| `PNPM_STORE_DIR`, `NUGET_PACKAGES` | `agent-cache/pnpm-store`, `agent-cache/nuget` |
| `npm_config_cache`, `PIP_CACHE_DIR`, `UV_CACHE_DIR`, `HF_HOME` | `.dev-cache/npm-cache`, `pip`, `uv`, `huggingface` |
| `YARN_CACHE_FOLDER`, `TORCH_HOME`, `TRANSFORMERS_CACHE`, `PLAYWRIGHT_BROWSERS_PATH`, `PUPPETEER_CACHE_DIR`, `GOCACHE`, `GOMODCACHE`, `DOTNET_CLI_HOME`, `ELECTRON_CACHE`, `DENO_DIR` | `agent-cache/yarn`, `torch`, `transformers`, `playwright`, `puppeteer`, `go-build`, `go-mod`, `dotnet`, `electron`, `deno` |
| `CODEX_HOME`, `CLAUDE_CONFIG_DIR` (`--include-agent-homes`) | `agent-home/codex`, `agent-home/claude` |

`right-release disk probe [--json]` makes a real attempt to create, then immediately remove, one uniquely named 0-byte file (`.rk-disk-probe-<pid>-<nonce>`) in the user profile root, the local temp (`AppData\Local\Temp`, or `TMPDIR` on macOS), the OS temp and the external root. It writes nothing else. Run it from Claude, Codex and a child process as proof. It exits 1 when `internalWritable` is true (or a probe file could not be removed). `--json` prints exactly:

```json
{"schema":"rightkit.disk.probe.v1","host":"<hostname>","root":"<external root>","results":[{"location":"profile|local-temp|os-temp|external-root","path":"<directory probed>","outcome":"blocked|allowed|error","code":"<error code or null>"}],"internalWritable":false}
```

`outcome` is `allowed` (file created and removed), `blocked` (`EACCES`, `EPERM` or `EROFS`) or `error` (directory missing or another failure; `code` says which). `code` is `null` when allowed, except `UNLINK_<code>` when the probe file could not be removed. `internalWritable` is true when profile, local-temp or os-temp is `allowed`.

## Disk hygiene on the agent volume (`disk check`, `sweep`, `run`, `run-root`, `schedule`)

One job keeps the agent volume (`/Volumes/D` on macOS, `D:\` on Windows) from filling. `right-release disk schedule install` installs it: an hourly launchd job (`StartInterval` 3600) on macOS, one hourly repetition trigger in a hidden Task Scheduler entry on Windows. Both run `right-release disk run --auto` from a stable per-user copy of the CLI. `right-release disk schedule status` exits 1 when the binding is broken, the job is not loaded, or no full pass finished in the last 26 hours.

- **Full pass** (`disk run`, or `--auto` once a day, on the first run after 03:30 local time): the self-check, `disk check`, the sweep (stale idle data moves to holding), the purge of expired holding folders, and the warnings. Once per UTC day it also appends one `disk.report` event. Logged as `disk.run.ok`, or `disk.floor.warn` when a floor warns. Every run event (`disk.run.ok`, `disk.floor.warn`, `disk.run.light`) carries the agent volume's `freeBytes` and its `floorId`; with the agent volume unmounted both say so (`freeBytes` null) instead of reporting the first floor, the Mac internal drive. Before the move, each candidate is checked once more for a `.keep` marker, a live lease and fresh content: one that changed since the plan stays (`disk.sweep.skip`, reasons `rechecked-keep`, `rechecked-leased`, `rechecked-fresh`). Two items with one name from different parents land as `name`, `name.1`, `name.2`, and the move line names the source parent.
- **Light pass** (`disk run --light`, and `--auto` on every other hour): stat-only. It reads the free space of the floors and sweeps the writers marked `light` and the scratch roots. It sizes no writer, walks no workspace, looks for no stray target and purges nothing. Logged as `disk.run.light`. `--auto` decides from the newest full-pass line of `disk-hygiene.jsonl`, found by reading the log backwards in growing windows (4, 16, 64 MiB, then up to 256 MiB): a busy day's sweep lines never hide it. A missing log means the job never ran (full pass). A log that exists but cannot be read (EIO, ENOTDIR, a volume that went away) keeps the run light and is said so; `disk schedule status` prints the read error instead of "no completed full run".
- **Emergency**: a floor flagged `agentVolume` that warns (on macOS, `/Volumes/D` below its 250 GiB soft floor) purges the holding folders on that volume at once, with no undo window (`disk.run.emergency`), in the full pass and the light pass alike. Holding on another volume (C: while D: is squeezed) keeps its undo window, and a warning from any other floor or writer never purges. It acts only once the operator has recorded a calibration (below); an uncalibrated emergency purges nothing, logs `disk.emergency.skipped` (`reason` `uncalibrated`, `calibration-stale` or `log-unreadable`), prints `EMERGENCY SKIPPED (<reason>)` and the run goes on as an ordinary one (normal holding window, normal sweep).

**Before the first install: calibrate.** The floors in `disk-policy.json` are proposals until someone has compared them with the real volume, and the emergency purge has no undo window, so it stays off until you do. Run `right-release disk check --json` and compare each floor's `freeBytes` with its `thresholdBytes`, then `right-release disk check --calibrate`. When no `agentVolume` floor is warning, that appends one `disk.calibrated` event (every floor with `id`, `label`, `path`, `thresholdBytes`, `freeBytes`, `agentVolume`) to `disk-hygiene.jsonl` and exits 0. When one is warning it records nothing, names the floor and the shortfall, and exits 1: lower the floor in `disk-policy.json` or free space first, so that running the volume low is a decision, never an accident. An emergency is eligible only when the log holds a calibration that lists every `agentVolume` floor of the current policy with the same `thresholdBytes` (changing a threshold, or the volume's size under a percentage floor, makes it stale; calibrate again). A log that cannot be read counts as uncalibrated. `disk schedule install` repeats this step, and `disk schedule status` prints `calibration: recorded <age> ago` or `calibration: NOT RECORDED (emergency purge disabled)`.

`right-release disk check [--json]` lists each writer's size against its cap and each floor's free space (exit 3 on a warning). It also lists the unmanaged remainder of each agent volume, the top-level entries no writer covers, and the stray cargo targets (a `target` or `.cache/cargo-target` directory tagged with `CACHEDIR.TAG` outside RustRight's roots, up to depth 4 under `claude` and `scratch`). `right-release disk sweep [--dry-run] [--days N] [--delete]` moves what is due; `--delete` removes instead of moving, and `disk purge [--dry-run] [--days N]` removes expired holding folders on demand.

Lifetimes, from `packages/release/disk-policy.json`:

| Data | Idle or held for | Policy key |
| --- | --- | --- |
| Scratch child (`/Volumes/D/scratch/<name>`) | 1 day | `scratchStaleDays` |
| Stray cargo target | 1 day, and never while git tracks something in it | `strayTargetDays` |
| Holding folder `.rightkit-swept/YYYY-MM-DD` | the date plus 1 day has passed, and nothing in it was written in the last day | `holdingDays` |
| Sweep writers (caches, temp, DerivedData) | 7 days, or the writer's own `staleDays` | `staleDays` |
| Unknown top-level entry of a workspace root | 3 days; reported only, unless the host ran `disk allow-workspace-sweep` | `unknownStaleDays` |
| Directory holding a `.keep` file | 14 days after the marker's last write; a symlink named `.keep` counts for nothing | `keepMarkerDays` |
| Run root with a live lease | until `leaseUntil` | `disk run-root --days` (default 3, at most 30) |

A holding folder is a date-named directory under `.rightkit-swept` at the root of the item's own volume. Items from the Mac internal disk park in `/Volumes/D/.rightkit-swept` while that volume is mounted. The sweep moves a unit only when nothing in it is fresher than its window, no `.keep` marker or live lease protects it, and no process has it open.

**Run roots.** `right-release disk run-root <name> [--days N] [--json]` (N is positive and at most 30) leases `<scratch>/<name>` (`/Volumes/D/scratch/<name>` or `D:\scratch\<name>`): it creates the directory and writes `.rightkit-run.json` (`{"schema":1,"name","createdAt","leaseUntil"}`). Renewing keeps `createdAt` while the lease still fits 30 days from it, otherwise `createdAt` starts over. A lease counts only in the directory it names, may span at most 30 days (plus one day) from `createdAt`, and must not be created in the future; the guard applies the same rules, and refuses any write to a file named `.rightkit-run.json`, so only this command makes one. `right-release disk run-root --release <name>` removes the lease, and the directory then ages out by the scratch rules. Creation is refused, with nothing written, while the scratch root is over its `scratch-total` cap or the owner prefix (the part of the name before its first `-`) is over its `scratchPrefixCaps` entry (`legion` 40 GiB, `rk` 30 GiB, `pulse` 20 GiB, `hr` 20 GiB). The build-control heavy-write guard admits `git clone`, `git worktree add` and curl, wget and `hf`/`huggingface-cli download` outputs only below a live lease (see `packages/build-control-policy/README.md`); the guard reads this file format and never writes it.

**Build budget.** A writer whose `budget` names parts (`sccache` for the compiler-cache rows, `total` for `build-control-targets`) takes its cap from RustRight's `buildBudget`, read from `rustright doctor --json --no-scan` (20 s timeout, no shell, nothing cached). The writer reports `capSource: "rustright"` when it does. Without RustRight the writer keeps its policy `capBytes` and reports `capSource: "policy"`. The byte caps of the rows this change adds are proposals pending the host inventory.

**Daily report.** The `disk.report` event in `disk-hygiene.jsonl` (`schema` 1) carries free space and floors, bytes by category, growth since the previous report, the largest units and the growers, over-cap writers, unmanaged bytes, uncovered top-level entries, scratch bytes by owner and a steady-state estimate. Its field names are frozen in `disk-report.test.mjs`, because Pulse's Storage card reads them. The report describes the agent volume only: when it cannot be read, `freeBytes`, `usedBytes`, `totalBytes`, `categories`, `unmanagedBytes`, `overlapBytes` and `delta` are null (steady state zero), and so is the `delta` of the next report, which has no agent-volume baseline. `du` sums allocated bytes per writer, so APFS clones can make the writers add up to more than the used bytes: `overlapBytes` is that excess, `unmanaged` is then 0, nothing is scaled, and the identity is `sum(categories) - overlapBytes = usedBytes` (for a delta, `sum(delta.categories) - delta.overlapBytes = delta.usedBytes`). It does not yet count refusals or summarise the broker's GC passes.

## RightKit pins are proven against the registries (`rightkit-registry-resolver.mjs`)

At build admission the controller resolves the app's EXACT locked RightKit versions (every `Cargo.lock` entry and `pnpm-lock.yaml` resolution of a RightKit-owned package) against crates.io (index `cksum` + `yanked`) and npm (`dist.integrity` + `deprecated`). A new version of a known RightKit package needs no new release of this tool. The claim is scoped to KNOWN names: ownership comes from `rightkit-versions.json` and `rightkit-ownership.json`, a name prefix alone is refused, and there is no maintainer-membership proof, so a package first published after this tool still needs an ownership update. Only canonical registry sources count (no path, git, tarball or directory); the recorded minimum and `revoked` policy still apply; pins are never rewritten.

What the digest comparison is: the lock digest and the registry digest agree (metadata agreement). It is NOT proof of the bytes a build consumes; this tool does not materialize or hash the installed packages, so no byte-level claim is made.

- **Coverage.** Every RightKit dependency a manifest declares (`Cargo.toml`, `package.json`, including the directory of every canonical lock outside the app root) needs a lock entry for that package name somewhere in the canonical lock inventory; a missing or incomplete lock refuses admission, with no bundled-only fallback. This is a name-level check, not importer-specific lock proof. Lock inputs come from the build's canonical input inventory (`requiredInputs`, `pinnedCargoLocks`, native-iOS locks), not a separately built list.
- **Freshness is a build-admission gate.** A fresh `right-release build` reads each package's latest live (crates.io `max_stable_version`, npm `dist-tags.latest`) and REFUSES every pin behind it (`pinned X -> latest Y: run \`pnpm exec right-release update run\` ...`), held or not: a `.rightkit/update.json` hold is a migration lane task, reported with the refusal, never an exception. If latest cannot be read live the failure is RECOVERABLE (a cached latest is only last-observed). The latest row is always the live registry state. The cache keeps a separate high-water mark as downgrade protection for derived answers only: a lower crates.io answer while the higher version is still published is a recoverable `eligibility-conflict`; a withdrawn high-water version resolves itself; an npm `latest` tag moved lower on purpose is followed.
- **Reuse is trusted-local, not authenticated.** The admitted snapshot (`<state root>/rightkit-registry-snapshot.json`), the admission record (`rightkit-admission.json`: release id, commit, platform, snapshot digest, hash of `release-inputs.lock.json`) and the lock payload are plain files in the release state root. The release worker (`RIGHT_RELEASE_RIGHTKIT_SNAPSHOT`), `--resume-from`, the seal and `upload` check that they agree with each other, with this release's identity and with the current locks, and refuse a missing, foreign, inconsistent or stale set without resolving again. That catches mix-ups and edits that leave the files inconsistent. It does NOT authenticate them: anyone who can write the state root can forge a consistent set, and a resume proves only that the recorded admission is consistent with this release and the current locks, not that the registry still agrees or that the controller wrote it. There is no signing layer, and no existing broker attestation is reused here. A publication after admission changes nothing and freshness is not re-checked.
- **Sealed evidence.** The sealed `release-manifest.json` carries `rightReleaseVersion`, `rightkitRegistryEvidence: 1` and `rightkitRegistry` (`digest`, `snapshot`). A manifest without the evidence is refused on every seal and upload path. The only exemption is an explicitly identified pre-0.6.0 release: its manifest records no tool version (or one below 0.6.0) and its release id is named in `RIGHT_RELEASE_LEGACY_REGISTRY_RELEASES`. A stripped seal that records 0.6.0 or later is refused whatever is set.
- `right-release doctor` resolves live and lists newer versions as informational; `right-release doctor --admission` applies the admission refusals.
- Cache: `<RIGHT_RELEASE_CACHE_ROOT>/rightkit-registry/latest/` (external storage; none when no cache root is configured). There is no offline override for a fresh admission: a registry outage is a recoverable failure after bounded retries.

## Known issues

- `smol-toml` is pinned exactly. A consumer that depends on a different `smol-toml` version gets a second copy.
- Apps pin this package at different versions; `docs/ADOPTION.md` lists each pin and marks the stale ones.
- Prepublish runs the test suite, the Windows signer integration test and the registry parity check.

## Post-sign hook chains

`target.postSign` accepts either one command object or an array of command objects. An array runs in order, for example `create-mac-updater` followed by `lsclean`, and stops at the first failing command, so the release fails closed. Every command in the array is scanned for forbidden build features and release switches, exactly like a single command. A single object behaves as before.

Each command may set `timeoutMs`. When it expires the release kills the whole process tree (`taskkill /T /F` on Windows, the process group elsewhere) and exits non-zero, so a hung child such as Tauri leaves no orphan.


## App-specific forbidden release crates (G14)

Declare exact Cargo package names in `right-release.config.mjs`:

```js
export default {
  // Existing app, version & distribution fields...
  releaseDependencies: {
    manifest: "src-tauri/Cargo.toml",
    forbiddenCrates: [
      "rightkit-record", "rightkit-automation", "rightkit-hotkeys",
      "rightkit-overlay", "rightkit-agent-tools", "rightkit-qa", "rightkit-qa-driver",
    ],
    // Optional extra features for build flags hidden inside opaque scripts:
    features: [],
    // Set true only when every release command disables default features:
    noDefaultFeatures: false,
  },
  targets: {
    mac: { cargoTarget: "aarch64-apple-darwin", /* existing packaging config */ },
    win: { cargoTarget: "x86_64-pc-windows-msvc", /* existing packaging config */ },
  },
};
```

`right-release build` enforces this policy before packaging. `--skip-checks` skips only app scripts;
this gate also blocks dry runs when Cargo cannot resolve dependencies. Development builds do not run it.
Without `releaseDependencies`, existing apps retain their current behavior. An explicit empty
`forbiddenCrates: []` still verifies that Cargo can resolve its graph.

Standalone checks, run from app root:

```sh
pnpm exec right-release check-release-dependencies
pnpm exec right-release check-release-dependencies --platform win
pnpm exec right-release check-release-dependencies --all-targets
pnpm exec right-release check-release-dependencies --config path/to/right-release.config.mjs
```

Default selection uses host platform's release commands & host architecture's triple. `--all-targets` checks every configured
release target; non-host targets need `cargoTarget`, `targetTriple` or `rustTarget`. Each target must be
registered with managed Cargo. Unknown hosts fail unless an explicit platform or all targets is selected.

Graphs use `cargo tree --locked -e normal --prefix none --target <triple>` through normal Cargo dispatch.
Normal direct & transitive dependencies count; build-only & dev-only edges do not. Names match exactly,
including renamed dependencies' actual package names; versions, sources & duplicate tree entries do not
change matching. Checks never read Cargo.lock as evidence of reachability.

Each distinct command feature set is resolved separately. Commands, recipe steps & package scripts
(up to three nested script calls) contribute `--features`, `-F`, `-f`, `--all-features` &
`--no-default-features`; `releaseSwitches.features` & `releaseDependencies.features` add explicit features.
Declare flags hidden inside shell/Node script files in config. Manifest precedence is
`releaseDependencies.manifest`, target `cargoManifest`, `releaseSwitches.manifest`, native assembly
manifest, then `src-tauri/Cargo.toml` or `Cargo.toml`. Declared missing manifests fail.

SDK usage:

```js
import { assertReleaseDependencies, forbiddenCrateFindings } from "@rightkit/release/release-dependencies.mjs";

const evidence = assertReleaseDependencies({ root: appRoot, config, target: config.targets.mac });
// { manifest, target, trees, forbiddenCrates }; throws with crate & target on refusal.
const names = forbiddenCrateFindings(treeText, config.releaseDependencies.forbiddenCrates);
```

`run(command, args, options)` can inject Cargo execution for tests. Standalone CLI rejects missing policy,
invalid arguments, failed resolution & forbidden dependencies with nonzero exit status. Demo capability
files, base Tauri config content & app-specific default-feature restrictions remain separate guards.


## Template rename, PE digest, icon packing

`right-release init --name <Product Name> --id <reverse.dns.bundle.id> [--slug <lowercase>] [--dir <root>] [--dry-run]` renames the desktop app template identity in place (see `init-command.mjs`); it runs no gate or build. Besides the text files it renames `Cargo.lock` and re-sorts it the way cargo writes it (so `cargo --locked` accepts the renamed lock), carries the renamed lock's new digest into each `<lock> sha256 <hex>` / `sha256=<hex>` field of a notice `notice-lock-digest` (CA-14) judges whose lock resolves to it (the same text in reports, evidence, archives or source is left alone), recomputes the digests in `legal/legal-manifest.json` for the documents the rename changed (a changed material document means `legal-snapshot` must run again), keeps the `r2-rightapps` distribution service name, and prints the steps only a person can do: `pnpm install`, the doctor, the legal host and text, and registering the new repository on each host (`ownership-repositories.json` and its byte copy under `packages/build-control-policy`, then `right-release ownership trust-refresh` on the Mac and the Dell, which needs a person with administrator rights).

The release config is `right-release.config.json` (M24, `release-config.mjs`): every command that reads one defaults to it when it sits beside the working directory, loads a `.json` path through the strict loader, and still imports a legacy `right-release.config.mjs` for an app that has not moved. `{{localAppData}}` in a JSON config is `%LOCALAPPDATA%` on Windows and `<home>/AppData/Local` elsewhere.

`right-release pe digest <file> [--algorithm sha256|sha384|sha512] [--json]` prints the signature-independent PE content digest (Authenticode image hash: CheckSum, certificate table entry and certificate data excluded, so signing, re-signing and timestamping do not change it). `right-release pe imports <file> [--allow dll,...] [--allow-file <path>] [--json]` lists regular and delay-load imports and exits 1 when any is outside the allow list (case-insensitive; a trailing `*` allows a prefix such as `api-ms-win-*`; no allow list refuses every import). SDK: `peContentDigest(bytes)` and `verifyImportClosure(bytes, allowedDlls)` in `pe-content.mjs`.

`right-release icons ico|icns --out <file> --frame <PNG>... [--dir <folder>]` packs per-size PNG frames, hand-tuned small ones included, into an ICO (sizes 1..256, PNG-compressed entries) or an ICNS (16, 32, 64, 128, 256, 512, 1024). Frame sizes come from each PNG's IHDR; non-square, duplicate or unsupported sizes are refused. Rendering the PNG frames from a design master stays with the app's design source; the DMG background generator is `generate-dmg-background.py`.
