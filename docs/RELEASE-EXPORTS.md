# @rightkit/release: public module surface

Plan item 2.10 (gaps C4-16, C4-17). `@rightkit/release` had no `exports` map, so every file in the package was importable and consumers imported 24 internal modules (signing, notarization, DMG, packaging, evidence) by deep path. The package could not rename or split a module without breaking a release script in another repository. This file is the inventory the map was cut from; `packages/release/release-exports.test.mjs` keeps the map, this table and the package in step.

## Rule

- `packages/release/package.json` lists every public subpath as its own `"./<file>"` entry with the same file as its target. There is no `"."` entry (the package has no importable index; `import "@rightkit/release"` fails, as before), no wildcard and no condition object, so `import`, `import()`, `import.meta.resolve` and `require.resolve` all see the same set.
- Every other file in the package is private. Node refuses it with `ERR_PACKAGE_PATH_NOT_EXPORTED`.
- An entry has one reason to exist: it is a CLI file (`bin`), `package.json`, a module the README documents as SDK (`sdk`), or a module a checked-out consumer imports today (`app-import`). A new public subpath lands in the same commit as its inventory row or README section; the test fails otherwise.
- `app-import` rows are the baseline from 2026-10-24. They are expected to shrink as the signing, notarization and packaging flows move behind `right-release` recipes (plan item 2.1 and the `winPackage` / `macPackage` recipes). Removing a row means the last consumer is gone; removing the map entry is a breaking change for @rightkit/release and needs a version bump.
- The map does not govern a file read by path (`node_modules/@rightkit/release/<file>`). Those accesses are listed below so they can be moved to `import.meta.resolve` of an exported subpath.

## Exports map

| Subpath | Class | Public because |
|---|---|---|
| `./bin/rightkit-gate.mjs` | bin | target of the `rightkit-gate` bin; hooks and docs name the installed path |
| `./cargo-contract.mjs` | app-import | imported by heardright |
| `./cargo-target.mjs` | app-import | imported by coderight, cutright, heardright, postright, pulse, rightkit/desktop-app-template, scraperight, viewright |
| `./cli/right-release.mjs` | bin | target of the `right-release` bin; `@rightkit/legal` resolves it with `createRequire().resolve`, scripts in four apps and Legion spawn it by path |
| `./development.mjs` | app-import | imported by cutright, ember, mailright |
| `./direct-bootstrap.mjs` | app-import | imported by coderight, legion |
| `./disk-policy.mjs` | app-import | imported by pulse |
| `./dmg-build.mjs` | app-import | imported by cutright; README documents `buildDmg` as the SDK form of `right-release dmg build` |
| `./engine-locator.mjs` | sdk | README (Commands, Engine locator): `locateEngine` is importable from it |
| `./github-release.mjs` | app-import | imported by legion |
| `./legal-contract.mjs` | app-import | imported by coderight, heardright, mailright, viewright |
| `./macos-file-evidence.mjs` | app-import | imported by legion |
| `./macos-notarization.mjs` | app-import | imported by heardright |
| `./macos-signing-identity.mjs` | app-import | imported by cutright |
| `./macos-signing-session.mjs` | app-import | imported by heardright |
| `./managed-cargo.mjs` | app-import | imported by ember, pulse |
| `./native-release-finalization.mjs` | app-import | imported by legion |
| `./notary-auth.mjs` | app-import | imported by heardright, scraperight |
| `./package.json` | package | tools read the installed version and the `bin` table; Node itself needs it listed once an `exports` map exists |
| `./packaged-app-identity.mjs` | app-import | imported by pulse |
| `./pe-content.mjs` | sdk | README (Template rename, PE digest, icon packing): `peContentDigest` and `verifyImportClosure` are the SDK |
| `./publication-gate.mjs` | app-import | imported by rightkit/packages/ci |
| `./qa-build-features.mjs` | app-import | imported by ember |
| `./release-dependencies.mjs` | sdk | README (SDK usage): `assertReleaseDependencies` and `forbiddenCrateFindings` |
| `./sign-windows.mjs` | app-import | imported by coderight, heardright |
| `./signing-agent-client.mjs` | app-import | imported by heardright |
| `./stage-binary.mjs` | app-import | pulse loads it with a dynamic import of a file URL built from node_modules (`scripts/release/windows-payload.mjs:219`, see Path accesses) |
| `./stage-sidecar.mjs` | app-import | imported by cutright, pulse |
| `./supply-chain-evidence.mjs` | app-import | imported by coderight, legion |
| `./tauri-bundle-marker.mjs` | app-import | imported by cutright |

## Import inventory

Imports are complete quoted specifiers `@rightkit/release/<subpath>` in code files (JavaScript and TypeScript, Rust strings and shell or workflow snippets that hand the name to Node), found by `packages/release/test-support/release-exports-scan.mjs`. Comment lines, Markdown, patches and audit JSON are history, not imports. Locations are relative to the repository named in the Consumer column; the scan read each working tree at the commit below.

| Repository | Scanned at | Imports |
|---|---|---|
| coderight | `9e2b2ffa4` | 6 |
| cutright | `f18ce876f` | 9 |
| genright | `114071d92` | 0 |
| heardright | `6ba4bbae9` | 11 |
| mailright | `16910929c` | 2 |
| postright | `a7fb58361` | 1 |
| pulse | `e39b3d1d3` | 5 |
| scraperight | `472eb2691` | 2 |
| viewright | `d1bac5af3` | 2 |
| sellright | `86b050d09` | 0 |
| legion | `cfe76b42d` | 15 |
| ember | `d95ba26c8` | 8 |
| rightkit/desktop-app-template | this commit | 2 |
| rightkit/packages/legal | this commit | 1 |
| rightkit/packages/ci | this commit | 2 |

GenRight and SellRight import nothing from the package by subpath (GenRight mentions one only inside archived patch files).

| Subpath | Consumer | Location | Form |
|---|---|---|---|
| `cargo-contract.mjs` | heardright | `tauri-app-next/scripts/clean-bundle-output.mjs:5` | static import |
| `cargo-target.mjs` | coderight | `apps/coderight-tauri/scripts/lib/target-root.mjs:2` | static import |
| `cargo-target.mjs` | cutright | `apps/cutright/right-release.config.mjs:4` | static import |
| `cargo-target.mjs` | cutright | `apps/cutright/scripts/dev-build.mjs:8` | static import |
| `cargo-target.mjs` | cutright | `apps/cutright/scripts/lib/target-root.mjs:2` | static import |
| `cargo-target.mjs` | cutright | `apps/cutright/scripts/mac-dmg.mjs:15` | static import |
| `cargo-target.mjs` | heardright | `tauri-app-next/scripts/lib/target-root.mjs:3` | static import |
| `cargo-target.mjs` | postright | `right-release.config.mjs:6` | static import |
| `cargo-target.mjs` | pulse | `scripts/release/mac-payload.mjs:114` | dynamic import |
| `cargo-target.mjs` | rightkit/desktop-app-template | `desktop-app-template/right-release.config.mjs:7` | static import |
| `cargo-target.mjs` | rightkit/desktop-app-template | `desktop-app-template/scripts/build-mac-notarized.sh:56` | static import in a node snippet |
| `cargo-target.mjs` | scraperight | `right-release.config.mjs:4` | static import |
| `cargo-target.mjs` | viewright | `scripts/lib/target-root.mjs:5` | static import |
| `cli/right-release.mjs` | rightkit/packages/legal | `packages/legal/bin/rust-licenses.mjs:13` | string literal |
| `development.mjs` | cutright | `apps/cutright/scripts/dev-build.mjs:9` | static import |
| `development.mjs` | ember | `scripts/desktop/build-native.mjs:5` | static import |
| `development.mjs` | ember | `scripts/desktop/candidate.mjs:4` | static import |
| `development.mjs` | mailright | `scripts/stage-dev-sidecars.mjs:6` | static import |
| `direct-bootstrap.mjs` | coderight | `apps/coderight-tauri/scripts/finalize-headless-release.mjs:6` | static import |
| `direct-bootstrap.mjs` | coderight | `apps/coderight-tauri/scripts/package-windows-headless.mjs:5` | static import |
| `direct-bootstrap.mjs` | legion | `engine/xtask/src/package_windows_release/finalize.rs:254` | module name handed to node |
| `direct-bootstrap.mjs` | legion | `engine/xtask/src/package_windows_release/finalize.rs:270` | module name handed to node |
| `direct-bootstrap.mjs` | legion | `engine/xtask/src/package_windows_release/finalize.rs:284` | module name handed to node |
| `direct-bootstrap.mjs` | legion | `engine/xtask/src/package_windows_release/finalize.rs:304` | module name handed to node |
| `direct-bootstrap.mjs` | legion | `engine/xtask/src/rightkit_release_bridge.rs:74` | dynamic import in a node snippet |
| `disk-policy.mjs` | pulse | `scripts/release/scratch-root.mjs:9` | dynamic import |
| `dmg-build.mjs` | cutright | `apps/cutright/scripts/mac-dmg.mjs:16` | static import |
| `github-release.mjs` | legion | `engine/xtask/src/package_windows_release/finalize.rs:330` | module name handed to node |
| `github-release.mjs` | legion | `engine/xtask/src/package_windows_release/finalize.rs:350` | module name handed to node |
| `legal-contract.mjs` | coderight | `apps/coderight-tauri/scripts/test-legal-release-gate.mjs:8` | static import |
| `legal-contract.mjs` | heardright | `tauri-app-next/scripts/assert-release-contracts.mjs:4` | static import |
| `legal-contract.mjs` | heardright | `tauri-app-next/scripts/test-legal-release-gate.mjs:9` | static import |
| `legal-contract.mjs` | mailright | `scripts/test-legal-release-gate.mjs:8` | static import |
| `legal-contract.mjs` | viewright | `scripts/check-legal-contracts.mjs:11` | static import |
| `macos-file-evidence.mjs` | legion | `scripts/ci/right-git-ci.sh:1469` | static import in a node snippet |
| `macos-notarization.mjs` | heardright | `tauri-app-next/scripts/mac-dmg.mjs:31` | static import |
| `macos-signing-identity.mjs` | cutright | `apps/cutright/scripts/sign-macos-bundle.mjs:11` | static import |
| `macos-signing-session.mjs` | heardright | `tauri-app-next/scripts/assemble-macos-ax-qa.mjs:6` | static import |
| `macos-signing-session.mjs` | heardright | `tauri-app-next/scripts/preflight-macos-release.mjs:7` | static import |
| `macos-signing-session.mjs` | heardright | `tauri-app-next/scripts/sign-cloudkit-dev-helper.mjs:6` | static import |
| `managed-cargo.mjs` | ember | `scripts/desktop/build-native.mjs:4` | static import |
| `managed-cargo.mjs` | ember | `scripts/desktop/candidate-qualification.mjs:4` | static import |
| `managed-cargo.mjs` | ember | `scripts/desktop/gate.mjs:4` | static import |
| `managed-cargo.mjs` | ember | `scripts/desktop/installed-qualification.mjs:3` | static import |
| `managed-cargo.mjs` | ember | `scripts/desktop/static-check.mjs:129` | string literal |
| `managed-cargo.mjs` | pulse | `scripts/release/cargo.mjs:5` | static import |
| `native-release-finalization.mjs` | legion | `engine/xtask/src/rightkit_release_bridge.rs:190` | dynamic import in a node snippet |
| `notary-auth.mjs` | heardright | `tauri-app-next/scripts/mac-dmg.mjs:28` | static import |
| `notary-auth.mjs` | scraperight | `right-release.config.mjs:5` | static import |
| `packaged-app-identity.mjs` | pulse | `scripts/release/candidate.mjs:6` | static import |
| `publication-gate.mjs` | rightkit/packages/ci | `packages/ci/templates/publish-npm.yml:53` | static import in a node snippet |
| `publication-gate.mjs` | rightkit/packages/ci | `packages/ci/templates/publish-npm.yml:70` | static import in a node snippet |
| `qa-build-features.mjs` | ember | `scripts/desktop/build-native.mjs:6` | static import |
| `sign-windows.mjs` | coderight | `apps/coderight-tauri/scripts/package-windows-headless.mjs:115` | import.meta.resolve |
| `sign-windows.mjs` | heardright | `tauri-app-next/scripts/build-windows-identity.mjs:70` | import.meta.resolve |
| `signing-agent-client.mjs` | heardright | `tauri-app-next/scripts/preflight-macos-release.mjs:8` | static import |
| `stage-sidecar.mjs` | cutright | `apps/cutright/scripts/dev-build.mjs:12` | static import |
| `stage-sidecar.mjs` | pulse | `scripts/release/stage-receipt.mjs:7` | static import |
| `supply-chain-evidence.mjs` | coderight | `apps/coderight-tauri/scripts/finalize-headless-release.mjs:2` | static import |
| `supply-chain-evidence.mjs` | legion | `engine/xtask/src/package_windows_release/finalize.rs:239` | module name handed to node |
| `supply-chain-evidence.mjs` | legion | `engine/xtask/src/package_windows_release/finalize.rs:245` | module name handed to node |
| `supply-chain-evidence.mjs` | legion | `engine/xtask/src/rightkit_release_bridge.rs:92` | dynamic import in a node snippet |
| `supply-chain-evidence.mjs` | legion | `engine/xtask/src/rightkit_release_bridge.rs:107` | dynamic import in a node snippet |
| `supply-chain-evidence.mjs` | legion | `engine/xtask/src/rightkit_release_bridge.rs:124` | dynamic import in a node snippet |
| `supply-chain-evidence.mjs` | legion | `engine/xtask/src/rightkit_release_bridge.rs:146` | dynamic import in a node snippet |
| `tauri-bundle-marker.mjs` | cutright | `apps/cutright/scripts/dev-build.mjs:11` | static import |

One row is a string check, not an import: ember `scripts/desktop/static-check.mjs:129` verifies that its gate script still contains `@rightkit/release/managed-cargo.mjs`, so renaming that module would fail Ember's static check.

## Path accesses

Code that reaches into the installed package by file path instead of by specifier. The exports map does not apply to these, so they keep working, but they are the next thing a package move breaks. "Map" says whether the file is also exported.

| Subpath | Consumer | Location | Use | Map |
|---|---|---|---|---|
| `cli/right-release.mjs` | coderight | `apps/coderight-tauri/scripts/before-build.mjs:113` | spawns the CLI file with Node | exported |
| `cli/right-release.mjs` | coderight | `apps/coderight-tauri/scripts/verify-packaged-app.mjs:25` | spawns the CLI file with Node | exported |
| `cli/right-release.mjs` | legion | `engine/xtask/src/release/installer_release_chain.rs:160` | spawns the CLI file with Node | exported |
| `cli/right-release.mjs` | legion | `engine/xtask/src/release/windows_finalize_installer.rs:303` | spawns the CLI file with Node | exported |
| `cli/right-release.mjs` | pulse | `scripts/release/windows-dev-install.mjs:37` | spawns the CLI file with Node | exported |
| `cli/right-release.mjs` | pulse | `scripts/release/windows-dev-install.mjs:129` | spawns the CLI file with Node | exported |
| `cli/right-release.mjs` | pulse | `scripts/release/windows-dev-session.mjs:29` | spawns the CLI file with Node | exported |
| `cli/right-release.mjs` | pulse | `scripts/release/windows-dev-session.mjs:34` | spawns the CLI file with Node | exported |
| `cli/right-release.mjs` | pulse | `scripts/release/windows-payload.mjs:280` | spawns the CLI file with Node | exported |
| `package.json` | viewright | `scripts/check-release-truth.mjs:190` | reads the installed version | exported |
| `rightkit-versions.json` | viewright | `scripts/check-release-truth.mjs:191` | reads the version feed | not exported |
| `sign-windows.mjs` | pulse | `scripts/release/windows-payload.mjs:310` | spawns the signer script with Node | exported |
| `stage-binary.mjs` | pulse | `scripts/release/windows-payload.mjs:219` | dynamic import of a file URL | exported |
| `upload-release.mjs` | heardright | `tauri-app-next/scripts/test-release-build-upload-contract.mjs:46` | reads the file as text for a contract test | not exported |

`rightkit-versions.json` and `upload-release.mjs` are read, never imported, and are not exported. When an app needs one of them it should ask for an export (an inventory row) rather than build the path.

## Refresh

```sh
# from packages/release, with the product checkouts beside rightkit
node test-support/release-exports-scan.mjs coderight=../../../coderight pulse=../../../pulse   # rows in this table's format
node --test release-exports.test.mjs                                                           # map, inventory and package agree; live scan of every checkout found
RIGHTKIT_REQUIRE_ASSETS=1 node --test release-exports.test.mjs                                 # a missing checkout fails instead of skipping
```

The test also scans this repository (the app template, the CI workflow templates, `@rightkit/legal`) on every run, skipping tests, `docs/`, `release-evidence/` and `ci/consumers/` (the packed-consumer recipe names internal modules on purpose, to prove the map refuses them). It scans the product checkouts beside rightkit when they exist and fails on any import outside the map. A hosted checkout holds rightkit alone, so there the live scan skips with its reason and the map, inventory and package checks still run.
