# Crate declarations for consumers (rightkit#1346)

A crate that works inside the RightKit tree can still need something its consuming app must do: build and bundle a helper executable, declare an OS permission, patch a transitive crate, or limit itself to some targets. Cargo does not carry any of that to the app, so each crate declares it in its own `Cargo.toml` under `[package.metadata.rightkit]`, and release tooling reads the declaration and checks the app. This file is the schema. The declarations below exist today; the checks that read them are owned by `@rightkit/release` and the gate (not yet written; see "Checks still to write").

## `[[package.metadata.rightkit.helpers]]`

Cargo never builds a dependency's `[[bin]]`, so an app that links a crate whose code starts a helper process must build, bundle and sign that helper itself.

| Key | Meaning |
|---|---|
| `name` | The executable name (without `.exe`). |
| `kind` | `cargo-bin` (a `[[bin]]` of this crate), `swift-package` or `external`. |
| `required_when_feature` | The feature of this crate that needs the helper. Absent means always. |
| `build_features` | Features the helper itself is built with. |
| `platforms` | `macos`, `windows`, `linux`. |
| `bundle_path` | Where the crate looks for it; `beside-app-executable` today. |
| `env_override` | The environment variable that overrides discovery, if any. |
| `sign` | `true` when the app's signing identity must sign it. |

Declared: `rightkit-tts` (`rightkit-tts-worker`). Still to declare, by their owners: `rightkit-record` (macOS Swift helper, Windows `rightkit-record-helper`) and `rightkit-crash` (`rightkit-dump-helper`).

## `[[package.metadata.rightkit.permissions]]`

| Key | Meaning |
|---|---|
| `feature` | The feature of this crate that needs the permission. |
| `platform` | `macos`, `windows`, `android`, `ios`. |
| `info_plist` | Info.plist keys the app must set (macOS and iOS). |
| `entitlements` | Entitlements the app must sign with. |
| `reason` | Why, for the reviewer. |

Declared: `rightkit-audio` feature `capture` on macOS (`NSMicrophoneUsageDescription`, `com.apple.security.device.audio-input`). Still to declare, by their owner: `rightkit-record` (screen and audio capture).

## `[[package.metadata.rightkit.consumer_patches]]`

A `[patch.crates-io]` applies only in the workspace that writes it, so a published crate does not carry its own patch.

| Key | Meaning |
|---|---|
| `crate` | The patched crate name. |
| `registry_version` | The registry version the patch replaces. |
| `source` | The RightKit path of the fixed copy. |
| `requires` | Dependency requirements the fixed copy must satisfy. |
| `advisories` | The advisories it fixes. |
| `remove_when` | When the patch can go. |

Declared: `rightkit-typeset` (`citationberg`). The fork is not published under a RightKit name: typst and hayagriva name `citationberg` themselves, so only a patch of that name replaces it.

## `supported_targets`

`[package.metadata.rightkit] supported_targets = ["<triple>", ...]` lists the targets a crate builds for. Declared: `rightkit-tts-qwen-sys`. An unsupported target stops in its `build.rs` with a message that lists them.

## Checks still to write

1. Gate bundle check: for each `helpers` entry of a crate the app enables, the app's sealed bundle contains the executable, signed with the app identity.
2. Release permission check: for each `permissions` entry of an enabled feature, the app's Info.plist and entitlements carry the keys.
3. Consumer patch check: the app's manifest declares the patch and its lock resolves the required version.
4. Consumer lockfile check for the `katex` override (see `@rightkit/doc-render` `rightkit-doc-render-check-katex`).
5. A published-version variant of the all-packages app (`e2e-app`) built from registry versions, with no path or link dependencies, so inheritance defects like these fail there.
