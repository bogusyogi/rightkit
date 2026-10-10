# RightKit crate audit, group 3 (read-only)

Scope: `crates/` rightkit-oauth, -ocr, -ort, -overlay, -ownership, -pdf, -playback, -process, -qa, -qts, -qts-ggml, -qts-ggml-sys, -record, -review.
Method: Read/Grep/`wc -l` only; no cargo run. Usage counts are grep hits over `src/` (+ `build.rs`), comments excluded where noted. "pub API" counts are `pub fn|struct|enum|trait` lines anywhere under `src/` (includes pub items inside non-re-exported modules and impl methods, so it is an upper bound). Version/duplication facts come from `/Volumes/D/claude/rightkit/Cargo.lock`.

**rightkit-ocr is audited from the working tree** (uncommitted 0.2.0 bump: `Cargo.toml`, `CHANGELOG.md`, `README.md`, `src/lib.rs`, `src/router.rs`, `src/bin/rightkit-ocr.rs` modified; `tests/lease_and_provenance.rs` untracked; adds `ModelLeaseHook`/`lease_hook` in `src/router.rs`).

Workspace-wide version splits relevant to this group (Cargo.lock): sha2 0.10.9 + 0.11.0; getrandom 0.2/0.3/0.4; thiserror 1/2; windows 0.61/0.62; windows-sys 0.45/0.52/0.59/0.60/0.61; toml 0.8/0.9/1.1; prost 0.13.5 + 0.14.4; rand 0.8/0.9/0.10; png 0.17 (via image) + 0.18 (qa); lopdf 0.42 (via pdf-extract) + 0.44.

Verdict tally (production + build deps; dev deps listed separately):

| crate | needed | unused | feature-gate-it | replace-with | heavy | dev (needed) |
|---|---|---|---|---|---|---|
| oauth | 10 | 0 | 1 | 2 | 2 | 2 |
| ocr | 11 | 0 | 0 | 0 | 0 | 1 |
| ort | 3 | 0 | 0 | 0 | 0 | 2 |
| overlay | 2 | 0 | 0 | 0 | 0 | 0 |
| ownership | 9 | 0 | 0 | 2 | 0 | 1 |
| pdf | 8 | 0 | 1 | 0 | 1 | 2 |
| playback | 2 | 0 | 0 | 2 | 1 | 0 |
| process | 2 | 0 | 0 | 0 | 0 | 1 |
| qa | 10 | 0 | 1 | 4 | 0 | 0 |
| qts | 6 | 0 | 0 | 3 | 0 | 0 |
| qts-ggml | 1 | 0 | 0 | 0 | 0 | 0 |
| qts-ggml-sys | 2 | 0 | 0 | 0 | 0 | 0 |
| record | 9 | 0 | 0 | 0 | 0 | 0 |
| review | 2 | 0 | 0 | 0 | 0 | 0 |

No dependency is outright unused. The two cargo-machete hints on qts and the two on qts-ggml-sys are false positives (build scripts / cfg-gated code), but two of them (qts prost-build, protoc-bin-vendored) are still removable by design (see qts).

---

## rightkit-oauth

### Profile
- Purpose: loopback PKCE + RFC 8628 device flow, provider presets, OS-credential-store token refresh, and a framework-agnostic daemon `server` flow with AES-256-GCM file persistence.
- Version 0.3.0; `publish` not set (publishable); license MIT OR Apache-2.0.
- src LOC (2809): `lib.rs` 768; `server/` 1252 (`mod.rs` 656, `store.rs` 306, `provider.rs` 217, `sensitive.rs` 73); `credentials.rs` 304; `device.rs` 292; `system.rs` 120; `token_codec.rs` 73. Tests 1062+377+270+28; example `server_axum.rs` 62.
- Pub API: 49 fn, 20 struct, 8 enum, 7 trait.
- Features: default = `secrets`, `system-sources`. `secrets` (dep rightkit-secrets); `system-sources` (dep rightkit-process); `server` (base64, ring, subtle, url, zeroize, serde, libc); `server-example` (server + axum + tokio).
- Workspace dependents: `rightkit-daemon` (`default-features=false, features=["server"]`, both dep and dev-dep, `crates/rightkit-daemon/Cargo.toml:18,36`). No npm package depends on it.

### Dependencies
| dep | spec | optional | where used (src hits) | verdict | reason |
|---|---|---|---|---|---|
| rightkit-http | path 0.2.0 | no | lib/device/credentials/server (8) | needed | HTTP client, `Secret`, percent-encode |
| rightkit-secrets | path 0.1.0 | `secrets` | `credentials.rs:26,30` | needed | credential store |
| rightkit-process | path 0.3.0 | `system-sources` | `system.rs:11` OwnedCommand | needed | runs `gh`/git credential helper |
| getrandom | 0.3 | no | `lib.rs:744`, `server/mod.rs:633`, `store.rs:178` (3) | needed | state/verifier/nonce |
| serde_json | 1.0.150 | no | 20 | needed | token parsing |
| sha2 | 0.10.9 | no | `lib.rs:385` PKCE S256; `server/mod.rs:337,637` (2) | needed | PKCE challenge. Note: with `server` on, `ring::digest` is also linked; sha2 is 0.10 while fs/cache/models/pdf use 0.11 |
| base64 | 0.22 | `server` | `server/mod.rs:10,555` (2; 1 call, STANDARD for Basic auth) | replace-with | in-crate `base64url` hand-rolled at `lib.rs:749-768`; extend that helper (std alphabet) instead of a second implementation |
| ring | 0.17 | `server` | `server/store.rs:5` AEAD | needed | AES-256-GCM; C/asm build but rightkit-daemon already links ring |
| subtle | 2.6 | `server` | `server/mod.rs:18,416-430` (4 `ct_eq`) | replace-with | `ring::constant_time::verify_slices_are_equal` (ring is already required by `server`) |
| url | 2.5 | `server` | `server/provider.rs:159,205` (2 `Url::parse`) | needed | security validation of redirect/endpoint URLs (scheme, userinfo, query, fragment); shared by daemon/browser/mcp. Pulls idna, but correctness-critical |
| zeroize | 1.8 | `server` | 19 | needed | secret wiping |
| serde | 1.0.228 derive | `server` | `server/mod.rs:96,194` | needed | pending-state (de)serialization |
| libc | 0.2 | `server` | `server/store.rs:238,270,303` all under `#[cfg(unix)]` | feature-gate-it | move to `[target.'cfg(unix)'.dependencies]`; currently compiled (unused) on Windows |
| axum | 0.8 | `server-example` | only `examples/server_axum.rs:5` | heavy | gated, but it only serves one example; make it a dev-dependency with `required-features` so it is not part of the published feature surface |
| tokio | 1 (rt-multi-thread, macros, net) | `server-example` | only the example (`:40`) | heavy | same as axum |
| (dev) tempfile | 3 | | tests (12) | needed | |
| (dev) rightkit-http `testing` | path | | tests (9) | needed | mock server |

### Overlap / split notes
- Crypto/encoding duplicated between oauth `server`, `rightkit-daemon/src/util.rs` (own hex, `ring::digest` SHA-256, `subtle` ct-eq at `:60,:64`), and `rightkit-secrets` (base64, zeroize). Three crates each hold their own small hex/base64/ct-eq helpers. A single `rightkit-secrets` (or http) helper set would remove them.
- The crate has two responsibilities with disjoint dependency sets: client sign-in (`lib`, `device`, `credentials`, `system`) and the daemon `server` module + `SealedFileTokenStore` (ring, zeroize, subtle, url, libc). Already feature-isolated, so a split is optional; only worthwhile if `server` grows. Not a merge candidate.

---

## rightkit-ocr (working tree, uncommitted 0.2.0)

### Profile
- Purpose: product-neutral OCR: Apple Vision / Windows.Media.Ocr native backends with per-line geometry, isolated worker process, and a digest-verified two-stage ONNX fallback.
- Version 0.2.0 in the working tree (committed value not checked); `publish = true`; MIT OR Apache-2.0.
- src LOC (2598): `lib.rs` 622; `native/` 604 (`vision.rs` 374, `win_native.rs` 168, `mod.rs` 62); `router.rs` 363; `worker.rs` 297; `support/` 448 (`boxes.rs` 234, `session.rs` 62, `preprocess.rs` 61, `decode.rs` 50, `model_files.rs` 26, `support.rs` 15); `tests.rs` 110; `types.rs` 84; `bin/rightkit-ocr.rs` 70.
- Pub API: 43 fn, 12 struct, 5 enum, 1 trait (`ModelLeaseHook`, new in the working tree).
- Features: default = `native`, `onnx`. `native` = objc2-* (macOS) + windows (Windows); `onnx` = rightkit-ort + sha2.
- Workspace dependents: none (apps consume the published crate).

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| image | 0.25 (png, jpeg; no defaults) | no | 25 (types, router, native/*, support/*) | needed | PNG encode `router.rs:360`, decode; features minimal |
| rightkit-ort | path 0.2.2 | `onnx` | `lib.rs:39-43`, `router.rs:14`, `support/session.rs:1-2` | needed | runtime config + `ProviderDiagnostic` |
| sha2 | 0.10 | `onnx` | `lib.rs:45,279` (`verify_artifact`) | needed | artifact digest pin. Note it reads whole file into memory (`lib.rs:271-286`); `rightkit_fs::digest::sha256_reader` (used by rightkit-models) would stream |
| serde | 1 derive | no | types/router/worker (9) | needed | |
| serde_json | 1 | no | `worker.rs:140,145,224`, bin `:67` | needed | worker protocol |
| objc2 | 0.6 | `native` (macOS) | `native/vision.rs` | needed | |
| objc2-foundation | 0.3 (6 features) | `native` (macOS) | `vision.rs:33` | needed | |
| objc2-core-graphics | 0.3 | `native` (macOS) | `vision.rs:30` | needed | |
| objc2-vision | 0.3 (no defaults) | `native` (macOS) | `vision.rs:34` | needed | |
| objc2-core-foundation | 0.3 | `native` (macOS) | `vision.rs:29` | needed | |
| windows | 0.62 (7 features) | `native` (Windows) | `win_native.rs:26-30` (HSTRING, Language, SoftwareBitmap, Ocr, CryptographicBuffer) | needed | all 7 feature modules map to those imports |
| (dev) base64 | 0.22 | | tests (7) | needed | fixture PNG decode |

### Overlap / split notes
- **ONNX session setup (ocr vs ort vs asr).** `src/support/session.rs:9-62` hand-rolls "DirectML first, else CPU" and builds `ProviderDiagnostic` entries itself, although `rightkit_ort::SessionOptions::new(ExecutionProvider::platform_accelerator()).with_cpu_fallback(true).build_from_file_reported()` (`rightkit-ort/src/session.rs:95-146`) does exactly this and records the fallback reason. Gap that blocks the swap: ocr's DirectML path sets `with_parallel_execution(false)` and `with_memory_pattern(false)` (`session.rs:51-55`), which rightkit-ort's DirectML path does not (it sets independent thread pool / optional `ep.dml.disable_graph_fusion`, `ort/src/session.rs:178-203`). Add those two knobs to `SessionOptions`, then migrate ocr. Same story in `rightkit-asr` (12 `Session::builder()` sites in 10 files plus its own 10-variant `ExecutionProvider` in `execution.rs:181-237`), `rightkit-vad/src/silero.rs`, and `rightkit-qts/src/pipeline/vocoder.rs:781` (own `VocoderExecutionProvider`). Only `rightkit-embed/src/embedder.rs:187` uses `SessionOptions`.
- **Subprocess lifecycle.** `worker.rs:15,189-277` runs the worker with plain `std::process::Command`, `child.kill()` + `wait()`, capped reader threads. `child.kill()` does not reap descendants; `rightkit_process::OwnedCommand` (`wait_or_kill`, `terminate_tree`, Windows Job Object) is the suite primitive. `rightkit-ownership/src/runner.rs` has near-identical `kill_and_reap`/deadline/reader code (see ownership). Candidate: one `rightkit_process::run_bounded` (deadline + output caps + tree kill).
- **Model lease.** `ModelLeaseHook` (`router.rs`, new) is an app-callback shim; the lease owner is `rightkit-models/src/suite/lease.rs`. Keep ocr dependency-free (as designed), but the trait could live in rightkit-models.
- **Split?** Two backends (native FFI vs ONNX) plus worker already feature-gated; no split. `support/` (boxes, ctc decode, preprocess, 448 LOC) is PaddleOCR-specific and fine to stay.

---

## rightkit-ort

### Profile
- Purpose: product-neutral ONNX Runtime dynamic-library resolution, environment (shared thread pool), execution-provider probe and session builder; single suite `ort` pin `=2.0.0-rc.13`.
- Version 0.2.2; publishable; MIT OR Apache-2.0.
- src LOC (1666): `lib.rs` 791 (runtime resolve/configure); `environment.rs` 541; `session.rs` 250; `probe.rs` 84.
- Pub API: 34 fn, 13 struct, 7 enum, 0 trait.
- Features: default = `session` (pulls `ort`). Optional: `coreml`, `directml`, `half` (re-exports half), `ndarray` (re-exports ndarray), and 21 pure pass-through ort features (`tracing`, `download-binaries`, `tls-native`, `copy-dylibs`, `preload-dylibs`, `acl`, `armnn`, `azure`, `cann`, `cuda`, `migraphx`, `nnapi`, `nvrtx`, `onednn`, `openvino`, `qnn`, `rknpu`, `tensorrt`, `tvm`, `vitis`, `webgpu`, `xnnpack`).
- Workspace dependents: rightkit-qts, -ocr, -embed, -vad, -tts, -asr.

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| ort | `=2.0.0-rc.13`, load-dynamic, api-24; declared 3x (base, apple+coreml, windows+directml) | `session` | 31 | needed | suite pin |
| half | 2 | `half` | `lib.rs` re-export (2) | needed | f16 re-export for ASR |
| ndarray | 0.17 | `ndarray` | re-export (2) | needed | |
| windows-sys | 0.61.2 (`Win32_System_SystemInformation`) | Windows target (non-optional) | `environment.rs` core-record count (1) | needed | physical-core budget |
| (dev) rightkit-media | path 0.2.3 | | `tests/e2e_runtime.rs` (1) | needed | catalog hash check |
| (dev) sha2 | 0.10 | | tests (1) | needed | |

### Overlap / split notes
- See ocr, asr, qts above: `SessionOptions`/`ExecutionProvider` exists but is bypassed by asr, ocr, vad, qts. The 21 pass-through features exist only so "no crate declares ort"; consumers actually forward: asr (cuda, tensorrt, coreml, directml, migraphx, openvino, webgpu, nnapi, tracing, download-binaries, tls-native, copy-dylibs, preload-dylibs), vad (tracing, download-binaries, tls-native, copy-dylibs), qts (all 19 EP names), tts none. Cost is manifest noise, not build cost.
- `environment.rs` (shared pool, thread budget) and `lib.rs` (runtime file resolution + catalog discovery) are two independent concerns (process-wide pool vs library location); a `rightkit-ort` split is not justified by size.

---

## rightkit-overlay

### Profile
- Purpose: shared overlay state, hover gating, hit testing, work-area placement and Windows TOPMOST/NOACTIVATE repair for Right Suite pills/notches.
- Version 0.1.0; `publish` not set; no `rust-version`; no LICENSE files, no CHANGELOG; `tests/` is empty.
- src LOC: `lib.rs` 306 (single file; Windows module at `:79-250`, non-Windows stubs at `:251-258`).
- Pub API: 21 fn, 4 struct, 3 enum.
- Features: none.
- Workspace dependents: none (apps outside this repo).

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| serde | workspace (derive) | no | `lib.rs:7,16,20,24` | needed | serializable state |
| windows-sys | 0.61.2 (Foundation, UI_WindowsAndMessaging) | Windows target | `lib.rs:83-85` | needed | matches ort/crash/logs/hotkeys (the lighter family) |

### Overlap / split notes
- Product names inside a "shared" crate: `OverlayKind::{HeardRightPill, CockpitNotch}` (`lib.rs:17`), `HR_STATUS_WINDOW_LEVEL` (`:9`). Product-neutral naming would match the repo rule.
- No tests (`tests/` empty), no license files, no `rust-version`: lower bar than sibling crates. Not publish-hardened.
- Possible overlap with `rightkit-hotkeys`/`rightkit-tauri` window-style code was not inspected (unverified).

---

## rightkit-ownership

### Profile
- Purpose: Rust implementation of the ownership scanner, verdict and admission (the RustRight broker links it).
- Version 0.2.1; `publish = true`; MIT OR Apache-2.0.
- src LOC (11838): `scan.rs` 1429; `approval.rs` 1272; `lockfiles.rs` 1154; `deps.rs` 1059; `tests.rs` 897 (test module); `admission.rs` 627; `committed.rs` 560; `verdict.rs` 532; `evidence.rs` 517; `inventory.rs` 499; `keylog.rs` 462; `trust.rs` 407; `runner.rs` 391; `source.rs` 332; `worker.rs` 314; `keyauth.rs` 313; `json.rs` 272; `limits.rs` 216; `gitio.rs` 192; `paths.rs` 161; `lib.rs` 90; `error.rs` 52; `types.rs` 46; `clock.rs` 44. Also `catalog/**` data and `tests/ownership_fixtures.rs` 745.
- Pub API: 138 fn, 45 struct, 3 enum, 2 trait.
- Features: none.
- Workspace dependents: none in `crates/`, `desktop-app-template`, `packages/*/package.json` (packages/release only reads the JSON catalog). Consumer is the external broker.

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| serde_json | 1 (preserve_order) | no | 44 (17 files) | needed | |
| sha2 | 0.10 | no | approval/committed/json/keylog (6) | needed | on 0.10 while pdf/fs/cache/models use 0.11 |
| hex | 0.4 | no | `committed.rs:124,129`, `json.rs:211` (3 `hex::encode`) | replace-with | `format!("{:x}", digest)` (digest output implements LowerHex); drops a crate |
| url | 2 | no | `approval.rs:9,36,704` | needed | WHATWG canonical identity |
| sha1 | 0.10 | no | `committed.rs:9,126` | needed | git object ids |
| fancy-regex | 0.19 | no | deps/source/trust | needed | lookahead `deps.rs:982`, catalog-supplied patterns `source.rs:57` |
| toml | 0.9 (parse, serde, std, preserve_order) | no | `lockfiles.rs:358` | needed | Cargo.lock parse. Third toml major in workspace (qa uses 0.8) |
| p256 | 0.13 (ecdsa, pem, std) | no | approval/keylog (8) | needed | key-log signatures |
| rsa | 0.9 (std, pem, sha2) | no | approval/keylog (8) | needed | RSA-2048 verification `keylog.rs:54,80-83`; heavy (bigint) but required |
| base64 | 0.22 | no | approval/keylog (7) | needed | |
| spki | 0.7 (pem, std) | no | `keylog.rs:14` (1 import, `EncodePublicKey`) | replace-with | `rsa::pkcs8::spki` / `p256::pkcs8` re-exports (both already enabled via pem). Not build-verified |
| (dev) tempfile | 3 | | `runner.rs:312,378` (test mod), `tests.rs` | needed | |

### Overlap / split notes
- `runner.rs:93-217` (Node scan worker: `Command`, deadline wait, capped readers, `kill_and_reap` at `:161`) is the same shape as `rightkit-ocr/src/worker.rs`. `gitio.rs:85` is a third raw `Command::new`. Neither uses `rightkit-process`. Same `run_bounded` recommendation as ocr.
- Crypto helpers overlap `rightkit-license` (ed25519, base64) conceptually (signature verification), but algorithms differ (P-256/RSA vs Ed25519); no merge.
- Candidate split: the crate mixes the scanner (`scan`, `deps`, `lockfiles`, `source`, `inventory`, `committed`, ~5.5k LOC, needs fancy-regex, toml, sha1) with the signed-trust stack (`approval`, `keylog`, `keyauth`, `trust`, ~2.5k LOC, needs p256, rsa, spki, url). Two independent responsibilities with disjoint heavy deps; splitting `rightkit-ownership-trust` would let non-broker consumers avoid rsa/p256. Low urgency since there is no workspace dependent.

---

## rightkit-pdf

### Profile
- Purpose: product-neutral PDF engine: inspect, compose (source-text replace / add text / OCR correction), font subsetting, staged atomic save, optional merge/select and text extraction.
- Version 0.2.0; publishable; MIT OR Apache-2.0.
- src LOC (7336): `compose.rs` 1926; `inspect.rs` 1842; `model.rs` 1338; `save.rs` 789; `font.rs` 768; `test_fixtures.rs` 381; `merge.rs` 162; `commit.rs` 63; `lib.rs` 35; `extract.rs` 32. `assets/SourceSerif4-Variable.woff2` 52 KB.
- Pub API: 41 fn, 31 struct, 9 enum, 2 trait.
- Features: default = `merge`, `edit`, `fonts`. `fonts` = fontdb, ttf-parser, subsetter, wuff; `edit` = fonts + getrandom (compose/save/commit); `merge` = no deps; `extract` (off by default) = pdf-extract.
- Workspace dependents: none.

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| lopdf | 0.44.0 (no defaults, `rayon`) | no | 17 | needed | core parser; `rayon` feature pulls rayon |
| thiserror | 2.0.20 | no | `commit.rs:13` only (`#[derive(thiserror::Error)]`, `edit` module) | feature-gate-it | unconditional but one use in an `edit`-only module; gate on `edit` or write the 3-variant `Display` by hand |
| fontdb | 0.24.0 | `fonts` | `font.rs` (16) | needed | |
| ttf-parser | 0.25.1 | `fonts` | `font.rs` (18) | needed | |
| subsetter | 0.2.6 | `fonts` | `font.rs:431,442` | needed | |
| wuff | 0.2.8 | `fonts` | `font.rs:322` | needed | WOFF2 decode of the bundled font |
| serde | workspace derive | no | model.rs etc. | needed | |
| sha2 | 0.11.0 | no | `model.rs:769` (`sha256_hex`, unconditional) | needed | on 0.11 (fs/cache/models) vs 0.10 elsewhere |
| getrandom | 0.4.3 | `edit` | `commit.rs:33`, `save.rs:236` | needed | token/nonce; 0.4 vs 0.3 used by oauth/http/mcp/secrets/models |
| pdf-extract | 0.12 | `extract` | `extract.rs:15-30` | heavy | pulls its own `lopdf 0.42.0` (Cargo.lock) next to 0.44, plus encoding_rs, cff-parser, etc. Off by default but locked |
| (dev) serde_json | workspace | | `model.rs` tests (14) | needed | |
| (dev) image | 0.25 (png) | | `tests/corpus_qa.rs` (1) | needed | |

### Overlap / split notes
- Text extraction exists twice: `extract.rs` (pdf-extract) and `inspect.rs` (own page-graph/text walker, 1842 LOC). If `inspect` can serve the callers of `extract`, the second lopdf (0.42) and its tree disappear.
- `inspect.rs` (read-only, 1842) is not feature-gated while `compose/save/commit/font` are; the crate is cohesive via features, no split needed.
- Overlaps with `rightkit-ocr` only through the "OCR correction" compose path (data in, no code dependency).

---

## rightkit-playback

### Profile
- Purpose: source-preserving FFmpeg compatibility remux/AAC derivative preparation with fingerprint cache and cancellable requests.
- Version 0.1.1; publishable; MIT OR Apache-2.0.
- src LOC (496): `lib.rs` 496 (Tauri command wrappers at `:432-496`). Tests: `remux_e2e.rs` 176, `cancel_race.rs` 43.
- Pub API: 16 fn, 5 struct, 2 enum, 1 trait.
- Features: default = none; `tauri` (dep tauri, `default-features=false`).
- Workspace dependents: none.

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| rightkit-media | path 0.2.3 | no | `lib.rs:24` (`CancellationToken`) | needed | light (fs digest, process, serde) |
| rightkit-process | path 0.3.0 | no | `lib.rs:25` OwnedCommand, WaitOutcome | needed | |
| parking_lot | `=0.12.5` | no | `lib.rs:21` (`Mutex`, 1 use) | replace-with | `std::sync::Mutex`; this is the only parking_lot user in the workspace (grep of every `Cargo.toml`) |
| sha2 | `=0.10.9` | no | `lib.rs:22,241-258` (1 fingerprint fn) | replace-with | `rightkit_fs::digest` (already linked through rightkit-media), which unifies on sha2 0.11 and drops the exact pin |
| tauri | 2 (no defaults) | `tauri` | `lib.rs:432-496` (3 commands) | heavy | whole Tauri for 3 `#[tauri::command]` wrappers; `rightkit-tauri` crate exists as the natural home |

### Overlap / split notes
- **Merge candidate into `rightkit-media`.** `FfmpegCandidates::resolve` (`lib.rs:403-430`) duplicates ffmpeg binary resolution in `rightkit-media/src/{dev_resolve,native}.rs`; `compatibility_args`/`compatibility_copy` (`:261-340`) build FFmpeg invocations outside `rightkit-media::{command,runner}` (`FfmpegCommand`, `run_capture`). The crate already depends on media and process, is 496 lines, and has no other dependents. Fold into rightkit-media behind a `playback` feature; move Tauri command glue to rightkit-tauri.
- Cache: fingerprint + eviction (`:140-260`) overlaps `rightkit-cache::DiskCache` (size-capped LRU, atomic writes). Not inspected deeply; worth checking.

---

## rightkit-process

### Profile
- Purpose: ownership-safe child lifecycle (Job Object / process group), restart, health, stall-watchdog and stderr-tail primitives.
- Version 0.3.3; publishable; MIT OR Apache-2.0. No non-platform dependencies.
- src LOC (3173): `owned.rs` 937; `spawn_windows.rs` 857; `process_handle.rs` 299; `stall.rs` 236; `restart.rs` 227; `process_windows.rs` 191; `stderr_tail.rs` 106; `process_unix.rs` 102; `health.rs` 95; `windows_job.rs` 90; `lib.rs` 33. Tests ~1,700 across 9 files incl. trybuild.
- Pub API: 80 fn, 22 struct, 10 enum, 0 trait.
- Features: none.
- Workspace dependents (11 + template): rightkit-shell (+example-app), -playback, -browser, -sandbox, -oauth, -jobs, -record (macOS), -media, -tts, -qa, `desktop-app-template/src-tauri`.

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| libc | 0.2 (unix target) | no | `owned.rs` (14), `process_unix.rs` (fd/close_range), `process_handle.rs` | needed | |
| windows | 0.62 (Foundation, Security, ToolHelp, JobObjects, Threading) | Windows target | `owned.rs`, `process_handle.rs`, `process_windows.rs`, `spawn_windows.rs`, `windows_job.rs` | needed | narrow, each feature used (ToolHelp = thread snapshot `process_windows.rs:13-14`) |
| (dev) trybuild | `=1.0.116` | | `tests/api.rs`, `tests/ui` | needed | compile-fail contract |

### Overlap / split notes
- **process vs sandbox vs service.** `rightkit-sandbox/src/windows_job.rs` is a 5-line re-export of process's `WindowsJob` (good). But `rightkit-sandbox/src/process_tree.rs` (228, unix-only enumerate + bounded kill, `proc_listallpids`) is a second tree-kill next to `OwnedChild::terminate_tree*`, and `rightkit-qa` has `kill_tree` plus two pid-liveness probes (see qa). `AdoptedProcess::is_running` (`owned.rs:632-700`) is the shared primitive. Unix tree enumeration should move into process. `rightkit-service` is IPC/discovery (no overlap).
- Split candidate (low priority): `health.rs`+`restart.rs`+`stall.rs` (~560 LOC, platform-neutral supervision policy; consumers `rightkit-jobs/src/supervisor.rs`, `desktop-app-template/.../engine_supervisor.rs`, `rightkit-record/src/macos/mod.rs`) vs spawn/containment (~2,600 LOC platform FFI). Only worthwhile if supervision wants a dependency-free crate; both halves share one tiny dep set so the saving is nil.
- Missing primitive: a "run with deadline and capped output" runner; reimplemented in ocr/worker.rs, ownership/runner.rs, qa/process.rs `run_owned`, and rightkit-media/runner.rs.

---

## rightkit-qa

### Profile
- Purpose: Rust QA harness: engine black-box scenarios, hidden native UI driving via rightkit-control, scenario wrapper, paid-provider HTTP mocks, loudness/WAV/PNG/frame validators, run lock, orphan sweep, hashed evidence.
- Version 0.2.14; publishable; MIT OR Apache-2.0; lib `rightkit_qa` + bin `rightkit-qa`.
- src LOC (10273): `control.rs` 1191; `storage.rs` 853; `scenario.rs` 826; `media.rs` 791; `mock.rs` 747; `harness.rs` 682; `suite.rs` 562; `check.rs` 444; `page.rs` 417; `loudness.rs` 414; `process.rs` 408; `engine.rs` 371; `workspace.rs` 360; `settings.rs` 335; `browser.rs` 300 (feature); `config.rs` 280; `evidence.rs` 257; `guard.rs` 251; `bin/rightkit-qa.rs` 244; `lock.rs` 233; `http.rs` 113; `util.rs` 94; `audio.rs` 54; `lib.rs` 46.
- Pub API: 242 fn, 57 struct, 5 enum, 1 trait (largest surface in the group).
- Features: default none; `browser` = rightkit-browser + tokio.
- Workspace dependents: `rightkit-shell/example-app` (dev-dep, `Cargo.toml:22`), `rightkit-models` (dev-dep, only for its mock server, `rightkit-models/Cargo.toml:43`). (The `@rightkit/qa` npm package is a different artifact the crate replaces.)

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| rightkit-fs | 0.3 (digest only) | no | `util.rs:51` (`sha256_bytes`) | needed | |
| rightkit-control | 0.1.8 path, **default features** | no | `control.rs:162` (`mac::frontmost_name`), `:221` (`mac::frontmost_pid`); `guard.rs` strings only | feature-gate-it | only macOS-only fns are used; default `wv2-0_38` drags `windows 0.61` + `webview2-com 0.38` onto Windows builds (rightkit-control/Cargo.toml). Make it `[target.'cfg(target_os="macos")']` with `default-features=false` |
| rightkit-framed-sidecar | path | no | `engine.rs` | needed | |
| rightkit-service | path | no | `control.rs:15,154` | needed | |
| rightkit-process | path 0.3.0 | no | `process.rs` OwnedCommand | needed | |
| rightkit-browser | 0.2.2 | `browser` | `browser.rs` | needed | |
| tokio | 1 (rt, time) | `browser` | `browser.rs` | needed | |
| png | 0.18 | no | `media.rs` decode/encode (30) | needed | second png major (image uses 0.17) |
| serde | 1.0.228 derive | no | 48 | needed | |
| serde_json | 1.0.150 | no | 48 | needed | |
| libc | 0.2 | no | `storage.rs:668-669` only (`libc::kill(pid,0)`, unix) | replace-with | `rightkit_process::AdoptedProcess::new(pid).is_running()` (`owned.rs:632`); also unconditional dep for a unix-only call |
| time | 0.3 (formatting) | no | `util.rs:38-39` (RFC3339 now) | replace-with | 1 call; trivial civil-date formatter from `SystemTime` |
| toml | 0.8 | no | config/scenario (14) | needed | toml 0.8 vs ownership 0.9 vs 1.1 elsewhere: align |
| uuid | 1 (v4) | no | `util.rs:55` (1 `Uuid::new_v4().to_string()`) | replace-with | `getrandom` hex; uuid pulls getrandom anyway |
| windows | 0.62 (Foundation, UI_WindowsAndMessaging) | Windows target | `control.rs:166-` (`GetForegroundWindow`, `GetWindowThreadProcessId`) | replace-with | `windows-sys 0.61` (overlay/ort/crash/logs/hotkeys already use it); 2 calls |

### Overlap / split notes
- **Pid liveness x2 and tree-kill x3.** `process.rs:13-35 is_alive` shells out to `kill -0`/`tasklist.exe`; `storage.rs:662-` `pid_live` re-implements it with `libc::kill` / `tasklist`; `rightkit-process` has `AdoptedProcess::is_running` and `terminate_tree`; `rightkit-sandbox::process_tree` is a third. Collapse to process's.
- **Mock HTTP x2.** `mock.rs` (747) + `http.rs` (113, hand-rolled blocking client) vs `rightkit-http/src/testing.rs` (in-process mock server with `RecordedRequest`). `rightkit-models` dev-depends on rightkit-qa solely for its mock (Range, `cut_after`). Move the richer mock to one place (rightkit-http `testing`), then rightkit-models stops depending on the whole QA harness.
- **Media probes/validators.** `media.rs` (WAV parser `parse_wav` `:181`, `wav_info`, `ffprobe` `:104`, PNG decode `:565`, frame stats `:418-514`) and `loudness.rs` (BS.1770, 414) vs `rightkit-media` (`probe.rs`, `quickprobe.rs` pure-Rust WAV/MP4 duration, `loudnorm.rs`, tool resolution in `dev_resolve.rs`/`native.rs`) vs `rightkit-record/src/media.rs:263 probe_mp4` (471). Three MP4/WAV parsers and two ffmpeg-tool locators (`media.rs:645 locate_tool`). `review::FrameStat` is the data contract that qa's `pixel_stats`/`frames_report` could feed, not a duplicate.
- **qa vs control vs record vs playback (screenshots/video/frame validators).** qa owns validators (data in, verdict out); control owns capture/offscreen (`rightkit-control/src/offscreen.rs`); record owns capture bundles + MP4 probe; playback owns remux. No screenshot capture is duplicated in qa itself; the overlap is the media parse/probe layer above.
- Split candidate: qa bundles two independent responsibilities: (a) QA orchestration (suite, scenario, harness, check, evidence, storage, lock, workspace, settings) and (b) a media/audio validator library (`media`, `loudness`, `audio`, ~1.3k LOC, only needs png + serde_json + sha256). The validators have no dependence on rightkit-control/service; extracting them (or folding into rightkit-media) lets rightkit-models/others use them without the harness.
- `guard.rs:198` embeds a Tauri-plugin snippet as text (not a dependency); fine.

---

## rightkit-qts

### Profile
- Purpose: Qwen3-TTS inference (GGUF + ggml transformer, ONNX vocoder via rightkit-ort), vendored from yet-another-ai/qts v0.2.0.
- Version 0.2.4; publishable; license Apache-2.0 (not dual); `repository` points at `adrdsouza/claude`, not `bogusyogi/rightkit` (same for the two ggml crates). lib name `qts`.
- src LOC (11399): `pipeline/tts_transformer.rs` 6093; `pipeline/vocoder.rs` 1232; `lib.rs` 1101; `pipeline/backend.rs` 682; `pipeline/speaker_encoder.rs` 514; `pipeline/tokenizer.rs` 490; `voice_clone_prompt.rs` 267; `model/` 489 (`loader.rs` 250, `paths.rs` 162, `test_fixture.rs` 69, `mod.rs` 8); `mode.rs` 238; `synthesis_profile.rs` 221; `error.rs` 46; `pipeline/byte_unicode.rs` 18; `pipeline/mod.rs` 8; `build.rs` 16.
- Pub API: 103 fn, 28 struct, 6 enum, 1 trait.
- Features: default none. `metal`, `vulkan`, `blas`, `native` forward to `rightkit-qts-ggml-sys`; 19 ONNX EP names (`acl armnn azure cann coreml cuda directml migraphx nnapi nvrtx onednn openvino qnn rknpu tensorrt tvm vitis webgpu xnnpack`) forward to rightkit-ort.
- Workspace dependents: `rightkit-tts` (optional `qts`, `engine` feature; forwards only `metal`/`vulkan`, `rightkit-tts/Cargo.toml:29-30,40`).

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| rightkit-qts-ggml-sys | path 0.1.0, no defaults | no | `use ggml_sys as sys` (`backend.rs:7`, lib name `ggml_sys`; grep for the crate name shows 0) | needed | |
| thiserror | 2 | no | `error.rs:3` | needed | |
| rand | 0.10 | no | `tts_transformer.rs:8,5259` (one `rand::rng().random::<f32>()`) | replace-with | one uniform f32 for sampling; `getrandom` or a tiny xorshift. Adds a third rand major (0.8/0.9/0.10 in lock) |
| hound | 3.5.1 | no | `speaker_encoder.rs:7` (WavReader, production) | needed | `rightkit-qa/media.rs` and `rightkit-tts/wav.rs` also have WAV code |
| rustfft | 6.4.1 | no | `speaker_encoder.rs:8-9,70` | needed | mel front end |
| prost | 0.14 | no | `voice_clone_prompt.rs:2` | needed | but see mismatch below |
| rightkit-ort | path 0.2.2, `tracing` | no | `vocoder.rs` (6) | needed | |
| (build) prost-build | 0.13 | no | `build.rs:11-15` | replace-with | machete false positive (used by build.rs) but removable: `proto/voice_clone_prompt.proto` is 3 messages / 9 fields; hand-write `#[derive(prost::Message)]` structs, drop `build.rs`. Also fixes prost 0.13.5 (build) vs 0.14.4 (runtime) duplicate in the lock |
| (build) protoc-bin-vendored | 3 | no | `build.rs:9` | replace-with | same: only exists to feed prost-build; ships protoc binaries |

### Overlap / split notes
- **qts vs qts-ggml vs qts-ggml-sys vs tts.** qts calls `ggml_sys` directly; `rightkit-qts-ggml` is a deprecated facade with zero dependents (see below). `rightkit-tts` consumes qts only for the `metal`/`vulkan` features, so qts's other 21 forwarding features are unused by any workspace crate.
- Two ORT session builders: `vocoder.rs:778-790` + `VocoderExecutionProvider` is a private reimplementation of rightkit-ort `SessionOptions` (level3 + intra threads + EP registration).
- Split candidate: the crate holds two engines in one: ggml transformer/tokenizer/speaker encoder (needs `ggml_sys`, 9k LOC) and an ORT vocoder (needs rightkit-ort). Both are required for synthesis so a split only helps if the vocoder is reused (it is not).
- `tts_transformer.rs` at 6,093 lines is the single largest source file in the group.

---

## rightkit-qts-ggml

### Profile
- Purpose: **deprecated** thin safe wrapper (`Context` over `ggml_init`/`ggml_free`) kept "published so existing consumers keep building" (`src/lib.rs:1-4`).
- Version 0.1.1; publishable (no `publish=false`); Apache-2.0; lib `qts_ggml`.
- src LOC: `lib.rs` 63 (one test).
- Pub API: 2 fn, 1 struct.
- Features: default none; `metal`, `vulkan`, `blas`, `native` forward to sys.
- Workspace dependents: none (grep of all Cargo.toml / package.json / sources found only qts-ggml-sys).

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| rightkit-qts-ggml-sys | path 0.1.0, no defaults | no | `lib.rs:6` (`pub use ggml_sys as sys`) | needed | the only dependency |

### Overlap / split notes
- Retire: no consumer in the workspace, deprecated in its own docs, 63 lines. Remove from the workspace (and yank/final-release) once external consumers are confirmed gone.

---

## rightkit-qts-ggml-sys

### Profile
- Purpose: FFI bindings + CMake build of vendored ggml (CPU, Metal, Vulkan, BLAS); `links = "rightkit_ggml"`.
- Version 0.1.1; publishable; Apache-2.0 AND MIT; lib `ggml_sys`.
- Size: `build.rs` 395; `src/lib.rs` 87; `bindings_stamp.rs` 87; `tests/bindings_match.rs` 129; checked-in generated `bindings-aarch64-apple-darwin.rs` 4058 and `bindings-x86_64-pc-windows-msvc.rs` 3970 lines; `vendor/ggml` 7.6 MB.
- Pub API: bindgen-generated (not counted by the pub-fn grep; src has 0 hand-written items).
- Features: default none; `metal`, `vulkan`, `blas`, `native`; `regenerate-bindings` (dep bindgen).
- Workspace dependents: rightkit-qts, rightkit-qts-ggml.

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| bindgen | 0.72 | `regenerate-bindings` | `build.rs` `#[cfg(feature="regenerate-bindings")] generate_bindings` | needed | machete false positive: used only under the feature; default builds copy the checked-in bindings |
| cmake | 0.1 | no | `build.rs:~38` `cmake::Config::new` | needed | builds vendored ggml |

### Overlap / split notes
- `bindings-x86_64-pc-windows-msvc.rs` is listed in `PENDING_UNSTAMPED` (`tests/bindings_match.rs:14`): the Windows bindings are not stamped against the vendored headers.
- `build.rs` contains host-tool discovery for managed builds (cmake shims, Vulkan SDK, LIBCLANG) at `:210-` that is process-env mutation specific to GenRight; keep out of any shared crate boundary.
- Only two bindings files (macOS arm64, Windows x64): a Linux/macOS-x86_64 target will panic with "no checked-in bindings" unless `regenerate-bindings` is used (`build.rs` fallback).

---

## rightkit-record

### Profile
- Purpose: platform-neutral recorder core (RK3): record lifecycle, quarantine/recovery, session bundle writer, per-field redaction, bundle verification, frame reader; OS capture behind a `Backend` trait.
- Version 0.1.1; `publish = false`; MIT OR Apache-2.0; lib `rightkit_record` + bin `rightkit-record-helper`.
- src LOC (9369): `recorder.rs` 844; `windows/` 1884 (`mod.rs` 685, `mf.rs` 363, `live.rs` 344 (test-only), `emit.rs` 205, `gpu.rs` 161, `window.rs` 126); `process.rs` 635; `bundle.rs` 582; `macos/` 982 (`live.rs` 552, `golden.rs` 243, `mod.rs` 187); `protocol.rs` 533; `verify.rs` 526; `media.rs` 471; `pacer.rs` 420; `redact.rs` 388; `plan.rs` 245; `backend.rs` 229; `codec.rs` 224; `bin/rightkit-record-helper/` 535 (`args.rs` 313, `main.rs` 222); `fake.rs` 206; `reader.rs` 203; `error.rs` 114; `support.rs` 96; `hash.rs` 94; `clock.rs` 86; `lib.rs` 72. Plus `helpers/macos` (Swift).
- Pub API: 152 fn, 54 struct, 11 enum, 3 trait.
- Features: none.
- Workspace dependents: none.

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| rightkit-contracts | path | no | 41 | needed | |
| rightkit-framed-sidecar | path | no | `process.rs:21-22,396`, `macos/golden.rs:17` | needed | |
| serde | 1.0.228 derive | no | protocol/bundle | needed | |
| serde_json | 1.0.150 | no | 51 | needed | |
| blake3 | 1.8 | no | `hash.rs` (18) + bundle/verify/reader | needed | file/frame hashing |
| hmac-sha256 | 1.1.14 | no | `hash.rs:36-37` | needed | RFC 4231 HMAC for redaction MACs; a third hash stack (blake3, own SHA-256 inside hmac-sha256, sha2 elsewhere) but wire format is fixed (`HMAC_PREFIX`) |
| rightkit-secrets | path | no | `redact.rs:133-135` | needed | redaction key store |
| rightkit-process | path | macOS target | `macos/mod.rs:28` (OwnedChild, OwnedCommand, StderrTail) | needed | already target-gated |
| windows | 0.62, 20 features | Windows target | `windows/*` (56) | needed | every feature maps to a use except one: `Foundation_Metadata` has no `Foundation::Metadata` use in src (no `IsSupported`/`ApiInformation` either); likely removable (confirm on a Windows build) |

### Overlap / split notes
- `media.rs` (471: `probe_mp4`, `synthetic_fragmented_mp4`) is a pure-Rust MP4 box parser duplicating `rightkit-media/src/quickprobe.rs` (exact WAV/MP4 duration) and qa's ffprobe path.
- Platform halves: `windows/` (~1.9k LOC, 20 `windows` features: WGC + D3D11 + Media Foundation) and `macos/` (~1k LOC + Swift) are each a distinct platform capture implementation; the neutral core is about 5.5k LOC. `windows/` could be its own crate (it was folded in from `helpers/record-windows`), which would remove the heavy windows feature list from the core manifest. Cost is nil on macOS because of target gating.
- Capture overlap with `rightkit-control/src/offscreen.rs` (off-screen WebView recording) is by purpose (app-content capture vs window capture); not compared in code.

---

## rightkit-review

### Profile
- Purpose: deterministic review rules over a media/brief description, declared holds, capped repair loop, advisory judge interface, versioned verdict records (no media decoding, no model calls).
- Version 0.1.0; `publish = false`; MIT OR Apache-2.0; `#![forbid(unsafe_code)]`.
- src LOC (1074): `rules.rs` 461; `subject.rs` 172; `repair.rs` 140; `judge.rs` 83; `record.rs` 73; `verdict.rs` 62; `lib.rs` 43; `error.rs` 40. Tests: `repair_and_judge.rs` 325, `holds.rs` 214, `planted_defects.rs` 125.
- Pub API: 10 fn, 17 struct, 7 enum, 2 trait.
- Features: none.
- Workspace dependents: none (GenRight/CutRight/PostRight are external apps).

### Dependencies
| dep | spec | optional | where used | verdict | reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | 46 | needed | |
| serde_json | 1.0.150 | no | `record.rs:43,48,71`, `error.rs:15,36` | needed | |

### Overlap / split notes
- Measurements enter as data (`FrameStat`, `TextRegion`, `DeterminismProbe`, `subject.rs`); `rightkit-qa::media::{pixel_stats, frames_report, image_report}` compute comparable statistics. They are complementary, not duplicated, but the two stat structs are separate types that would need a conversion if a pipeline used both.
- No merge needed; smallest, cleanest crate in the group.

---

## Cross-cutting overlap summary (group 3)

1. ONNX session setup: `rightkit-ort::SessionOptions` is used only by rightkit-embed; ocr (`support/session.rs`), asr (`execution.rs`, 12 builder sites), vad, qts vocoder each hand-roll provider selection. Add DirectML sequential/no-memory-pattern knobs and migrate ocr first.
2. Bounded subprocess runners: ocr `worker.rs`, ownership `runner.rs`/`gitio.rs`, qa `process.rs::run_owned`, media `runner.rs::run_capture` each reimplement deadline + output caps + kill; ocr and ownership use plain `Child::kill` (no tree kill). Add one `rightkit_process::run_bounded`.
3. Process liveness / tree kill: qa x2 (`process.rs:13`, `storage.rs:662`) + `AdoptedProcess::is_running` + `sandbox::process_tree` + `OwnedChild::terminate_tree`.
4. Media parse/probe: qa `media.rs`/`loudness.rs`, media `quickprobe/probe/loudnorm`, record `media.rs` (three MP4/WAV parsers, two ffmpeg-tool locators, plus playback `FfmpegCandidates`).
5. HTTP mock: qa `mock.rs`+`http.rs` vs rightkit-http `testing.rs`; rightkit-models depends on the whole QA crate only for the mock.
6. qts family: qts-ggml is a dead facade; qts forwards 21 features nobody uses; prost 0.13/0.14 split.
7. Merge: playback into rightkit-media. Split: qa validators (media/loudness/audio) out of the harness; ownership scanner vs trust stack; record `windows/` as its own crate (optional).
8. Small-crate hygiene: overlay has no license files, tests, or rust-version and has product names; qts crates carry a different `repository` URL and Apache-only license than the rest.
