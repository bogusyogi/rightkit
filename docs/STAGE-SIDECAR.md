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
      stage: { cmd: "rustright", args: ["cargo", "run", "-p", "..."] } },
    { kind: "onnxruntime", version: "1.24.4", catalog: "../rightkit/crates/rightkit-ffmpeg/catalog/native-libraries.json",
      as: "libonnxruntime.dylib" },
    { kind: "native-library", library: "pdfium", version: "chromium/8021", catalog: { crate: "rightkit-ffmpeg" } },
    { kind: "native-library", library: "ggml", version: "0.20.2", catalog: { crate: "rightkit-ffmpeg" },
      source: { dir: "../retained-builds/whisper-cpp-1.9.3/macos-arm64" }, platforms: ["mac"] },
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
| `cargo-bin` | `rustright cargo build --locked -p <package> --bin <bin> --target <triple> --profile <profile> [--features a,b]`. Copies the executable to `<stageDir>/<name>-<triple>[.exe]`. `bin` and `name` default to `package`; `profile` defaults to `release`. Optional `sha256` pins the built binary (only meaningful for reproducible builds). |
| `ffmpeg` | Uses `media-sidecar.mjs`: picks the approved catalog row per tool, runs the optional `stage` command, copies the notice set and the LGPL source offer, checks the staged file against the row's `contentSha256`. Same keys as `mediaSidecar` plus `platforms`, `sources`, `probe` and `resourceDir`. By default the tools are externalBins (`<stageDir>/<tool>-<triple>[.exe]`); with `resourceDir` they are staged flat as `<resourceDir>/<tool>[.exe]` (no triple, not an externalBin), see "ffmpeg in a resource directory" below. |
| `onnxruntime` | The alias of `native-library` with `library: "onnxruntime"` (below), with its own keys (`version` is `x.y.z`, no `library`, `files` or `source`), receipt kind `onnxruntime` and cache `.cache/onnxruntime`. Picks the `approved` or `pinned` row of `native-libraries.json` for `version` and the target os/arch, downloads the pinned archive (digest checked before use), extracts the library (digest checked), writes it to `resourceDir` (default `src-tauri/resources/runtime`, name `as` or the catalog `artifactFileName`) and the notices to `licenseDir` (default `<resourceDir>/LICENSES`). Not an externalBin. A row's `companions` are staged beside it (`companions: false` opts out): on Windows `DirectML.dll` and its licence come with `onnxruntime.dll`. |
| `native-library` | `{ library, version, catalog, resourceDir?, licenseDir?, cacheDir?, as?, files?, companions?, source?, platforms? }`: stages a dynamic library from catalog rows. See "Native libraries" below. |
| `resource` | Copies one file `from` to `to` (both inside the app) and verifies the optional `sha256`. `from` may also be a directory: it is copied recursively into `to` and every file is hashed into the receipt (one row per file; `sha256` and `machO` are refused for a directory). |
| `remote` | `{ name, url, sha256, dest \| to, size? }`: an https download pinned by digest. See "Placement, remote files" below. |

### ffmpeg in a resource directory (`resourceDir`)

Tauri's `externalBin` layout names the tools `ffmpeg-<triple>` and Tauri strips the triple in the bundle. Apps that load ffmpeg and
ffprobe from a resource directory instead (CutRight, ScrapeRight and HeardRight stage them into `runtime/`) set `resourceDir` on the
`ffmpeg` entry. The tools then land at `<resourceDir>/ffmpeg[.exe]` and `<resourceDir>/ffprobe[.exe]`, from the same approved catalog
rows, with the same digest check, notices and source offer, `sources` fetch, optional `probe`, universal lipo merge and receipt as
the externalBin layout. Without `resourceDir` nothing changes.

```js
// export-ass ffmpeg and decode ffprobe in one entry, fetched from the pinned archives, staged flat into runtime/
{ kind: "ffmpeg", resourceDir: "src-tauri/resources/runtime", variants: { ffmpeg: "export-ass" },
  catalog: { crate: "rightkit-ffmpeg" },
  notices: { dir: "src-tauri/resources/third-party/ffmpeg", sourceOfferContact: "https://example.com/source" },
  sources: {
    ffmpeg: { url: "https://example.com/ffmpeg-export-ass.zip", sha256: "<archive sha256>", member: "ffmpeg" },
    ffprobe: { url: "https://example.com/ffprobe-decode.zip", sha256: "<archive sha256>", member: "ffprobe" },
  } }
// -> src-tauri/resources/runtime/ffmpeg, src-tauri/resources/runtime/ffprobe  (ffmpeg.exe, ffprobe.exe on Windows)
```

- **`resourceDir`** is a contained relative path (the same validation as the `onnxruntime` `resourceDir`: not absolute, no `..`, not
  the app root). `variants` (per-tool variant) and `variant` work as before: each tool's row is chosen by its own variant, so one entry
  may stage the export-ass ffmpeg and the decode ffprobe.
- **`bundleDir` is the externalBin key** (where Tauri puts the externalBin inside a macOS bundle), so `bundleDir` together with
  `resourceDir` is refused at validation (`STAGE_SIDECAR_CONFIG_INVALID`, "bundleDir and resourceDir cannot be combined"). Drop
  `bundleDir`; `resourceDir` files are plain resources, which `tauri.conf.json` lists under `bundle.resources`.
- **Receipt rows** keep `kind: "ffmpeg"` and every field of the externalBin rows (`catalogRow`, `version`, `catalogContentSha256`,
  `variant`, `notices`, `probe`, `slices` for a universal file) with `path` inside `resourceDir` and `externalBin: false`, so
  `externalBinNames` leaves them out (list them under `bundle.resources`, not `externalBin.names`). The notices and the source offer
  still go to `notices.dir`.
- **Outputs.** `--check` lists `<resourceDir>/<tool>[.exe]` for each tool (`<resourceDir>/<tool>` for universal-apple-darwin) with no
  `(externalBin)` mark. Those paths are claimed like every output: two entries (or a `resource`, `native-library` or `dest`) may not
  write the same file, and an entry that writes `resourceDir` itself is refused as overlapping. A `resourceDir` entry no longer claims
  `<stageDir>/<tool>-<triple>`. `wipeStageDir` only ever lists `stageDir` and removes `<name>-<current triple>[.exe]` there, so it never
  touches a `resourceDir` file (a stale triple-named leftover of an earlier externalBin layout in `stageDir` is still wiped).
- **The `sources` route fits `resourceDir` best.** Each declared tool is fetched, its member digest is checked against the approved
  row, and only then copied to `<resourceDir>/<tool>`, so a swapped archive stages nothing. A file already in `resourceDir` that matches
  its row is not rewritten, but it is made executable (`0o755` off Windows) like a fresh copy, so a file left at another mode by an
  earlier run or a checkout does not stay unrunnable; one that does not match is replaced.
- **A `stage` command and `resourceDir`.** The command runs as before (`cwd` the app root) and its arguments are not rewritten on a thin
  target, so `{stageDir}` is not substituted there. Because the entry verifies `<resourceDir>/<tool>`, a command used with `resourceDir`
  must write the tools there itself (pass it the directory); a command that still writes `<stageDir>/<tool>-<triple>` (what
  `rightkit_ffmpeg::stage_external_bin` does) fails with `STAGE_SIDECAR_FFMPEG_FAILED` (`details.causeCode: MEDIA_SIDECAR_NOT_STAGED`)
  naming the `resourceDir` path it expected. On `universal-apple-darwin` nothing moves: each slice is still staged by the command under
  `.right-release/slices/<triple>` (`{arch}`, `{triple}` and `{stageDir}` name the slice, as for an externalBin entry) and only the
  lipo-merged file is written to `<resourceDir>/<tool>`. Bytes the command wrote are verified against the row, but stage-sidecar
  does not delete a file the command itself wrote.

### Native libraries (`native-library`, and its alias `onnxruntime`)

```js
{ kind: "native-library", library: "pdfium", version: "chromium/8021", catalog: { crate: "rightkit-ffmpeg" } }
{ kind: "native-library", library: "onnxruntime", version: "1.24.4", catalog: ..., as?: "...", companions?: true }
{ kind: "native-library", library: "ggml", version: "0.20.2", catalog: ..., files?: ["libggml.0.20.2.dylib"], source?: { dir: "../retained-builds/ggml" } }
```

- **`library`** is a library name of `native-libraries.json` that is a dynamic library: `onnxruntime`, `directml`, `pdfium`, `whisper-cpp`,
  `ggml`, `sherpa-onnx`. `ffmpeg` has its own kind; `sqlite-vec` and `libgit2` are static crates, so they are refused with the route
  (depend on them through Cargo; their rows pin the crate checksum). **`version`** is any catalog version token (`1.24.4`,
  `chromium/8021`): letters, digits and `. _ + / -`; it is only compared with the rows and never used as a path.
- **Rows.** Every serving row of that library, version, os and arch is staged, each under its `artifactFileName` (ggml has several rows per
  os and arch). A row serves when it is `approved` or `pinned`; `planned` and `stale-unverified` rows never do. An exact (os, arch) row
  wins; a macOS `universal` row serves aarch64 and x86_64 targets only when no exact row serves. `files` restricts a multi-file library to
  the listed bare `artifactFileName` values; each must match exactly one serving row, and the order is the order staged. `as` renames
  the one staged file: it is invalid with a `files` list of other than one name (configuration), and refused at staging, typed
  (`STAGE_SIDECAR_CONFIG_INVALID`), when the library would stage more than one file. A companion keeps the name it is loaded by.
- **Companions.** A row's `companions` (catalog row ids; the Windows onnxruntime row names `directml-1.15.4-windows-x64`) are staged
  beside it into the same `resourceDir`, with their own notices in `licenseDir` and their own receipt row (`companionOf` is the parent row
  id). Each id must exist, serve the same os and arch, and be `approved` or `pinned` (else `STAGE_SIDECAR_NOT_APPROVED` naming the id),
  and may not list companions of its own. `companions: false` stages the library alone. The file names come from the catalog, so
  validation cannot claim them: before anything is written, staging plans every `onnxruntime` and `native-library` entry (catalog read once,
  no effects) and refuses, with `STAGE_SIDECAR_CONFIG_INVALID`, a file that another entry (a `resource`, `remote`, another native entry)
  also writes into the same directory. The message names both entries and, for a companion, the remedy: drop the other entry's copy,
  or set `companions: false` on the entry that stages the companion.
- **Bytes.** From the row's pinned archive: downloaded through `deps.fetchBytes` into `cacheDir` (default `.cache/native-library`; add it to
  `.gitignore`), digest checked before use, and the one member unpacked by `archive.member` (an archive path, which tells the same-named
  members of a nupkg apart: `runtimes/win-x64/native/onnxruntime.dll`, `bin/x64-win/DirectML.dll`, `bin/pdfium.dll`), else the bare
  `artifactFileName`. Or from `source: { dir }`, a directory (relative to the app root, may step out of it like the ffmpeg local
  source, or absolute) holding the shipped file under its `artifactFileName`; nothing is downloaded. `source` is required for a row with no
  archive (the macOS whisper.cpp, ggml and sherpa-onnx rows were measured from retained builds: without it the entry fails with
  `STAGE_SIDECAR_CONFIG_INVALID` naming `source.dir`); with an archive row it replaces the download.
- **Verification** (fail closed: every file of an entry is verified before any is written; `STAGE_SIDECAR_HASH_MISMATCH` with
  `details.expected` and `details.actual`). The archive's sha256 equals `archive.sha256`. A file extracted from it equals the row's
  whole-file `sha256` when the row has one AND its content digest when the row has `contentSha256` or `contentSha256ByArch`. A file
  read from `source.dir` is not byte-identical to the pinned bytes by construction (it may have been re-signed), so it must match the
  content digest when the row has one, else the whole-file `sha256`. The content digest is the signature-independent digest of
  `content-digest.mjs` (Authenticode rule for PE, canonical image for Mach-O); a universal row pins `contentSha256ByArch`
  (`{ aarch64, x86_64 }`) and every slice is checked. A thin row (`contentSha256`) takes a file with exactly one slice: a universal Mach-O,
  such as a good slice for the row's arch beside a foreign one, is refused (`STAGE_SIDECAR_HASH_MISMATCH`), from an archive, from
  `source.dir` and as an already-staged file. A malformed recognized format is refused, never hashed plainly. After the copy the staged
  bytes are held to the same row again (the source was read a second time for the copy): a file that changed after it was verified is
  removed and refused, never left staged. A row that pins no
  digest never serves. A file already in `resourceDir` is kept only when it passes the same check, never because it exists. The receipt's
  `sha256` is the whole-file hash of the staged file. The `onnxruntime` alias keeps its original check, the whole-file `sha256` of the
  extracted library (its rows must carry one).
- **Universal macOS.** One `universal` row is staged once and checked with `lipo -archs` (before it is written); per-arch rows are staged
  under `.right-release/slices/<triple>` and lipo-merged, file by file, with both arches naming the same files. A library whose rows
  are only aarch64 (whisper.cpp and ggml on macOS today) is refused with `STAGE_SIDECAR_NOT_APPROVED`, not guessed.
- **Receipt row** (`kind: "native-library"`): `library`, `name`, `path`, `sha256`, `sizeBytes`, `catalogRow`, `version`,
  `archiveSha256` (when an archive was used), `contentSha256` or `contentSha256ByArch` (when the row pins one), `license`, `notices`,
  `externalBin: false`, `hostProof` (when the row has it), `companionOf` (companions), and `slices` for a universal file.
- **`--check`** lists `resourceDir/<each file in files>`, `resourceDir/<as>`, or `resourceDir/<catalog artifactFileName>` (no catalog is
  read). The files named by `files` or `as` are claimed like any other output, so two entries (or a `resource`) cannot write the same
  file; a catalog-driven entry names nothing at validation time and claims nothing. `cacheDir` is a protected path.

**Rows never run on a host.** A row with `hostProof: "never-run"` (only legal on a row that is not `approved`: pinned or planned) has
digests that were measured from the upstream bytes, but nobody has loaded or run the file on a host of that OS. A pinned never-run row
stages and loads normally, because that is how host proof gets produced; the marker travels so nobody mistakes the file for proven.
Staging prints a warning naming the row (the `right-release stage-sidecar` command writes it to stderr, so `--json` stays clean on stdout;
a row of the library that is left out because it is not approved or pinned is reported the same way, as `not staging <row>`), and the
receipt row carries `hostProof: "never-run"` (the Windows onnxruntime, DirectML and PDFium rows today). An `approved` row that carries it, or any other value, is refused: approving a row after a host run removes the marker.

### Build output layout

The RustRight broker builds with an explicit `--target`, so output lands in `<targetDir>/<triple>/<profile>/`.
The executable path is taken from cargo's own `compiler-artifact` stream first. If the stream has none, the module reads
`cargo metadata` and looks in `<targetDir>/<triple>/<profile>/<bin>` then `<targetDir>/<profile>/<bin>` (`dev` and `test`
map to `debug`, `bench` to `release`, as in `swap-helper-bundle.mjs`). If neither exists it fails with
`STAGE_SIDECAR_BINARY_NOT_FOUND` and lists what it tried.

## Receipt

`<stageDir>/stage-sidecar.json` (contract `rightkit-stage-sidecar-v1`): `target`, `os`, `arch`, `stagedAt`, `skipped`, and
per file `kind`, `name`, `path` (app-relative), `sha256`, `sizeBytes`, `externalBin`, plus kind specific evidence
(`sourceSha256`, package, profile, features; catalog row id, version and digests; licence and notices with digests; `hostProof` and `companionOf` for catalog rows that have them).
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
- **native-library (DirectML, PDFium, whisper.cpp, ggml, sherpa-onnx)**: the same rule for every row, companions included: the
  DirectML licence terms and third-party notices ship with `DirectML.dll`, PDFium's BSD-3-Clause text and its third-party notices with
  the PDFium library, the MIT or Apache-2.0 text with whisper.cpp, ggml and sherpa-onnx. A row with no notice files, or a listed file that
  is missing, fails with `STAGE_SIDECAR_NOTICE_MISSING` before anything is written. `native-library` receipt rows are catalog-backed, so
  `license-notices binaries` needs no declaration for them (only `remote`, `command`, `cmake` and `prebuilt` rows do).
- Planned or unapproved rows (for example onnxruntime 1.24.4 on macOS, the Windows whisper.cpp, ggml and sherpa-onnx rows, or ffmpeg on
  Linux today) fail closed with `STAGE_SIDECAR_NOT_APPROVED` rather than staging an unverified binary.

## Errors

All failures are `StageSidecarError` with a stable `code`: `STAGE_SIDECAR_CONFIG_INVALID`, `_TARGET_UNSUPPORTED`,
`_TARGET_UNKNOWN`, `_BUILD_FAILED`, `_BINARY_NOT_FOUND`, `_HASH_MISMATCH` (`details.expected`/`actual`),
`_NOT_APPROVED`, `_NOTICE_MISSING`, `_CATALOG_UNREADABLE`, `_DOWNLOAD_FAILED`, `_EXTRACT_FAILED`, `_RESOURCE_MISSING`,
`_FFMPEG_FAILED`, `_NOT_UNIVERSAL`, `_RECEIPT_INVALID`. For `native-library`: `_CONFIG_INVALID` (bad keys, an unknown or static-crate
`library`, a bad `version` token, a `files` entry that is not a bare name, an empty `source.dir`, `as` with several files, a row with no
archive and no `source`), `_NOT_APPROVED` (no serving row, a planned or stale row, a companion that is missing, planned or for another
os or arch, a row with an inconsistent field or no digest), `_BINARY_NOT_FOUND` (a file missing from `source.dir`) and
`_HASH_MISMATCH` (archive, extracted file or source file against the row).

## API

`stageSidecars({ appRoot, manifest, target, deps })` returns `{ receipt, receiptPath, files, externalBinNames }`.
Other exports: `validateSidecarManifest`, `parseTargetTriple`, `resolveTargetTriple`, `builtBinaryCandidates`,
`validateStageSidecarReceipt`, `StageSidecarError`. `deps` (all optional) injects `fs`, `run(command, args, { cwd, env })`,
`fetchBytes`, `extractMember`, `loadMediaCatalog`, `loadNativeCatalog`, `makeTempDir`, `removeDir`, `now`, `pid`, `log`, `warn`, `env`.
The API is quiet by default: progress lines go to `log`, warnings (a never-run row, a row not staged) to `warn`, which falls back to
`log` when only `log` is given. The command passes `warn` and prints to stderr. Tests use fakes only (`stage-sidecar.test.mjs`), plus one
run of the command itself in a temporary app.

## CLI wiring (for release owner)

`right-release stage-sidecar [--target <triple>] [--app-root <dir>] [--config <file>]`: load the `sidecars` key from
`right-release.config.mjs`, resolve the target with `resolveTargetTriple({ target })` (`--target`, then
`TAURI_ENV_TARGET_TRIPLE`, then `rustright rustc -vV`), call `stageSidecars`, print the staged paths. Apps then call it
from `beforeBuildCommand` instead of their own scripts.

## Not covered

Signing and running
`tauri build`. The apps' dev-mode QA flags (such as mailright's `--test-mode`) map to `profile: "dev"` plus `features`
in a dev-specific manifest; selecting between manifests is the caller's job.

## Build profile, external catalogs, source builds and universal macOS (bogusyogi/rightkit#345)

**Profile switch.** A `cargo-bin` entry's `profile` is a Cargo profile name or `{ dev: "release-iterate", release: "release" }`
(exactly those two keys, both required). `RIGHTKIT_BUILD_MODE` (`dev` | `release`, default `release`, anything else refused)
selects. The wrapper form accepts a default for entries without their own: `{ sidecars: [...], profiles: { dev, release } }`.

**Catalog sources.** `ffmpeg`, `onnxruntime` and `native-library` `catalog` accepts a path relative to the app, an absolute path,
`{ crate: "rightkit-ffmpeg", file? }` (the crate directory comes from `rustright cargo metadata`, so a path or registry
dependency both work; `file` defaults to `catalog/artifacts.json` for ffmpeg and `catalog/native-libraries.json` for
onnxruntime and native-library) or `{ npm: "<pkg>/<file>" }` (found by walking up `node_modules` from the app, scoped packages allowed). A bad
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
(`lipo -archs` showing `arm64` and `x86_64`), and a `resource` with `machO: true` is checked the same way. `ffmpeg`, `onnxruntime`
and `native-library` stage each arch's catalog rows under `.right-release/slices/<triple>` and lipo-merge them (an `onnxruntime` or
`native-library` `universal` macOS row is staged once and checked with `lipo -archs`); a library with no row for an arch is refused.

## Placement, remote files, nested calls, target roots and `--check` (bogusyogi/rightkit#345)

**`dest`.** `cargo-bin`, `command` and `remote` entries accept `dest`, a contained app-relative path such as
`resources/coderight-engine/coderight.exe`. It replaces `<stageDir>/<name>-<triple>`; no `.exe` is added (write it in
`dest`), the file is not an externalBin (`externalBin: false` in the receipt, absent from `externalBinNames`; a `command`
with `dest` must omit `externalBin` or set it `false`), and executables get `0o755` on POSIX every run, even when the
file is unchanged. A `dest` or `to` needs at least one normal path segment (`.` and `./` are refused) and may not equal
or contain the app root, the stage directory, the receipt, `.right-release` (and the remote cache), an onnxruntime or
native-library `cacheDir`, `.git`, `src`, `src-tauri`, `node_modules` or `package.json`. Two entries may not write the same path, and
their outputs may not be nested inside each other (case-insensitive). `clean: true` (any entry kind with a destination)
removes a stale file before staging, and for a directory `resource` the stale directory, so a failed build never leaves
the old binary behind. A directory is removed recursively only for a directory `resource`; any other destination is removed
non-recursively. Just before the delete the destination's parent is resolved with `realpath`, and a parent that escapes the
app root through a symlink, or lands on a protected path, is refused (`STAGE_SIDECAR_CLEAN_REFUSED`). For a `remote` entry the
download is verified before the destination is cleaned. The wrapper key `wipeStageDir: true` removes, after staging, only
top-level regular files in `stageDir` named exactly `<name>-<current triple>[.exe]` for a cargo-bin, command or ffmpeg tool
name the manifest declares (including entries skipped on this platform or placed at `dest`) that this run did not produce. It
never recurses and never touches other triples, notices, temporaries or unrelated files. The receipt lists them under `wiped`. Opt-in only.

**Remote files (`kind: "remote"`).** `{ name, url, sha256, dest | to, size? }`. Only `https://` URLs are accepted
(validation refuses anything else, and a redirect to a non-https URL fails). The download goes through `deps.fetch`
(default global `fetch`). The bytes are hashed, and `size` checked when given, before anything is written: the verified
bytes land in the content-addressed cache `<app>/.right-release/cache/remote/<sha256>` (add it to `.gitignore`), and
`dest` is copied from the cache entry only. A digest or size mismatch is `STAGE_SIDECAR_HASH_MISMATCH`, an HTTP failure
`STAGE_SIDECAR_DOWNLOAD_FAILED`; nothing is cached or placed. A later run with a valid cache entry does not fetch; a
corrupt cache entry is re-downloaded. Committed files (for example the Windows `onnxruntime.dll` and `DirectML.dll`) use
`resource`, pointing `from` at the committed directory.

**Build profile per mode.** The a59df383 `profile: { dev, release }` (or wrapper `profiles`) covers "release for
`tauri build`, dev for `right-release dev`". `right-release dev` runs every build command with `RIGHTKIT_BUILD_MODE=dev`
(and `RIGHTKIT_CARGO_PROFILE=dev`) via `developmentEnvironment` in `development.mjs`; a plain `tauri build`, a recipe
build or CI leaves it unset, which means `release`. Nothing else is required of the app. A `beforeBuildCommand` that
runs under `right-release dev` inherits the dev mode. Set `RIGHTKIT_BUILD_MODE` yourself only outside those commands.

**Nested broker calls.** A `beforeBuildCommand` (or `beforeDevCommand`) that calls `stage-sidecar` runs inside the outer
broker build, and its `rightkit` build call exited 125. There is no way to reuse the outer lease: `acquireManagedRunLease`
records a per-process lease file under the managed data root and passes nothing to children (no RK_* run-id variable).
So `stage-sidecar` detects the context (Tauri's `TAURI_ENV_PLATFORM` or `TAURI_ENV_TARGET_TRIPLE` present, on a managed
host: `RIGHTKIT_BUILD_BROKER_SOCKET` or the agent-bin PATH, or `RIGHTSUITE_HEAVY_WORK_OWNER=1`) and, when a non-skipped
`cargo-bin` (or a `{ crate }` catalog) would call the broker, fails fast with `STAGE_SIDECAR_NESTED_BROKER`: run
stage-sidecar in recipe `prepare`, before the Tauri build. Manifests with only `resource`, `remote`, `command` and path
catalogs are unaffected. `RIGHTKIT_STAGE_SIDECAR_ALLOW_NESTED=1` skips the check.

**Managed target roots.** The build output directory is resolved through `rustright cargo metadata --format-version 1
--no-deps` (`target_directory`, which cargo derives from `CARGO_TARGET_DIR` or the broker's managed root) when the
build's own artifact stream names no executable. Candidates are, in order: the explicit `CARGO_TARGET_DIR` of the calling
environment (resolved against the app root) and then `target_directory`, each as `<root>/<triple>/<profile>/<bin>` then
`<root>/<profile>/<bin>`. A failed metadata query is only fatal when `CARGO_TARGET_DIR` is also unset.

**`right-release stage-sidecar --check`.** Validates the manifest (all errors at once, exit 1) and prints the plan: target,
build mode, stageDir, and per entry the output paths, externalBin, clean and url. It builds, downloads, copies and
deletes nothing and spawns no process; without `--target` it uses `TAURI_ENV_TARGET_TRIPLE` or the Node host triple.
`--json` prints the plan as JSON. An app's template check can call it to gate the manifest shape.
