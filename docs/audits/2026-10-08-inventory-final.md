# RightKit inventory: final reconciled list

Date 2026-10-09. Inputs: `2026-10-08-inventory-merged.md` (F/N/R/S ids), `-review-fable.md` (Fb; ids M1-M10, I1-I10), `-review-astra.md` (As; ids A-H, section letters). Read-only pass; the only commands run were greps, `sed`, `cmp`, and `ls`. No builds or tests, no Windows host. "Verified" means I read the cited line this pass.

Rebased facts: 53 crates (`ls crates` shows 54 entries, one is `INDEX.md`). F29 (mcp RNG) is fixed in source (`crates/rightkit-mcp/src/server.rs:256-270`) and delivered to nobody until CodeRight moves off `=0.3.1` (`coderight/engine/Cargo.toml:156`, per both reviews). Windows lead retracted "automation does not compile on Windows"; the `windows` 0.62 move is cleanup only. `rightkit-automation` header (`Cargo.toml:11-13`) confirms the `windows` dependency is `cfg(windows)`-gated.

## 1. Agreement table

Legend: AGREED, FABLE-ONLY, ASTRA-ONLY, CONFLICT. Resolution: accept / reject / decide (owner).

### 1.1 AGREED (both reviews, or both confirm the merged claim)

| Item | Finding | Result |
|---|---|---|
| F01 asr `eyre` unused | both C | accept, Wave 1 |
| F03 playback serde/serde_json unused | both C | accept, Wave 1 |
| F04 overlay dev `serde_json` unused | both C | accept, Wave 1 |
| F02 audio `windows-implement` | both C statically (`audio/Cargo.toml:43-44`, `lifecycle.rs:161`) | accept, gated on a native Windows build |
| F06 qa framed-sidecar is used | both C (`qa/src/engine.rs:6`) | keep; refutation stands |
| F10 process nix `signal` is used | both C | keep |
| F07 vad defaults leak ort download features into asr | both C (`vad/Cargo.toml:25,29`; `asr/Cargo.toml:37`) | accept, Wave 1 |
| F29 RNG fail-closed | both: done in source | Wave 0 item is delivery only |
| F33 qts-ggml is a forwarding layer | both C | accept retirement; design differs, see CONFLICT |
| F34 ORT bypasses rightkit-ort | both C (asr/vad/qts/embed/ocr `ort =`; `tts/models.rs:152`) | accept, Wave 4 |
| F61 remove rightkit-computer | both C (`publish = false`, `computer/Cargo.toml:8`) | accept, Wave 1 |
| F65 do not fold rightkit-tauri into shell | both reject | reject; merged DEC closed as "keep" |
| F42 license-v2.json copies identical | both C | accept; see As-F for the fourth-copy test gap |
| F45 transcript dev-depends on asr | both C | accept, Wave 1 (gate behind a feature/macOS cfg) |
| F49 embed couples inference and vector store | both C | accept as feature split, not a db merge (As) |
| F23 right-ctl feature gating is right | both C | keep |
| F28 mcp carries two HTTP stacks | both C | defer, Wave 4 (needs rightkit-http streaming) |
| N01 doc-render mermaid/katex unused | both C | accept, Wave 1 |
| N03 `@rightkit/tauri` updates dep is type-only | both C | accept; fix the `workspace:` spec |
| N04/N05/N06 | As C; Fb silent but not disputed | accept (N04 as peer, N06 move names to `@rightkit/logs`) |
| F35 atomic-write copies | both agree; Fb says 13 is a floor, As adds CutRight copies | accept, owner rightkit-fs |
| F36 lock unification | both reject a semantic merge | reject merge; keep two named primitives |
| F38 job objects triplicated | both C | accept, Wave 2/3 |
| A01 agent-room-mcp plaintext 0600 credentials | both C; As says classification overstated | decide (Adrian): document the 0600 contract or add a keychain dependency |
| S02 Swift LicenseController vs extraction rule | As C; Fb silent | decide (Adrian) |
| F26/F27 shell `engine` and search `sqlite` defaults | Fb C; As rejects blanket default flips, notes shell consumers already opt out | no consumer win; see CONFLICT D-defaults |
| Models two stacks (G30) | both agree duplication is real | accept; scope differs, see CONFLICT |
| App-donor duplication exists | Fb M1-M10, As per-app table; overlap on ScrapeRight model store, HeardRight telemetry and ASR, CodeRight OCR, MailRight search/cache | accept; merged list in Wave 3 |

### 1.2 FABLE-ONLY (resolved)

| Id | Item | Verification | Resolution |
|---|---|---|---|
| I1 | `managed-roots.mjs`/`managed-cargo.mjs` are imported by release modules that stay | verified: imports at `packages/release/artifact-storage.mjs:1`, `cargo-contract.mjs:5`, `cargo-target.mjs:14`, `development.mjs:1,9`, `managed-host-secrets.mjs:1`, `publish-cargo.mjs:6`, `rightkit-signing-agent.mjs:2`, `signing-agent-install.mjs:1` | accept; RustRight must publish a client API first, then release switches imports. Owner RustRight, sequencing agreed with RightKit lead |
| I2 | `packages/release` monolith, 40 `ownership-*.mjs` plus `ownership-approval/` | counts from Fb, not re-measured | accept as Wave 4 (low urgency); owner RightKit lead |
| I3 | unused features: asr 8 EP features, qts 19, db `hooks`, playback `tauri`, shell `devtools`, search `filename-index` | not re-verified; As's CI-coverage caveat applies | accept only for asr/qts EP lists after a CI check; `filename-index` is rejected because As shows Cockpit can adopt it (`cockpit/core/src/storage_browser.rs:403`); db `hooks` and playback `tauri` need a build check first |
| I4 | models root stack (5,093 lines) vs `suite/` (4,174) | line counts from Fb; which stack CutRight/GenRight call is unverified (GenRight pins `rightkit-models` in four manifests, CutRight via `cutright-model-store`) | needs owner decision: CutRight and GenRight app chats confirm which stack. As asks for adapters, not a forced merge |
| I5 | service vs speech-client discovery overlap | not re-read | accept as Wave 2 (medium); owner RightKit lead |
| I6 | file-digest/hex helpers copied 11 times | Fb line cites, not re-read | accept; owner rightkit-fs `digest` module |
| I7 | HeardRight consumes ScrapeRight by git (`heardright-model-store/Cargo.toml:11`) | Fb cite | accept; folded into the model-store donor retirement. Violates the locked invariant |
| I8 | stray `rightkit/:memory:.ses` | present in git status (untracked) | accept, Wave 1 |
| I9 | new crates `rightkit-review`, `-scenario`, `-channel-memory` have no plan ids | unverified | decide (RightKit lead) |
| I10 | `coderight/engine/bins/coderight/D:/work/...` literal tree | not checked, out of scope | app chat (CodeRight) |
| M1-M10 | donor inventory (model store, capture, telemetry/crash, OCR, job objects, ORT policy, cache, hotkeys/overlay, ASR, scraperight-protocol) | spot-verified: `scraperight/crates/model-store/src/lock.rs` is byte-identical to `rightkit-models/src/lock.rs` (`cmp`); `coderight/.../sandbox/windows_job.rs` exists; HeardRight capture dir exists | accept M1-M7, M10; M8 hotkeys/overlay overlap unmeasured so needs owner decision (HeardRight/Cockpit chats); M9 ASR accepted but As adds the caveat that decoder primitives must not drag ORT into CoreML hosts |
| F20 | sha2 0.10 for cache/models | see CONFLICT F19/F20 | reject as a saving |
| rule | zero-consumer published crates get `publish = false` after one cycle | policy proposal | decide (Adrian) |
| Fb #7 | models zip/tar/flate2/dirs optional | the base64/chrono part was already wrong (optional under `rightapps`); zip/tar/flate2 unconditional per Fb | accept zip/tar/flate2 only; dirs per CONFLICT |

### 1.3 ASTRA-ONLY (resolved)

| Id | Item | Verification | Resolution |
|---|---|---|---|
| A | `Query::parse_lenient` on malformed input yields `Query::default()` which compiles to `1=1`, contradicting its doc ("matches nothing") | VERIFIED: `search/src/ast.rs:122-124` (`parse_lenient` doc and `unwrap_or_default`), `search/src/sql.rs:177-181` (empty becomes `"1=1"`), `tests/sqlite_e2e.rs:261-265` (empty query matches all). The ast parser returns `Err(UnbalancedGroup)` for a stray `)` (`ast.rs:119-125`) | accept, Wave 0 correctness. Fix: make lenient return a no-match query (or an explicit `invalid` flag) and keep empty-string behavior |
| B | `scripts/install-right-ctl.sh` signs the newest `release/right-ctl` under all managed workspaces | VERIFIED: `scripts/install-right-ctl.sh:11-12` (`find ... -path '*/release/right-ctl' ... ls -t | head -1`) | accept, Wave 0. Interim fix in RightKit: take the path from the build's reported artifact, or `rightkit cargo build --message-format=json`. Durable fix needs a RustRight output receipt |
| C | audio macOS `objc2-core-*` deps unconditional though used only under `capture` | VERIFIED: `audio/Cargo.toml:30-32` unconditional on macOS; uses only at `capture/device.rs:72,82`; `lib.rs:16-17` gates `capture`; `lifecycle` at `lib.rs:22` unconditional | accept, Wave 2 (make the objc2 deps `optional` under `capture`; gate lifecycle). Supersedes F24 default-trim |
| D | rightkit-tts `system` feature does nothing | VERIFIED: `tts/Cargo.toml:15-17` (`default = ["system"]`, `system = []`) vs `tts/src/lib.rs:11-15` (modules, `pub mod system`, ungated) | accept, Wave 2: make `system` real or delete the feature |
| E | pregenerate qts protobuf | `qts/proto/...`, `qts/build.rs:10` not re-read; Fb silent | accept as optional, low value; Wave 4 with the ggml bindings item |
| F | the fourth `license-v2.json` is outside the parity test | VERIFIED: `swift/contracts/contracts.test.mjs:9-13` lists swift, rust, typescript only; a fourth exists at `swift/RightKitSwift/Tests/RightKitLicenseTests/Contracts/license-v2.json` (`find`) | accept, Wave 0 (test-only). Keep the copies, add the fourth to the equality check |
| G | `models/archive.rs` deletes the destination before renaming | VERIFIED: `models/src/archive.rs:81-84` (`remove_file(dest)` then `rename`); the deterministic `dest.with_extension("extract-tmp")` at `:49` | accept, Wave 0 correctness: use atomic replace (`rename` over the destination; on Windows, `ReplaceFileW`), keep the old file on failure |
| H | npm table too compressed | doc quality | accept as docs task only; not a code wave |
| 20 | F36b sync line reads bounded; MCP server and legacy client are not | VERIFIED: MCP server `server.rs:643` uses unbounded `reader.lines()`; sync is bounded (As cite, not re-read) | accept, Wave 0: bound `stdio_loop` input lines in rightkit-mcp (needs a 0.5.1) |
| per-app | ScrapeRight lock/variant identical, GenRight chunk/wav, HeardRight `system_identity` and parakeet beam/backend/scoring, CutRight chain/filter/ducking, MailRight search/cache, Cockpit filename search, CodeRight OCR | VERIFIED: `cmp scraperight/crates/model-store/src/lock.rs ...rightkit-models/src/lock.rs` identical; `cmp genright/.../chunk.rs ...rightkit-tts/src/chunk.rs` identical. Others per As | accept into Wave 3 |
| 21,22,40 | F37 count of `Command::new` overstated; F39 speech-client Windows dep is dev-only; S03 archive script is not a second publisher | cites in As; F37 consistent with Fb's raw count of 20 | accept corrections; drop F39's runtime saving for speech-client; keep F37 only as "count terminal spawns" |
| S01 | Swift README drift already fixed | As cite | accept; drop S01 |
| G-rec | Reject handwritten base64/uuid/thiserror and nix to libc | see CONFLICT | accept As |

### 1.4 CONFLICTS

| Id | Fable | Astra | Resolution (evidence) |
|---|---|---|---|
| F05 oauth axum/tokio to example-only | C, treated as an open trim | overstated | REJECT as work. VERIFIED already done: `oauth/Cargo.toml:24` `server-example = ["server","dep:axum","dep:tokio"]`; axum/tokio are `optional = true` at `:53-54`. Moving them to dev-deps would only make test builds heavier. Remove from the list |
| F08 vad/qts `ndarray`+`tracing` unused | C (static) | overstated for tracing | NEEDS BUILD CHECK. `vad/Cargo.toml:17` and `qts/Cargo.toml:47` enable `ort/ndarray`; `ort-defaults` at `vad/Cargo.toml:29` also enables `ort/tracing`. Accept `ndarray` removal only if `cargo check` passes (Wave 1, owner RightKit lead). Keep `tracing` out of scope until observability is checked |
| F19/F20 sha2 and models trims | F20 C; F19 O | F19 wrong, F20 savings overstated | ACCEPT As on savings. F19: chrono/base64 already optional behind `rightapps` (both agree). F20: SHA 0.10 and 0.11 closures are the same size per As's lock reading; only a consumer-graph delta counts. Downgrade F20 to "align versions when touched", no claimed savings |
| F11 nix to libc in fs/jobs | accept (action 6) | defer; adds unsafe FFI | ACCEPT As, defer. The fs lock and jobs runner require POSIX record-lock semantics (`jobs/runner.rs:63-66`, `fs/src/lock.rs:244`). Wave 4 only if a unified fs lock API exists first. Does not block `json` feature or `digest` work |
| F36 lock unification | no merge; two named primitives | defer | AGREED outcome (see 1.1). Naming fs's two primitives is Wave 2 |
| F24/F25 audio capture, crash zip off by default | adoption first | reject blanket default flips; add real feature boundaries | ACCEPT BOTH PARTIALLY. Do As-C (real boundaries) in Wave 2. Defer default flips until the Wave 3 adoption decision for HeardRight capture and crash |
| F27/F26/F9 defaults off (shell engine, search sqlite) | accept | reject blanket default change | REJECT the flips. Shell consumers already opt out (viewright `:63`, mailright `:111`, coderight-tauri `:51`, per Fb), so no win; published default is behavior |
| F18 / small swaps (base64, uuid, thiserror) | included in action 7 as "small swaps" | reject | REJECT the base64/uuid/thiserror swaps (UUID risks wire/persisted identifiers). Keep tauri `getrandom` 0.2 to 0.3 (F18) as a separate low-risk item |
| F33/qts-ggml fate | merge into `rightkit-qts` as `mod ggml` | qts depends on sys directly; deprecate facade, migrate GenRight first | DECIDE (Adrian + GenRight chat). Both retire the crate; difference is where the 59 lines land. Recommendation: As's route, since it keeps `-sys` the single FFI owner and moves GenRight's three pins to two |
| G30 models | retire root stack after confirming callers | share low-level helpers first, keep adapters | DECIDE (CutRight and GenRight chats confirm callers; Adrian approves). Do As's low-level share first (archive/lock/download), then the Fb retirement |
| F40 hotkeys to windows-sys | "drop-in" | overstated | ACCEPT As: not drop-in; Windows lead, low priority |
| F65 npm side | Fb: zero export overlap, only move N06 names | As: TS can share contracts | AGREED effectively: move N06 log command names only |
| F37 raw `Command::new` count | C (20) | overstated | ACCEPT As: count terminal spawn paths. Use `rightkit-process` OwnedCommand coverage as the metric |
| Fb "TOCTOU R04 is the only open security defect" | yes | As: copying verified bytes helps only if the copy location resists replacement | ACCEPT As's caveat. Owner RustRight; the fix must load from a private, non-writable-by-others location |
| Fb #11 service/speech-client, M10 scraperight-protocol | accept | not addressed | ACCEPT as Wave 2/3 (not disputed) |
| Dependency-removal ordering | Fb: zero-risk batch before ORT routing | As: unused deps rank above everything but correctness | AGREED; reflected in waves |

## 2. Ranked action list by wave

Each wave item names one worker and disjoint files. Owner defaults to the RightKit lead unless noted. Support: Fb = Fable, As = Astra, M = merged inventory only. Savings are estimated unless marked measured.

### Wave 0: security and correctness (run in parallel)

| # | Change | Files | Removal/benefit | Risk | Verification | Owner | Support |
|---|---|---|---|---|---|---|---|
| 0.1 | Deliver mcp fail-closed: CodeRight `=0.3.1` to published `=0.5.0` (publish 0.5.0 first; staged in `rightkit-versions.json`, registry still 0.4.0) | `coderight/engine/Cargo.toml:156`, `rightkit-versions.json` | closes F29 for the only consumer | low (serve_http now returns `Result`) | CodeRight MCP HTTP tests; `rightkit cargo test -p rightkit-mcp` | CodeRight app chat; RightKit lead publishes | Fb, As |
| 0.2 | Bound MCP server stdio line reads (and review the legacy client path) | `crates/rightkit-mcp/src/server.rs:643`, `client.rs:384` | closes unbounded-allocation path | low-medium | oversize-line test; ships as 0.5.1 with 0.1 | RightKit lead | As |
| 0.3 | Search: malformed lenient query must match nothing | `crates/rightkit-search/src/{ast.rs:122,terms.rs:37,sql.rs:177}`, `tests/sqlite_e2e.rs` | correctness (fail-open to every row) | low | add test: `")"` returns zero rows; `"   "` still returns all | RightKit lead | As |
| 0.4 | models archive: replace atomically, never delete first | `crates/rightkit-models/src/archive.rs:49,81-84` | prevents loss of an installed file on rename failure | low-medium | injected-failure test; Windows XPS run for the rename semantics (Windows lead) | RightKit lead + Windows lead | As |
| 0.5 | right-ctl installer: sign the binary the build produced | `scripts/install-right-ctl.sh:11-12` | removes stale/concurrent-binary signing | low | two candidate outputs with misleading mtimes; macOS sign smoke. Durable form needs a RustRight build receipt | RightKit lead (interim); RustRight (receipt) | As |
| 0.6 | Broker native-addon TOCTOU: read once, verify, load from a private copy not writable by others | `build-control-broker` `windows-job.mjs:23-29` (per Fb) | closes load-after-verify race in a privileged daemon | medium | broker tests; Windows XPS run (addon is the Windows job addon) | RustRight (tracked here until the Rust rewrite) | Fb (As agrees with caveat) |
| 0.7 | Add the fourth license-v2.json to the parity test | `swift/contracts/contracts.test.mjs:9-13` | keeps four vector copies from drifting | none | `node --test swift/contracts/contracts.test.mjs` | RightKit lead | As |

### Wave 1: zero-risk removals and trims (parallel; each worker owns one crate or package)

| # | Change | Files | Expected removal | Risk | Verification | Owner | Support |
|---|---|---|---|---|---|---|---|
| 1.1 | Remove `eyre` from asr; asr takes vad with `default-features = false` and ort download/tls/copy-dylibs features off by default in vad | `rightkit-asr/Cargo.toml:28,37`, `rightkit-vad/Cargo.toml:17,25,29` | estimated: ort tree 47 to 10 (Fb figure, unmeasured here); removes a build-time binary download | low-medium: dev builds need `load-dynamic` or rightkit-ort | `rightkit cargo check -p rightkit-vad -p rightkit-asr`; asr/vad tests; then GenRight/ScrapeRight/CutRight consumer `cargo check` | RightKit lead | Fb, As, M |
| 1.2 | Remove playback `serde`+`serde_json`, overlay dev `serde_json` | `rightkit-playback/Cargo.toml:22-23`, `rightkit-overlay/Cargo.toml:19` | 3 deps | none | `cargo check` of both | RightKit lead | Fb, As, M |
| 1.3 | Remove vad/qts `ort/ndarray` only if it compiles | `rightkit-vad/Cargo.toml:17,29`, `rightkit-qts/Cargo.toml:47` | estimated: ndarray tree | low | `cargo check`; keep `tracing` | RightKit lead | Fb (As partial) |
| 1.4 | Delete `rightkit-computer` | `crates/rightkit-computer/`, workspace `Cargo.toml` | 1 crate, estimated hundreds of lines | none (unpublished, no consumers) | `cargo metadata`; versions JSON | RightKit lead | Fb, As, M |
| 1.5 | Delete stray files: `registration.patch`, `license/Cargo.lock`, untracked `rightkit/:memory:.ses` | repo root and `crates/rightkit-license/` | 3 files | none | git | RightKit lead | Fb, M |
| 1.6 | Reconcile `rightkit-versions.json`: crates with `publish = false` (automation, agent-room-core) out of `stagedCargo`; mcp registry entry | `rightkit-versions.json:246,248,261,311` | doc/state correctness | low | `pnpm --dir rightkit test:release` | RightKit lead | Fb, M |
| 1.7 | transcript: gate the asr dev-dependency | `rightkit-transcript/Cargo.toml:23`, `tests/roundtrip.rs:10` | lighter dev graph | none | `cargo test -p rightkit-transcript` | RightKit lead | Fb, As |
| 1.8 | doc-render: remove `mermaid`, `katex` | `packages/doc-render/package.json:34-35` | 2 packages | none | package build; ViewRight lockfile diff | RightKit lead | Fb, As |
| 1.9 | Windows: remove `windows-implement` from audio | `rightkit-audio/Cargo.toml:44` | 1 dep | low | native Windows build and XPS run | Windows lead | Fb, As |
| 1.10 | Trim asr/qts execution-provider feature lists to what ships (coreml, directml, cpu) | `rightkit-asr/Cargo.toml`, `rightkit-qts/Cargo.toml` | estimated: 8 and 19 features | low-medium | confirm no CI or consumer builds them first | RightKit lead | Fb only (As: do not trim without evidence) |

Dropped from the merged list as invalid: F05 oauth (already optional), blanket default flips, base64/uuid/thiserror swaps, S01 Swift README.

### Wave 2: consolidations inside RightKit (parallel; one owner crate each)

| # | Change | Files / crates | Expected removal | Risk | Verification | Owner | Support |
|---|---|---|---|---|---|---|---|
| 2.1 | rightkit-fs becomes owner of primitives: `json` feature (db opts out), `digest` module (sha256_file, hex), `atomic_write`, two named lock primitives | `rightkit-fs`, then callers in models (`hash.rs:7`, `suite/manifest.rs:345`), embed, jobs, media, tts, cache, http, pdf, qa | 11 digest copies and 13+ atomic-write copies (Fb counts; the atomic count is a floor); estimated | low in fs, medium per caller | fs/jobs/process unit tests; `cargo check` per consumer; one fs release | RightKit lead | Fb, As (As: by semantics, incremental) |
| 2.2 | Real feature boundaries: audio (objc2 deps optional under `capture`, lifecycle gate), tts (`system`/`engine`/client), embed (inference vs vector), ocr (native/onnx), pdf (edit/fonts/merge/extract) | audio `Cargo.toml:30-32`, `lib.rs:16-22`; tts `Cargo.toml:15-17`, `lib.rs:11-15`; embed; ocr; pdf | estimated: macOS CoreAudio deps and Windows bindings drop out of non-capture builds | medium (published defaults stay) | minimal/default builds per crate; Windows XPS run for audio lifecycle | RightKit lead; Windows lead for audio | As (C, D, plus its splits); Fb silent |
| 2.3 | models: one internal archive/lock/download layer used by root stack and `suite/`; fix atomic replace (Wave 0.4) first | `rightkit-models/src/{archive,lock,suite/}` | estimated: portion of 5,093-line root stack | medium | models tests; fixture byte-identity | RightKit lead | Fb, As |
| 2.4 | Service convention: fold speech-client host plan/takeover into rightkit-service | `rightkit-service/src/discovery.rs:35-192`, `rightkit-speech-client/src/lib.rs:103-198` | estimated: one discovery implementation | medium | service/speech-client tests; Windows pipe test (`tests/windows_pipe.rs`, XPS) | RightKit lead + Windows lead | Fb |
| 2.5 | Retire qts-ggml facade (qts depends on sys directly; deprecate the published facade) | `rightkit-qts-ggml/`, `rightkit-qts/src/pipeline/backend.rs:7` | 59 lines, 1 crate | low-medium | qts tests; GenRight worker build | RightKit lead after Adrian's decision (section 3) | Fb, As |
| 2.6 | ORT ownership: delete direct `ort =` pins in asr/vad/ocr/qts/embed, rightkit-ort absorbs runtime-candidate policy and tts dylib discovery; keep model-specific session options (asr DirectML at `model_transducer.rs:55`) | listed crates + `rightkit-ort`, `rightkit-tts/src/{models.rs:152,worker.rs,client.rs}` | estimated: 5 direct pins; enables Wave 3 deletion of two adapters | medium-high | rightkit-ort session smoke on macOS (CoreML) and Windows XPS (DirectML); each app e2e | RightKit lead; Windows lead | Fb, As |
| 2.7 | Small hygiene: tauri `getrandom` 0.3, sha2 align when touched (no saving claimed), relax exact pins, http `gzip` off for llm/oauth, logs trims, npm: `@rightkit/tauri` updates as peer and fix `workspace:` spec, react-image-crop as peer, N06 command names to `@rightkit/logs` | `rightkit-tauri/Cargo.toml:13`, `packages/tauri/package.json:19`, `packages/media-ui`, `packages/logs/src/index.ts:1` | estimated; small | low | per-package build; app lockfile diffs and typecheck (semver-minor) | RightKit lead | Fb, As |
| 2.8 | Product-name and agent-room-core `http` feature cleanups | secrets, service, speech-client; `agent-room-core` | estimated; small | low | tests | RightKit lead | Fb, M |

### Wave 3: app-donor retirement (each owned by that app's chat, at its next RightKit bump; one PR per app, disjoint repos)

Rule: adopt, delete the donor, keep app policy. Prove persisted formats are unchanged. Each app chat runs Mac and Windows.

| # | App | Retire | Replace with | Size | Risk | Verification | Support |
|---|---|---|---|---|---|---|---|
| 3.1 | ScrapeRight | `crates/model-store` (4,429 lines; `lock.rs` and variant files already byte-identical to RightKit, `cmp` verified for `lock.rs`) | `rightkit-models` suite | ~4,400 | medium-high (user data on disk) | fixture-dir byte-identity before/after; both OSes | Fb M1, As |
| 3.2 | HeardRight | `heardright-model-store` (3,201; consumes ScrapeRight by git, `Cargo.toml:11`) | `rightkit-models` suite | ~3,200 | medium-high | same; fixes the locked-invariant violation | Fb M1/I7 |
| 3.3 | HeardRight | `heardright_capture` (1,789) | `rightkit-audio` `capture` | ~1,800 | medium (dictation latency) | dictation e2e, device unplug/replug, both OSes | Fb M2 |
| 3.4 | HeardRight | `heardright-telemetry` system metrics, `system_identity.rs` (byte-identical to `rightkit-logs/src/system/identity.rs:13`), `crash_dump.rs`, outbox, `heardright-dump-helper` | `rightkit-logs`, `rightkit-crash` | ~6,500 | medium (consent/policy stays app-owned) | telemetry upload contract test; Windows minidump path | Fb M3, As |
| 3.5 | HeardRight | ASR primitives `beam.rs`, `backend.rs`, `scoring.rs` (byte-identical to `rightkit-asr/src/parakeet_core/`), inference crates | `rightkit-asr` via a light feature surface | exact-copy part small; full >25,000 is a later phase | medium-high | decoder golden outputs; production path coverage unconfirmed | Fb M9, As |
| 3.6 | CutRight | local audio chain/filter/ducking (300/76/151 RightKit lines present in app modules) | `rightkit-audio` filter graph | estimated ~500+ | low-medium | compare serialized presets and generated FFmpeg graphs | As |
| 3.7 | CutRight, ScrapeRight, GenRight | atomic-write helpers; `cutright-model-store` adapter already 81 lines (done) | `rightkit-fs` after Wave 2.1 | small | low | app tests | As, Fb |
| 3.8 | GenRight | `genright-qts/src/{chunk,wav}.rs` (`chunk.rs` verified byte-identical to `rightkit-tts/src/chunk.rs`) | `rightkit-tts` light helpers | ~100s | low | GenRight e2e; no engine load for chunking | As |
| 3.9 | ViewRight, ScrapeRight, CodeRight | native OCR (`viewright-ocr` 755, `scraperight-ocr` 1,412, `coderight-ocr` 546) | `rightkit-ocr` native-only feature (Wave 2.2 first; ocr currently drags ONNX) | ~2,700 | medium | each app's OCR golden tests; Windows build | Fb M4, As |
| 3.10 | CodeRight, HeardRight | Windows job objects (`sandbox/windows_job.rs`, `daemon/kill_tree.rs`, `engine_supervisor/lifecycle.rs`) | `rightkit-process` / `rightkit-sandbox` | ~400 | medium (Windows) | Windows build and XPS qualification; sandbox and engine-restart tests | Fb M5 |
| 3.11 | CodeRight | `coderight-cache` (614) | `rightkit-cache::prefix` | ~600 | low-medium | cache-key fixtures | Fb M7 |
| 3.12 | CodeRight, ScrapeRight | `coderight-ort-common` (289), `scraperight-ort-common` (105) | `rightkit-ort` (after Wave 2.6) | ~400 | medium | GenRight/ScrapeRight/CodeRight inference smoke | Fb M6 |
| 3.13 | ViewRight, MailRight, CodeRight | `keyring-core`/`ureq` direct use; `reqwest 0.13` beside `rightkit-http`; support bundle | `rightkit-secrets`, `rightkit-http`, `rightkit-logs` | estimated | medium | per-app tests | Fb |
| 3.14 | MailRight, Cockpit | search query, attachment/image cache; filename filtering (`storage_browser.rs:403`) | `rightkit-search` (+`filename-index`), `rightkit-cache::disk` | estimated | medium (cache key/layout migration) | query/golden tests | As |
| 3.15 | ScrapeRight | `scraperight-protocol` (1,215) | `rightkit-framed-sidecar` | ~1,200 | medium | engine tests | Fb M10 |
| 3.16 | All apps | pin reconciliation (process, ort, db, secrets, sandbox, qa, control, watch, template, mcp) folded into each PR above | per app | n/a | low | `cargo check` / `pnpm install` per app | Fb, M |
| 3.17 | VoiceRight | `ort 2.0.0-rc.9` direct use to rc.13 and rightkit-ort | separate migration | n/a | medium-high | VoiceRight e2e | Fb |
| 3.18 | CodeRight | delete literal `D:/work/...` tree under `bins/coderight` | n/a | tiny | none | git | Fb I10 |

### Wave 4: larger refactors (sequenced, lower priority)

| # | Change | Files | Expected removal | Risk | Verification | Owner | Support |
|---|---|---|---|---|---|---|---|
| 4.1 | RustRight publishes a client API for managed roots/cargo; release switches imports; then `managed-roots.mjs`/`managed-cargo.mjs` and broker items (`build-control-*`, rightkit CLI) move | `packages/release/*.mjs` (imports listed in section 1.2, row I1) | estimated: ~14 release modules stop owning broker code | medium-high | `pnpm --dir rightkit test:release`, `doctor:release`, `node --test packages/release/right-suite-contract.test.mjs` | RustRight; RightKit lead for imports | Fb |
| 4.2 | Release package split: one `ownership` package (40 `ownership-*.mjs`, 7,104 lines, plus `ownership-approval/`); release keeps build/sign/publish | `packages/release`, `ownership-approval/` | 0 lines removed, boundary only (Fb: 35,349-line package) | medium | same three commands | RightKit lead | Fb |
| 4.3 | Retire models root stack (5,093 lines) after Wave 2.3 and caller confirmation | `rightkit-models` root, CutRight, GenRight | estimated up to 5,000 lines | medium-high | consumer builds | RightKit lead + CutRight/GenRight chats | Fb |
| 4.4 | mcp: drop reqwest/tokio-net after rightkit-http gains streaming; move logs/models to rightkit-http | `rightkit-mcp/Cargo.toml:19-22`, `rightkit-http`, `logs/upload.rs:81`, `models/download.rs:184` | estimated: 25 crates in mcp tree | high | MCP streamable-HTTP suite, cancellation, response bounds, CodeRight smoke | RightKit lead | Fb, As (both: defer) |
| 4.5 | qts native build: pregenerated ggml bindings (bindgen is still a build-dependency, `qts-ggml-sys/Cargo.toml:25`) and protobuf code; align prost 0.14/0.13 (`qts/Cargo.toml:46,51`) | `rightkit-qts-ggml-sys`, `rightkit-qts` | build-time only; unmeasured | low-medium | build `metal` (Mac) and `vulkan` (Windows XPS); ABI/wire fixtures | RightKit lead; Windows lead | Fb, As |
| 4.6 | nix to libc in fs/jobs/process (only after 2.1 locks) | fs, jobs, process | estimated: 1 dep | medium (unsafe FFI) | lock semantic tests | RightKit lead | Fb accept, As defer |
| 4.7 | Windows programme: `windows` 0.61 to 0.62 across seven crates (cleanup, not correctness), hotkeys to windows-sys (not drop-in) | seven crates | estimated | medium | Windows build and XPS qualification | Windows lead | Fb, As |
| 4.8 | Test wiring: root test script (`package.json:11` runs 8 packages) to include ownership-approval, characterization, infrastructure, remaining packages | `package.json`, `packages/release/package.json:21` | n/a | low | CI | RightKit lead | Fb, As |

## 3. Decisions needed from Adrian (one line each, with recommendation)

1. Archive vs adopt for zero-consumer published crates (audio, crash, cache, hotkeys, overlay, search, pdf, ocr): recommend adopt at each donor app's next bump (Wave 3); anything with no adopter after one release cycle gets `publish = false`.
2. Zero-consumer rule as policy ("published crate with no consumer after one release cycle goes `publish = false`"): recommend yes.
3. Where the qts-ggml 59 lines go: recommend qts depends on `-sys` directly and the facade is deprecated (Astra), not merged into qts as `mod ggml` (Fable).
4. Models two-stack retirement (4.3): recommend approve only after CutRight and GenRight chats confirm which stack they call; do the shared low-level layer first.
5. `LicenseController` in Swift (S02): recommend move to a UI target or document the exception; the extraction rule and code disagree.
6. agent-room-mcp credential store (A01): recommend document the 0600/ACL contract now (file is already private) and defer a keychain dependency.
7. New crates `rightkit-review`, `-scenario`, `-channel-memory` lack plan ids: recommend add RK ids; no dependency action.
8. Hotkeys/overlay adoption (HeardRight, Cockpit): overlap is unmeasured; recommend ask each app chat for an adopt-or-archive answer before the next RightKit release.
9. WorkRight pins `ed25519-dalek` + `sha2` and uses no RightKit crate: recommend the WorkRight chat confirm whether it re-implements license verification (unverified).
10. Delivery of mcp 0.5.0 (publish, bump CodeRight pin): recommend do it now under the standing RightKit publish authorization; 0.2 (bounded stdio reads) rides with it as 0.5.1.
11. RustRight sequencing for `managed-roots`/`managed-cargo`: recommend RustRight ships the client API first and release imports switch before any file moves; broker TOCTOU (0.6) is not blocked on that.
12. Dropping the merged list's blanket default-off and handwritten-codec ideas: recommend yes, both reviews reject or contradict them.

## 4. Unverified carried forward

Windows builds for F02, audio lifecycle, job objects, hotkeys, `windows` 0.62 move (Windows lead); which models stack CutRight/GenRight call; WorkRight license stack; hotkey/overlay algorithm overlap; whether any CI exercises asr/qts EP features; RustSec status (no `cargo audit` run); ort tree 47 to 10 figure and all "estimated" removals; install-time effect of optional peers.
