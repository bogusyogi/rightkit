# Declarative sidecar staging (`right-release stage-sidecar`)

Tracks bogusyogi/rightkit#345. Eight apps each hand-rolled the same script to put engines, ffmpeg, onnxruntime and model
files where Tauri `externalBin` and resources expect them. `packages/release/stage-sidecar.mjs` replaces those scripts with
a `sidecars` list in `right-release.config.mjs`. It is the build-time step before `tauri build`; it never signs
(release.mjs signs staged files, see `external-bin.mjs`).

## Configuration

```js
export default {
  sidecars: [
    { kind: "cargo-bin", package: "mailright-engine" },
    { kind: "cargo-bin", package: "mailright-core", features: ["mailright-core/test-mode"], profile: "dev" },
    { kind: "cargo-bin", package: "heardright-dump-helper", platforms: ["win"] },
    { kind: "ffmpeg", tools: ["ffmpeg", "ffprobe"], catalog: "../rightkit/crates/rightkit-ffmpeg/catalog/artifacts.json",
      notices: { dir: "src-tauri/resources/third-party/ffmpeg", sourceOfferContact: "https://example.com/source" },
      stage: { cmd: "rightkit", args: ["cargo", "run", "-p", "..."] } },
    { kind: "onnxruntime", version: "1.24.4", catalog: "../rightkit/crates/rightkit-ffmpeg/catalog/native-libraries.json",
      as: "libonnxruntime.dylib" },
    { kind: "resource", from: "models/voice.onnx", to: "src-tauri/resources/models/voice.onnx", sha256: "<64 hex>" },
  ],
  // or { sidecars: [...], stageDir: "src-tauri/binaries", receiptPath: "src-tauri/binaries/stage-sidecar.json" }
};
```

Every entry may carry `platforms: ["mac" | "win" | "linux"]`; entries that exclude the target are validated, skipped, and
listed under `skipped` in the receipt. Validation is strict and reports every problem at once as
`StageSidecarError` (`code: STAGE_SIDECAR_CONFIG_INVALID`, `details.errors`): unknown keys, bad names, absolute or `..`
paths, non-hex digests, two entries writing the same file, and bare `cargo` in an ffmpeg `stage` command.

### Kinds

| kind | what it does |
| --- | --- |
| `cargo-bin` | `rightkit cargo build --locked -p <package> --bin <bin> --target <triple> --profile <profile> [--features a,b]`. Copies the executable to `<stageDir>/<name>-<triple>[.exe]`. `bin` and `name` default to `package`; `profile` defaults to `release`. Optional `sha256` pins the built binary (only meaningful for reproducible builds). |
| `ffmpeg` | Uses `media-sidecar.mjs`: picks the approved catalog row per tool, runs the optional `stage` command, copies the notice set and the LGPL source offer, checks the staged file against the row's `contentSha256`. Same keys as `mediaSidecar` plus `platforms`. |
| `onnxruntime` | Picks the `approved` or `pinned` row of `native-libraries.json` for `version` and the target os/arch, downloads the pinned archive (digest checked before use), extracts the library (digest checked), writes it to `resourceDir` (default `src-tauri/resources/runtime`, name `as` or the catalog `artifactFileName`) and the notices to `licenseDir` (default `<resourceDir>/LICENSES`). Not an externalBin. Archives are cached in `.cache/onnxruntime`. |
| `resource` | Copies one file `from` to `to` (both inside the app) and verifies the optional `sha256`. Directories are refused; list each file. |

### Build output layout

The RustRight broker builds with an explicit `--target`, so output lands in `<targetDir>/<triple>/<profile>/`.
The executable path is taken from cargo's own `compiler-artifact` stream first. If the stream has none, the module reads
`cargo metadata` and looks in `<targetDir>/<triple>/<profile>/<bin>` then `<targetDir>/<profile>/<bin>` (`dev` and `test`
map to `debug`, `bench` to `release`, as in `swap-helper-bundle.mjs`). If neither exists it fails with
`STAGE_SIDECAR_BINARY_NOT_FOUND` and lists what it tried.

## Receipt

`<stageDir>/stage-sidecar.json` (contract `rightkit-stage-sidecar-v1`): `target`, `os`, `arch`, `stagedAt`, `skipped`, and
per file `kind`, `name`, `path` (app-relative), `sha256`, `sizeBytes`, `externalBin`, plus kind specific evidence
(`sourceSha256`, package, profile, features; catalog row id, version and digests; licence and notices with digests).
The staged-file hash is re-read after the copy and must equal the source hash. Identical files are not rewritten, so
Tauri's `rerun-if-changed` is not triggered needlessly. `validateStageSidecarReceipt` checks the shape. The returned
`externalBinNames` is the list to use as `targets.<platform>.externalBin.names`.

## Licence handling

Staging a binary without its licence text is refused; notices are part of the stage, not an afterthought.

- **ffmpeg (LGPL-2.1-or-later, `--disable-gpl --disable-nonfree`)**: `notices.dir` and `notices.sourceOfferContact`
  are required. The catalog row's `attribution.noticeFiles` (the LGPL text and build notes, relative to the
  rightkit-ffmpeg crate) are copied into `notices.dir`, and `FFMPEG-SOURCE-OFFER.txt` is generated there with the exact
  source URL, source archive digest, build provenance and your contact. A row lacking notice files or a source URL, or
  one that is not `approved` with a content digest, fails (`MEDIA_SIDECAR_*`, wrapped as `STAGE_SIDECAR_FFMPEG_FAILED`
  with `details.causeCode`). The app must ship `notices.dir` in its resources (for Tauri, list it in `bundle.resources`).
- **onnxruntime (MIT)**: the row's `noticeFiles` (for example `ONNXRuntime-MIT.txt`, and the third-party notices on the
  Windows DirectML row) are copied to `licenseDir`. A row with no notice files fails with `STAGE_SIDECAR_NOTICE_MISSING`.
  Ship `licenseDir` alongside the library and include it in the app's third-party licence screen.
- Planned or unapproved rows (for example onnxruntime macOS x86_64, or ffmpeg on Linux today) fail closed with
  `STAGE_SIDECAR_NOT_APPROVED` rather than staging an unverified binary.

## Errors

All failures are `StageSidecarError` with a stable `code`: `STAGE_SIDECAR_CONFIG_INVALID`, `_TARGET_UNSUPPORTED`,
`_TARGET_UNKNOWN`, `_BUILD_FAILED`, `_BINARY_NOT_FOUND`, `_HASH_MISMATCH` (`details.expected`/`actual`),
`_NOT_APPROVED`, `_NOTICE_MISSING`, `_CATALOG_UNREADABLE`, `_DOWNLOAD_FAILED`, `_EXTRACT_FAILED`, `_RESOURCE_MISSING`,
`_FFMPEG_FAILED`, `_RECEIPT_INVALID`.

## API

`stageSidecars({ appRoot, manifest, target, deps })` returns `{ receipt, receiptPath, files, externalBinNames }`.
Other exports: `validateSidecarManifest`, `parseTargetTriple`, `resolveTargetTriple`, `builtBinaryCandidates`,
`validateStageSidecarReceipt`, `StageSidecarError`. `deps` (all optional) injects `fs`, `run(command, args, { cwd, env })`,
`fetchBytes`, `extractMember`, `loadMediaCatalog`, `loadNativeCatalog`, `makeTempDir`, `removeDir`, `now`, `pid`, `log`, `env`.
Tests use fakes only (`stage-sidecar.test.mjs`).

## CLI wiring (for release owner)

`right-release stage-sidecar [--target <triple>] [--app-root <dir>] [--config <file>]`: load the `sidecars` key from
`right-release.config.mjs`, resolve the target with `resolveTargetTriple({ target })` (`--target`, then
`TAURI_ENV_TARGET_TRIPLE`, then `rightkit rustc -vV`), call `stageSidecars`, print the staged paths. Apps then call it
from `beforeBuildCommand` instead of their own scripts.

## Not covered

Directory resources, signing, and running
`tauri build`. The apps' dev-mode QA flags (such as mailright's `--test-mode`) map to `profile: "dev"` plus `features`
in a dev-specific manifest; selecting between manifests is the caller's job.

## Build profile, external catalogs, source builds and universal macOS (bogusyogi/rightkit#345)

**Profile switch.** A `cargo-bin` entry's `profile` is a Cargo profile name or `{ dev: "release-iterate", release: "release" }`
(exactly those two keys, both required). `RIGHTKIT_BUILD_MODE` (`dev` | `release`, default `release`, anything else refused)
selects. The wrapper form accepts a default for entries without their own: `{ sidecars: [...], profiles: { dev, release } }`.

**Catalog sources.** `ffmpeg` and `onnxruntime` `catalog` accepts a path relative to the app, an absolute path,
`{ crate: "rightkit-ffmpeg", file? }` (the crate directory comes from `rightkit cargo metadata`, so a path or registry
dependency both work; `file` defaults to `catalog/artifacts.json` for ffmpeg and `catalog/native-libraries.json` for
onnxruntime) or `{ npm: "<pkg>/<file>" }` (found by walking up `node_modules` from the app, scoped packages allowed). A bad
spec is reported against its own entry (`sidecars[2] (onnxruntime): catalog.crate must be ...`) alongside every other
problem; an unresolvable one fails at staging with `STAGE_SIDECAR_CATALOG_UNREADABLE` naming the crate or package.

**`command` kind** for source builds (whisper.cpp with cmake):

```js
{ kind: "command", name: "whisper-cli", cmd: "cmake", args: ["--build", "build", "--config", "Release"],
  cwd: "vendor/whisper.cpp", output: "vendor/whisper.cpp/build/bin/whisper-cli", externalBin: true }
```

`cmd` is one executable (no spaces, no shell); `args` is an argv list. `cwd` and `output` are relative to the app root;
`externalBin` must be `true`. After the command exits 0 the output is staged as `<name>-<triple>[.exe]` and its sha256 is
recorded in the receipt (`kind: "command"`). A missing output or a non-zero exit stops staging.

**`universal-apple-darwin`.** `cargo-bin` builds `aarch64-apple-darwin` and `x86_64-apple-darwin`, runs
`lipo -create` (through `background-process.mjs`), checks `lipo -archs` shows both, and stages
`<name>-universal-apple-darwin`; the receipt lists both `slices`. A `command` output must already be universal
(`lipo -archs` showing `arm64` and `x86_64`), and a `resource` with `machO: true` is checked the same way. `ffmpeg` and
`onnxruntime` have no universal catalog row and are refused for this target: stage a lipo-merged file with `command`
or `resource`.
