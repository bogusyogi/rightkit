# Independent review of the merged RightKit inventory

Reviewer: Fable 5.1 (read-only). Date: 2026-10-08, evening. Subject: `2026-10-08-inventory-merged.md` (the Haiku + Luna merge). Code read at commit `ee07cb03c` plus the working tree; app repos read under `/Volumes/D/claude/*`. Only `rightkit cargo metadata --no-deps --offline` was run; no builds, no tests, no Windows host.

Verdict in one paragraph: the merged list is accurate on what it covers. Of 49 spot-checked claims, 45 are confirmed, 3 are overstated or partly wrong, and 1 is already fixed and committed. The ranking is defensible for RightKit-internal hygiene but misses the largest win Adrian actually asked for: **apps still ship their own copies of six RightKit capabilities** (model store, audio capture, telemetry and crash, OS-native OCR, Windows job objects, ONNX runtime policy), about 15,000 lines of donor code that RightKit already ported. Neither inventory looked inside the app repos beyond ViewRight's PDF crate. The ranked list also needs re-basing: the security item is done, the crate count is now 53, and one routing decision (moving `managed-roots.mjs` to RustRight) would break `right-release` as written.

## 1. State changes since the merge was written

These happened between 23:23 and the time of this review and make parts of the merged document stale, not wrong.

| Item | Merged doc says | Now | Evidence |
|---|---|---|---|
| F29 security fix | action #1, open | **done and committed** as rightkit-mcp 0.5.0 | commit `b90ba2b8d`; `crates/rightkit-mcp/src/server.rs:256` `random_hex_with` fails closed; tests at `:1338-1353` |
| Crate count | 50 | **53** (`rightkit-review`, `rightkit-scenario`, `rightkit-channel-memory` added) | `ls crates` = 53; `cargo metadata` lists them; commit `3f1d826f2` |
| rightkit-mcp version | 0.4.0 | 0.5.0 (staged in `rightkit-versions.json:261`; registry still 0.4.0 at `:311`) | manifest |
| F60 INDEX.md | stale at 47 crates | regenerated (53 rows) but the "crates.io" column still shows hotkeys/overlay as public with a MISMATCH marker | `INDEX.md:37,49` |

The merged document should be re-based on 53 crates before anyone acts on its coverage tables. The three new crates have no RK id in `docs/plans/2026-10-08-content-pipeline-execution-plan.md` (grep for `rightkit-review`, `rightkit-scenario`, `channel-memory` returns nothing), so their plan ownership is **unverified**.

## 2. Accuracy: spot-checks against code

Legend: C = confirmed as written; O = overstated or partly wrong; D = already done.

| # | ID | Claim | Result | Evidence |
|---|---|---|---|---|
| 1 | F29 | RNG error discarded; all-zero bearer possible | **D** | fixed in `server.rs:256-270`, committed `b90ba2b8d`; CodeRight still pins `=0.3.1` (`coderight/engine/Cargo.toml:156`) so the fix has not reached its only consumer |
| 2 | F01 | asr `eyre` unused | C | `asr/Cargo.toml:28`; zero hits in src/tests/build.rs |
| 3 | F03 | playback `serde`/`serde_json` unused | C | `playback/Cargo.toml:22-23`; zero hits |
| 4 | F02 | audio `windows-implement` direct dep unreferenced; `windows-core` needed | C (static) | `audio/Cargo.toml:43-44`; `lifecycle.rs:161` imports `windows::core::implement`; build unverified |
| 5 | F06 | qa framed-sidecar "unused" refuted | C | `qa/src/engine.rs:6` |
| 6 | F10 | process nix `signal` "unused" refuted | C, plus | `process_handle.rs:130-139` also calls `nix::libc::waitid`; nix is the libc re-export in two files, not one |
| 7 | F46 | watch tracing "replaceable" refuted | C | 9 `warn!`/`tracing::` sites |
| 8 | F04 | overlay dev `serde_json` unused | C | `overlay/Cargo.toml:19`; no `tests/` dir |
| 9 | F05 | oauth axum/tokio example-only | C | `oauth/Cargo.toml:24,27-28,53-54`; only `examples/server_axum.rs` |
| 10 | F07 | vad default enables `ort/download-binaries`; asr inherits | C | `vad/Cargo.toml:25,29`; `asr/Cargo.toml:37,51,56` |
| 11 | F08 | vad `ndarray`, qts `ndarray`+`tracing` unused | C (static) | no identifier hits in either `src/` |
| 12 | F12 | fs serde only in identity/lock/bin; db never uses serde | C | `fs/src/{identity,lock,bin/rightkit-clonefile}.rs`; `db/src` has no serde use |
| 13 | F26 | shell `engine` default; consumers opt out | C | `shell/Cargo.toml:13`; viewright `:63`, mailright `:111`, coderight-tauri `:51` all `default-features = false` |
| 14 | F27 | search `sqlite` default | C | `search/Cargo.toml:13` |
| 15 | F23 | agent-tools `browser` default; bin is cfg-gated; description names a non-dep | C | `agent-tools/Cargo.toml:6,12`; `right-ctl.rs:103`; `install-right-ctl.sh:10` |
| 16 | F34 | five crates declare `ort =` directly; tts re-implements dylib discovery | C | `ort =` in embed, asr, ocr, qts, vad (+ rightkit-ort); `tts/src/models.rs:149-153`, `worker.rs:52-54`, `client.rs:151` |
| 17 | F18 | tauri `getrandom =0.2.17` | C | `tauri/Cargo.toml:13`; `lib.rs:141` |
| 18 | F33 | qts-ggml is a 59-line wrapper | C | `qts-ggml/src/lib.rs` 59 lines; qts only forwards features (`qts/Cargo.toml:16-18`) |
| 19 | F61 | rightkit-computer deprecated, zero consumers | C | `computer/README.md:1-3`; no manifest in 14 repos names it |
| 20 | F65 | rightkit-tauri premise (898 lines, deps license+getrandom) | C | `tauri/Cargo.toml:13-14`; 898 lines incl. tests |
| 21 | F35 | 13 atomic-write copies | **O (undercount)** | 11 of the 13 cited lines verified; a wider grep for rename-based atomic patterns hits 39 files across crates. 13 is a floor |
| 22 | F38 | Windows job objects 3x in RightKit | C, plus | 4 files in 3 crates; **three more copies live in apps** (section 3) |
| 23 | F41 | logs/models/mcp bypass rightkit-http | C | `logs/upload.rs:81,88`; `models/download.rs:184-286`; `mcp/Cargo.toml:19-20` |
| 24 | F28 | mcp carries ureq + reqwest/tokio/futures-util | C | `mcp/Cargo.toml:19-22` |
| 25 | N01 | doc-render mermaid/katex unused | C | `doc-render/package.json:34-35`; zero src imports |
| 26 | N03 | @rightkit/tauri depends on updates type-only | C, plus | `tauri/package.json:19` is `workspace:0.2.4`, which publishes as an exact pin; that spec is the root cause of the duplicate copy |
| 27 | N09 | platform-ui QA helpers "dead" refuted | C | `genright/src/lib/platform.ts:2,15` |
| 28 | N16 | root `test` script covers 8 packages | C | `package.json:11` |
| 29 | F42 | license-v2.json 4 identical copies | C | md5 `0852…` x4 source copies (5 more under `.build/`, not tracked) |
| 30 | F54 | stray tracked `registration.patch`, `license/Cargo.lock` | C, plus | `git ls-files` confirms; add untracked `rightkit/:memory:.ses` |
| 31 | F56a | automation + agent-room-core `publish = false` yet staged | C | `Cargo.toml:7-8`; `rightkit-versions.json:246,248` |
| 32 | F20 | sha2 0.11 in cache/models/pdf, 0.10 elsewhere | C | 14 manifests; `cache:13`, `models:29`, `pdf:35` |
| 33 | F49 | embed vector store uses rusqlite directly, no bundled passthrough | C | `embed/src/vector.rs:1`; `embed/Cargo.toml:15` |
| 34 | F45 | transcript dev-depends on asr | C | `transcript/Cargo.toml:23` |
| 35 | F11 | fs/jobs nix `fs` feature for one lock each | C | `fs/Cargo.toml:25`; `jobs/Cargo.toml:26` |
| 36 | F22 | agent-room-core icacls duplicates fs/private.rs | C | `runtime.rs:57`; `fs/private.rs:180` |
| 37 | F31 | qts-ggml-sys build.rs probes Homebrew/HOME/site-packages cmake | C | `build.rs:281-309` |
| 38 | R04 | broker reads addon twice then `require` (TOCTOU) | C | `windows-job.mjs:23-29` (two `readFile` calls, then `require(path)`) |
| 39 | A01 | agent-room-mcp stores bearer + resume token as plain JSON 0600 | C | `agent-room-mcp/src/index.ts:98-112` |
| 40 | R01 | build-control-staging 11 lines, unused core dep | C | `staging/src/index.mjs` 11 lines; `package.json:8` |
| 41 | R06 | hard-coded `/Volumes/D` / `D:\` | C | `build-control-core/src/storage.mjs:12` |
| 42 | N17 | template pins stale | C | `desktop-app-template/package.json:44,53`; `src-tauri/Cargo.toml:35` |
| 43 | F19 | models zip, tar, flate2, dirs, chrono, base64 "not optional" | **O** | `chrono` and `base64` are already optional behind `rightapps`, and `default = []` (`models/Cargo.toml` features block). Only zip/tar/flate2/dirs are unconditional |
| 44 | F37 | qa has 20 raw `Command::new` | C | grep count 20 |
| 45 | F51 | product names in secrets/service/speech-client | C, scoped right | a wider grep finds product names in 20+ crates, but outside those three they are comments and docs |
| 46 | 2.B | CutRight uses a TS playback adapter, not rightkit-playback | C | `cutright/apps/cutright/src/lib/mediaAdapters.ts` |
| 47 | G26/27 | PDF and OCR duplicated in ViewRight | C, incomplete | OCR is duplicated in **three** apps, not one (section 3) |
| 48 | L01 | per-crate SHA-256 is domain-specific, no action | **O** | the *file-digest utility* is copied verbatim: `models/hash.rs:7` and `models/suite/manifest.rs:345` have byte-identical bodies; plus `embed/embedder.rs:699`, `jobs/store.rs:1502`, `media/lib.rs:645`, `tts/models.rs:94`; hex encoders at `cache/key.rs:7`, `embed/embedder.rs:686`, `http/untrusted.rs:111`; `sha256_hex` at `pdf/model.rs:767`, `qa/util.rs:51` |
| 49 | §1.1 | 50 crates | stale | 53 (section 1) |

Net: the merge's own verification work is sound. Its refutations of Haiku and Luna claims (F06, F10, F46, F48, N09, F66, F23) all hold.

## 3. Completeness: what both inventories missed

### 3.1 App-local copies of RightKit capabilities (the biggest gap)

RightKit's own source headers say where its code came from, and every donor is still shipping in its app. This is the "RightKit code that apps duplicate" list Adrian asked for.

| # | Capability | RightKit owner (and its origin note) | App copies still shipping | Size | Evidence of sameness |
|---|---|---|---|---|---|
| M1 | Model store | `rightkit-models/src/suite/` ("ported from ScrapeRight's `scraperight-model-store`… CutRight carries a copy", `suite/mod.rs:1-4`) | `scraperight/crates/model-store` (4,429 lines); `heardright-model-store` (3,201 lines) which consumes the ScrapeRight crate **by git dependency on another app repo** (`heardright-model-store/Cargo.toml:11`) | ~7,600 | ~150 shared function names between scraperight-model-store and rightkit-models (acquire_model_grant, download_resumable, parse_content_range, strong_etag, unpack_checked, write_atomic …); 24 shared with heardright-model-store. CutRight already migrated: `cutright-model-store` is an 81-line adapter |
| M2 | Microphone capture | `rightkit-audio::capture` ("Donor: HeardRight `heardright_capture`", `capture/mod.rs:4`), feature `capture`, zero consumers | `heardright/tauri-app-next/heardright_capture` (1,789 lines) | 1,789 | shared fns `add_emitted`, `classify_description`, `default_device_matches`, `from_cpal`, `finish` … |
| M3 | System metrics, crash capture, dump helper | `rightkit-logs/src/system/metrics.rs` (370 lines); `rightkit-crash` ("Merged from HeardRight `crash_dump.rs`, `crash_capture.rs`… and `heardright-dump-helper`", `crash/lib.rs:10-11`), zero consumers | `heardright-telemetry/src/system_metrics.rs` (396 lines) + `crash_dump.rs` + `telemetry_outbox.rs`; `heardright-dump-helper` (125 lines); `coderight-support-bundle` (405 lines) | ~6,500 | 37 shared fns incl. `read_cpu_times`, `thermal_state`, `sysctl_u64`, `webview_version`, `install_id`, `enqueue`, `purge` |
| M4 | OS-native OCR (Apple Vision / Windows.Media.Ocr) | `rightkit-ocr/src/native/` + `router.rs` | `viewright-ocr` (755), `scraperight-ocr` (1,412), `coderight-ocr` (546): same `vision.rs` / `win_native.rs` layout and `is_available`, `is_backend_unavailable` API | 2,713 | file layout and public fn names |
| M5 | Windows job objects | `rightkit-process` (owner) + sandbox + automation (F38) | `coderight/engine/crates/tools/src/sandbox/windows_job.rs` (117 lines, a diverged variant of `rightkit-sandbox/src/windows_job.rs`, 78 lines, 111 differing lines); `coderight/engine/bins/coderight/src/daemon/kill_tree.rs`; `heardright-app-runtime/src/engine_supervisor/lifecycle.rs` | ~400 | `CreateJobObject` grep. CodeRight pins `rightkit-sandbox =0.2.4` **and** carries its own job object |
| M6 | ONNX runtime candidate policy | `rightkit-ort` | `coderight-ort-common` (289, "runtime/model path adapter over published RightKit ORT"), `scraperight-ort-common` (105, "runtime candidate policy for rightkit-ort"); `voiceright` uses `ort 2.0.0-rc.9` directly with no RightKit at all (`voiceright/src-tauri/Cargo.toml:15`) | ~400 | descriptions |
| M7 | Prompt-cache keys | `rightkit-cache::prefix` ("from CodeRight", `cache/lib.rs:6`), published 0.1.0, zero consumers | `coderight-cache` (614 lines) | 614 | shared `observe_prefix`, `system_prompt_only_cache_key`, `fnv1a`, `parse_iso_day` |
| M8 | Hotkeys and overlay | `rightkit-hotkeys`, `rightkit-overlay` (zero consumers; overlay enum literally names `HeardRightPill`, `CockpitNotch`, `overlay/lib.rs:17`) | `heardright/src-tauri/src/hotkey.rs`, `fn_hotkey/ffi.rs`, `middle_mouse.rs`; Cockpit `pulse-hub` | unverified overlap | file presence only; algorithm overlap not measured |
| M9 | ASR / Parakeet decoding | `rightkit-asr`, `-vad`, `-transcript`, `-speech-client` (plan RK4) | `heardright-inference` (18,467), `heardright-onnx-asr` (1,487), `heardright-parakeet-core` (1,904), `orthic-speech` in scraperight (1,893), `cutright-speech` (2,961), `scraperight-engine` ASR | >25,000 | 37 fn names shared with heardright-parakeet-core/onnx-asr, 34 with orthic-speech. RK4 owns asr but the plan says nothing about retiring these copies |
| M10 | Line-delimited sidecar protocol | `rightkit-framed-sidecar` | `scraperight-protocol` ("Line-delimited JSON protocol types for the scraperight-engine sidecar", 1,215 lines) | 1,215 | description |

Apps also bypass RightKit for primitives it owns:

- **ViewRight** depends on `keyring-core`, `windows-native-keyring-store` and `ureq` directly (`viewright/src-tauri/Cargo.toml:57,67,91`) and does not pin `rightkit-secrets` or `rightkit-http` at all.
- **MailRight** keeps `reqwest 0.13` (`mailright/src-tauri/Cargo.toml:73`) alongside `rightkit-http =0.1.0` (`:109`).
- **WorkRight** uses zero RightKit crates and pins `ed25519-dalek =2.2.0` + `sha2 =0.10.9` + `rusqlite` + `tracing-subscriber` itself (`workright/Cargo.toml:28-45`). The dalek + sha2 pair is `rightkit-license`'s verification stack; whether WorkRight re-implements license verification is **unverified**.
- Hand-rolled atomic writes (file counts, indicative only): CodeRight 16 (e.g. `mobile_runtime/src/git.rs:982`, `tui/src/composer/history_store.rs:105`, `scheduler/persistence.rs:23`), CutRight 6, HeardRight 4. `sha256_file`-style helpers: CodeRight 64 files, GenRight 6, CutRight 5.

Why this matters more than any dependency trim: every RightKit crate above with "zero consumers" (audio, crash, cache, hotkeys, overlay, ocr, pdf) has a live donor in an app. The merged list treats them as "owner decisions" or proposes turning their features off by default (F24, F25). The correct frame is: **each is a half-finished extraction**. Either the app adopts the crate at its next RightKit bump and deletes the donor, or the crate is archived. Doing neither is the current state and is the main maintenance cost.

### 3.2 RightKit-internal items both missed

| # | Finding | Evidence | Action |
|---|---|---|---|
| I1 | **Routing error for RustRight.** `packages/release/managed-roots.mjs` and `managed-cargo.mjs` are imported by release modules that stay in RightKit: `artifact-storage.mjs`, `cargo-contract.mjs`, `cargo-target.mjs`, `development.mjs`, `publish-cargo.mjs`, `signing-agent-install.mjs`, `managed-host-secrets.mjs`, `rightkit-signing-agent.mjs`. Moving them to RustRight as the merged doc routes (section 4.2) breaks `right-release` unless RustRight publishes a client module that release consumes | grep of `managed-cargo|managed-roots` in `packages/release/*.mjs` (14 hits) | RustRight ships `@rightkit/build-control-clients`-style API first; release switches imports; only then do the files move |
| I2 | **`packages/release` is a 213-file, 35,349-line monolith.** 40 `ownership-*.mjs` files (7,104 lines) implement the same domain as the top-level `ownership-approval/` directory (20 of them reference it). Neither inventory proposed a split | `ls packages/release/*.mjs | wc -l`; prefix histogram: ownership 40, release 13, macos 9, signing/sign 14, cargo 7, publish 6 … | split `ownership-*` into one package merged with `ownership-approval/`; keep signing and publish in release |
| I3 | **Unused features nobody flagged.** `rightkit-asr` exposes 8 execution-provider features (cuda, tensorrt, directml, migraphx, openvino, webgpu, nnapi, preload-dylibs) and `rightkit-qts` 19 EP pass-through features; no consumer enables any (genright-qts sets `default-features = false` and only `metal`/`vulkan`). Also unused by any consumer: `rightkit-db` `hooks`, `rightkit-playback` `tauri`, `rightkit-shell` `devtools`, `rightkit-search` `filename-index` | `cargo metadata` feature lists; app manifests enable only `tauri-plugin`, `sqlcipher`, `bundled`, `wv2-0_38`, `browser`, `half`, `session`, `metal`, `vulkan` | trim asr/qts EP lists to what the suite ships (coreml, directml, cpu); delete `playback/tauri` and `db/hooks` unless a test builds them (unverified) |
| I4 | **rightkit-models carries two complete stacks** (root 5,093 lines vs `suite/` 4,174 lines). G30 records it, but action #16 reduces it to "one safe-archive module". The real action is retiring the root stack once CutRight and GenRight (pins `=0.2.1`, `=0.2.2`) are confirmed on `suite/` | line counts; `suite/mod.rs:1-4` | which stack CutRight/GenRight call is **unverified**; check before deleting |
| I5 | **service vs speech-client overlap.** `rightkit-service` ("the one discovery and authentication convention", `service/lib.rs:1-6`) and `rightkit-speech-client` both implement host discovery, takeover and `service.json` reads (`service/discovery.rs:35-192`; `speech-client/lib.rs:103-198`) | headers and fn lists | fold speech-client's host-plan/takeover into rightkit-service; speech-client keeps only the EngineFrame client |
| I6 | **File-digest and hex helpers** are copied 11 times in RightKit (L01 above) and dozens of times in apps. Owner should be `rightkit-fs` (a `digest` module), not a new crate and not "keep local" | L01 row | do with action #6 below |
| I7 | **HeardRight consumes a sibling app repo by git** (`heardright-model-store/Cargo.toml:11`) | manifest | violates "apps consume published RightKit, never Git"; fix via M1 |
| I8 | Stray `rightkit/:memory:.ses` (SQLite in-memory misuse artifact), untracked | `git status` | delete with F54 |
| I9 | New crates `rightkit-review`, `rightkit-scenario`, `rightkit-channel-memory` have no plan row; channel-memory and contracts both pin `jsonschema 0.58.6` with defaults off (54 crates, shared) | manifests; plan grep | owner to add RK ids; no dependency action |
| I10 | Out of RightKit scope, one line: `coderight/engine/bins/coderight/D:/work/.coderight/worktrees/...` is a Windows path literal materialised as a directory tree on the Mac checkout, containing copies of `windows_job.rs` and `kill_tree.rs` | `find … CreateJobObject` | CodeRight owner to delete |

## 4. Prioritisation: is the ranked list in the right order?

The stated rule is security and correctness first, then biggest dependency and maintenance win at lowest risk. Against that rule:

- **#1 is done.** Replace it with the delivery step: bump CodeRight's `rightkit-mcp` pin from `=0.3.1` to `=0.5.0` (`coderight/engine/Cargo.toml:156`). Until then the fix protects nobody.
- **R04 (broker TOCTOU) is the only open security defect** and sits in a separate RustRight table. It should head the combined list regardless of routing; the broker is a privileged daemon loading a native addon.
- **#2 (ort download-binaries off)** is correctly high: a build-time network fetch is a supply-chain and reproducibility issue.
- **#3 (route all ORT through rightkit-ort)** is the largest structural win but medium-high behavioural risk. It belongs after the zero-risk batch (#13 unused deps, #9 default features, #19 remove computer), not before.
- **The app donor retirement programme (section 3.1) is absent** and is the biggest maintenance win in the whole audit, roughly 15,000 lines of duplicated code plus a cross-repo git dependency. It needs app-owner coordination, so it is medium risk, but it should rank immediately after the security and zero-risk items and absorb #21 (pin reconciliation): each app's next RightKit bump is the moment to delete the donor.
- **#9 is partly the wrong direction.** Turning `audio/capture` and `crash/zip` off by default because "no consumer exists" ignores that HeardRight ships both donors. Decide adoption first (M2, M3); only then trim defaults.
- **#8 (mcp reqwest removal)** is HIGH risk, 25 crates, and rightkit-mcp just shipped a 0.5.0 break. Defer until `rightkit-http` has a streaming API (RK10 touches http anyway), then mcp consumes it.
- **#20 (A01 plaintext credentials)** is correctly below the Rust security items: the file is mode 0600 and the Node side has no RightKit secrets package, so "move to rightkit-secrets" is not directly possible. Mark as decide: OS keychain via a Node dependency, or document the 0600 contract.
- **#18 (release hygiene)** is partly done (INDEX regenerated); keep only the versions JSON reconciliation and the stray files.

## 5. Optimality: better designs where I see one

| Proposal | Merged design | Better | Why |
|---|---|---|---|
| F65 fold rightkit-tauri into rightkit-shell | DEC | **Do not fold.** Keep rightkit-tauri separate | it has no `tauri` dependency (deps: license + getrandom), compiles in engines and CLIs, and is pinned by 6 apps; shell is 253 crates and 3 apps opt in. The npm side (`@rightkit/tauri` vs `@rightkit/shell`) has zero export overlap (measured); only N06's log command names should move to `@rightkit/logs` |
| F33 qts-ggml merge | "into -sys or qts" | **into rightkit-qts as `mod ggml`**, delete the crate | 59 lines; GenRight's three forwarding optional deps become one; -sys stays the FFI boundary |
| F24/F25 audio capture, crash zip off by default | feature trims | **adoption decision first** (M2, M3) | the donors are live; trimming a port nobody uses while the donor ships is churn |
| F36 lock unification | DEC | **no unification**; rightkit-fs exposes two named primitives (`record_lock` POSIX fcntl, `flock`) and callers choose | semantics differ on purpose (`jobs/runner.rs:63-66`); a shared module that preserves both is the whole win |
| F35 atomic write owner | rightkit-fs after F12 | agree, and add `digest` (I6) in the same release | one fs bump instead of three; CodeRight pins fs `=0.1.0`, so plan the bump |
| F34 ONNX ownership | delete 5 direct pins | agree, plus: rightkit-ort absorbs the "runtime candidate policy" from `coderight-ort-common` and `scraperight-ort-common` and tts's `find_onnxruntime`, so apps delete their adapters (M6) | otherwise the apps keep a wrapper over the wrapper |
| F22 agent-room-core `http` feature | RK | **lowest priority**; alternatively move server deps to bin-only via `required-features` | zero crate consumers today; the only client is the TS package over HTTP |
| F64 zero-consumer published crates | DEC per crate | **rule**: a published crate with zero consumers after one release cycle gets `publish = false` until a consumer lands | stops paying release cost (cache, search, playback, audio, crash, pdf) without deleting code |
| F61 remove computer | RK | agree, do in the first batch | zero risk |
| F63 align/compose | PLAN keep | agree | |
| G30 models two stacks | "one safe-archive module" | **retire the root stack** after confirming CutRight/GenRight use `suite/` | 5,093 lines vs a shared helper |
| N03 updates peer | peer dep | agree, and fix the `workspace:0.2.4` spec that produces exact pins | root cause, not symptom |
| R-items | RR | agree, with I1's sequencing | `right-release` must not lose `managed-roots` |
| I2 release split | (none) | `packages/ownership` (merge `ownership-*.mjs` + `ownership-approval/`), `packages/release` keeps build/sign/publish | 7,104 lines of ownership logic in a release tool |
| I5 service/speech-client | (none) | speech-client keeps the EngineFrame client only; host plan/takeover moves to rightkit-service | one discovery convention, as service's own header claims |

## 6. Risks: what could break shipping apps, and the verification each needs

| Action | Consumers at risk | Break mode | Verification required |
|---|---|---|---|
| mcp 0.5.0 pin bump in CodeRight | CodeRight only | `serve_http` now returns `Result` on RNG failure; call sites must handle it | CodeRight MCP HTTP integration tests; `rightkit cargo test -p rightkit-mcp` already passes per commit |
| ort defaults off (F07) | asr/vad dev builds; GenRight/ScrapeRight/CutRight via rightkit-ort | dev build fails to find ORT without `load-dynamic`/rightkit-ort | `rightkit cargo check -p rightkit-vad -p rightkit-asr --no-default-features --features cpu`; tree diff 47→10 |
| ONNX routing (F34, M6) | GenRight, ScrapeRight, CutRight, CodeRight (ort pins =0.1.4 … =0.2.0), VoiceRight (rc.9) | provider/session options differ per app; CoreML/DirectML selection | rightkit-ort session smoke tests on macOS (CoreML) and Windows (DirectML); each app's e2e (`genright-e2e`, `cutright-ui-e2e`, ScrapeRight engine tests); VoiceRight is a separate rc.9→rc.13 migration |
| Model store adoption (M1) | ScrapeRight, HeardRight (live user data on disk) | on-disk layout or lock semantics differ; data loss | the suite port claims "without migrating a byte"; prove with a fixture-directory byte-identity test before and after; run on both OSes |
| Capture adoption (M2) | HeardRight dictation (latency-critical) | device recovery, resample quality | HeardRight dictation e2e + device unplug/replug QA on both OSes |
| Telemetry/crash adoption (M3) | HeardRight | crash dump path, upload outbox format | HeardRight telemetry upload contract test; Windows minidump path via `heardright-dump-helper` replacement |
| OCR adoption (M4) | ViewRight, ScrapeRight, CodeRight | Vision/WinRT availability probing, provenance fields | each app's OCR golden tests; Windows build |
| Job objects (M5, F38, F39) | CodeRight sandbox, HeardRight engine supervisor, all Windows builds | kill-on-close semantics; `windows` 0.61→0.62 types crossing the Tauri boundary (wv2 features) | Windows build + qualification by the Windows lead; CodeRight sandbox tests; HeardRight engine restart test |
| rightkit-fs json feature + nix→libc + digest + atomic (action 6) | rightkit-db (5 app repos), CodeRight (fs =0.1.0) | record-lock semantics; feature default | fs/jobs/process unit tests; db tree 30→23; `cargo check` on each consumer manifest with the new version |
| shell `engine` off by default | none (all opt out) | none | tree diff 253→244 |
| sha2 0.10 for cache/models | CutRight, GenRight (models) | API-identical; none expected | `cargo check -p rightkit-models` + tree diff |
| qts-ggml-sys pregenerated bindings | GenRight worker (metal/vulkan) | binding drift vs ggml pin | build the worker with `metal` (Mac) and `vulkan` (Windows) and run its e2e |
| mcp reqwest removal (F28) | CodeRight | streaming semantics | MCP streamable-HTTP integration suite; defer until rightkit-http streams |
| npm: updates peer, react-image-crop optional, peer ranges | MailRight, ViewRight, ScrapeRight, HeardRight, CodeRight, CutRight | install-time peer warnings; duplicate removal changes lockfiles | `pnpm install` lockfile diff per app + typecheck; republish is semver-minor |
| release split / managed-* move (I1, I2) | every app release | `right-release` import failure | `pnpm --dir rightkit test:release`, `doctor:release`, `right-suite-contract.test.mjs` |

Windows-only items (F02 windows-implement, F09 ocr features, F38–F40, M5) cannot be verified from this host and are labelled accordingly.

## 7. Final ranked action list I would sign off on

| # | Action | Scope | Risk | Replaces merged # |
|---|---|---|---|---|
| 1 | Deliver the mcp fail-closed fix: CodeRight `rightkit-mcp =0.3.1 → =0.5.0`; publish 0.5.0 (staged) | CodeRight, release | low | #1 (done) |
| 2 | Fix broker native-addon TOCTOU: read once, hash the buffer, load from a private verified copy. Track in RightKit until RustRight takes the package | build-control-broker | medium | RR#1 |
| 3 | Zero-risk batch: ort `download-binaries`/`tls-native`/`copy-dylibs` off by default in vad and asr, asr takes vad `default-features = false`, drop vad/qts `ndarray`+`tracing`; remove `eyre`, playback serde, overlay dev serde_json, oauth axum/tokio to example-only; delete `rightkit-computer`; delete stray files (`registration.patch`, `license/Cargo.lock`, `:memory:.ses`); reconcile versions JSON (`publish = false` crates out of `stagedCargo`, shell/sandbox registry entries) | vad, asr, qts, playback, overlay, oauth, computer, release | low | #2, #13, #19 (part), #18 |
| 4 | **App donor retirement programme**, one PR per app at its next RightKit bump, each deleting the donor: ScrapeRight and HeardRight model store → rightkit-models suite (removes the cross-repo git dep); HeardRight capture → rightkit-audio; HeardRight telemetry/crash/dump-helper → rightkit-logs + rightkit-crash; ViewRight/ScrapeRight/CodeRight native OCR → rightkit-ocr; CodeRight and HeardRight job objects → rightkit-process/sandbox; CodeRight cache → rightkit-cache; ViewRight keyring/ureq → rightkit-secrets/http; MailRight reqwest → rightkit-http. Fold pin reconciliation (process, ort, db, secrets, sandbox, qa, control, watch, template) into the same PRs | 7 apps | medium per app | new; absorbs #21, #23 (part) |
| 5 | ONNX ownership: delete the five direct `ort` pins, rightkit-ort absorbs runtime candidate policy (from coderight/scraperight ort-common) and tts dylib discovery; VoiceRight rc.9 migration as a separate task | vad, asr, ocr, qts, embed, tts, 4 apps | medium-high | #3 |
| 6 | rightkit-fs as primitive owner, one release: `json` feature (db opts out), nix→libc in fs/jobs/process, `digest` module (sha256_file/hex, 11 copies), `atomic_write` owner (≥13 copies), two named lock primitives (no semantic merge). Then migrate callers crate by crate | fs, db, jobs, process, then 12 crates | low (fs), medium (migrations) | #4, #16, L01 |
| 7 | Default-feature and dependency hygiene: shell `engine` off, search `sqlite` off, sha2 0.10 for cache/models, relax exact pins, http `gzip` off for llm/oauth, logs trims, tauri getrandom 0.3, thiserror/uuid/base64 small swaps, models zip/tar/flate2/dirs optional; trim asr/qts EP feature lists; drop `playback/tauri`, `db/hooks` if untested. **Not** audio capture or crash zip until #4 decides | 15 crates | low | #7, #9 (part), #10, #11, #12, I3 |
| 8 | qts: pregenerated ggml bindings, cmake probing review; merge `rightkit-qts-ggml` into `rightkit-qts` | qts-ggml-sys, qts-ggml, qts, GenRight | low-medium | #6, #19 (part) |
| 9 | Windows programme (Windows lead): `windows` 0.61→0.62 in the seven crates, job objects into rightkit-process, hotkeys → windows-sys, confirm `windows-implement` removal, ocr feature trims | 9 crates | medium | #17, F02, F09 |
| 10 | npm: doc-render drop mermaid/katex and decide remark-math; `@rightkit/tauri` updates → peer and fix `workspace:` spec; react-image-crop optional; peer ranges; log command names into `@rightkit/logs`; root test wiring (ownership-approval, characterization, infrastructure, remaining packages); qa README/publish state | 8 packages | low-medium | #14, #15, #20 (part) |
| 11 | Service convention: move speech-client host plan/takeover into rightkit-service; scraperight-protocol onto rightkit-framed-sidecar at ScrapeRight's next bump | service, speech-client, ScrapeRight, HeardRight | medium | I5, M10 |
| 12 | rightkit-models: confirm CutRight/GenRight are on `suite/`, then retire the root stack (5,093 lines) and keep one archive module | models, CutRight, GenRight | medium-high | #16 (part), G30 |
| 13 | release package: split `ownership-*` (7,104 lines) into one package merged with `ownership-approval/`; define the `managed-roots`/`managed-cargo` boundary with RustRight before any file moves | release, ownership-approval, RustRight | medium | I1, I2, RR#2–#6 |
| 14 | mcp HTTP stack: after rightkit-http gains streaming (RK10), mcp drops reqwest/tokio-net; logs and models move to rightkit-http | mcp, http, logs, models | high | #8 |
| 15 | Embed/pdf/transcript/product-leak cleanups and agent-room-core `http` feature | embed, pdf, transcript, cache, secrets, service, speech-client, sync, agent-room-core | medium | #22, #5 |
| 16 | Owner decisions, each with a deadline tied to #4: overlay and hotkeys (HeardRight/Cockpit adopt or archive); pdf (ViewRight adopts or `publish = false`); rule that zero-consumer published crates go `publish = false` after one cycle; `LicenseController`; qa-chrome helpers; agent-room-mcp credential store; plan ids for review/scenario/channel-memory; WorkRight license-stack duplication (unverified) | as listed | n/a | #23, A01, I9 |

## 8. Unverified items carried forward

- Windows builds for F02, F09, F38–F40, M5 (no Windows target on this host).
- Which rightkit-models stack CutRight and GenRight actually call (needed before #12).
- Whether WorkRight re-implements license verification (ed25519-dalek + sha2 pin only).
- Algorithmic overlap of HeardRight hotkey/overlay code with rightkit-hotkeys/overlay (file presence only).
- Whether any CI build exercises the asr/qts execution-provider features before trimming them.
- Plan ownership of `rightkit-review`, `rightkit-scenario`, `rightkit-channel-memory`.
- RustSec advisory status (no `cargo audit` run, same gap as the merge).
