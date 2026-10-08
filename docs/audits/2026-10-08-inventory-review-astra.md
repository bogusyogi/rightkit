**Verdict: useful inventory, but I would not sign off on its ranked action list unchanged.** It understates app-local duplication, treats several design proposals as verified optimizations, overstates dependency savings & puts risky rewrites ahead of straightforward removals.

Review was read-only: source inspection, manifest/lock parsing & byte comparisons; no builds, tests, publication or edits. Dependency-tree savings, registry state & installed-app behavior remain **unverified independently**. Paths below are relative to `/Volumes/D/claude/tools/rightkit`; `apps/` denotes `/Volumes/D/claude/`.

Checkout changed during review: MCP now declares `0.5.0`, its RNG fix is present, Swift README was corrected & new `rightkit-review` exists. These are snapshot changes, not evidence original reviewers fabricated findings.

**1. Accuracy — 40 spot-checks**

“Confirmed” verifies stated source fact, not automatically proposed remedy or estimated savings.

| # | Finding | Verdict | Evidence & correction |
|---|---|---|---|
| 1 | F01: ASR `eyre` unused | **Confirmed** | Declared at `crates/rightkit-asr/Cargo.toml:28`; recursive Rust-source scan found no reference. Straightforward removal candidate. |
| 2 | F02: audio `windows-implement` unused; retain `windows-core` | **Confirmed statically** | Direct declarations at `crates/rightkit-audio/Cargo.toml:43`. Actual macro import is `windows::core::implement`, at `src/lifecycle.rs:161`, used at `:198`. Direct `windows_implement` identifier absent. Windows compilation must validate removal. |
| 3 | F03: playback serde dependencies unused | **Confirmed** | `crates/rightkit-playback/Cargo.toml:22` declares both; recursive Rust-source scan found neither identifier. |
| 4 | F04: overlay `serde_json` is unused dev dependency | **Confirmed** | `crates/rightkit-overlay/Cargo.toml:18`; serde itself is used at `src/lib.rs:7`. Remove only `serde_json`. |
| 5 | F05: OAuth example dependencies should move to dev | **Overstated** | `crates/rightkit-oauth/Cargo.toml:24` already confines activation to `server-example`; example requires that feature at `:28`. Moving these to dev adds no consumer saving & can make ordinary test builds heavier. |
| 6 | F06: retain framed-sidecar in QA | **Confirmed** | `crates/rightkit-qa/src/engine.rs:6` imports & uses framing APIs. Original “unused” claim was correctly rejected. |
| 7 | F07: VAD defaults leak ORT download features into ASR | **Confirmed** | `crates/rightkit-vad/Cargo.toml:25` enables `ort-defaults`; `:29` forwards download/TLS/copy features. ASR dependency at `crates/rightkit-asr/Cargo.toml:37` retains VAD defaults. This proves feature activation, not that every build necessarily downloads binaries. |
| 8 | F08: ORT ndarray/tracing are unused | **Overstated** | Features are declared at VAD `Cargo.toml:17` & QTS `Cargo.toml:47`. Absence of application identifiers supports investigating ndarray; it does not prove ORT’s internal tracing is unused or valueless. Compile compatibility & observability are separate checks. |
| 9 | F10: retain process nix signal feature | **Confirmed** | `crates/rightkit-process/src/owned.rs:692` imports `signal::kill`; `:697` calls it. |
| 10 | F11/F36: replace nix & unify locks | **Overstated as optimization** | Real uses exist in `crates/rightkit-fs/src/lock.rs:254` & `crates/rightkit-jobs/src/runner.rs:60`. Jobs explicitly explains required POSIX semantics at `:63`; fs maintains descriptor ownership at `lock.rs:244`. Replacement introduces unsafe FFI obligations, not merely dependency deletion. |
| 11 | F19: models base64/chrono need optionality | **Wrong as blanket claim** | Both already belong to optional `rightapps`: `crates/rightkit-models/Cargo.toml:20`, `:24`, `:26`. Zip/tar/flate2 are unconditional at `:31`. Default-tree savings cannot include already-disabled chrono/base64. |
| 12 | F20: moving cache/models SHA saves eight crates each | **Overstated** | Cache’s sole runtime dependency is SHA at `crates/rightkit-cache/Cargo.toml:13`. Switching versions substitutes trees. Lock-derived recursive closures for SHA 0.10 & 0.11 both contain ten packages; see `Cargo.lock:6101` & `:6112`. Savings require an actual consumer already using 0.10 & no remaining 0.11 path. |
| 13 | F23: right-ctl is feature-gated correctly | **Confirmed** | Desktop/browser blocks are gated at `crates/rightkit-agent-tools/src/bin/right-ctl.rs:89` & `:103`. Default browser activation is explicit at `Cargo.toml:12`. No reason to remove CLI functionality merely to reduce its tree. |
| 14 | F28: MCP has two HTTP stacks | **Confirmed** | `crates/rightkit-mcp/Cargo.toml:19` declares ureq plus reqwest/Tokio. Async transport includes session handling, bounded bodies & cancellation: `src/advanced_http.rs:132`, `:544`, `:651`. Removing it is substantial behavioral work. |
| 15 | F29: RNG failure currently yields zero bearer | **Wrong for current checkout; historical finding superseded** | `crates/rightkit-mcp/src/server.rs:261` propagates entropy failure; `:262` rejects all-zero output. Token creation precedes bind at `:958`. Failure tests exist at `:1329`; execution & consumer delivery unverified. |
| 16 | F30/F31: ggml bindgen & host-tool discovery | **Confirmed** | Pregenerated path returns early at `crates/rightkit-qts-ggml-sys/build.rs:236`, but bindgen remains unconditional build dependency at `Cargo.toml:25`. Discovery probes Homebrew/home directories at `build.rs:284`. Adding generated files alone does not remove bindgen compilation. |
| 17 | F32: prost generations differ | **Confirmed** | Runtime `prost = "0.14"` at `crates/rightkit-qts/Cargo.toml:46`; `prost-build = "0.13"` at `:51`. Build executes vendored protoc at `build.rs:10`. |
| 18 | F33: qts-ggml is effectively forwarding layer | **Confirmed for inspected consumers** | Re-export at `crates/rightkit-qts-ggml/src/lib.rs:3`; its small `Context` wrapper starts at `:8`. QTS consumes `qts_ggml::sys` at `crates/rightkit-qts/src/pipeline/backend.rs:7`. GenRight explicitly pins all three layers at `apps/genright/crates/genright-qts/Cargo.toml:34`. |
| 19 | F34: ORT ownership bypasses | **Confirmed** | Direct ORT declarations remain in ASR `Cargo.toml:32`, VAD `:17`, OCR `:23`, QTS `:47`, embed `:13`. TTS duplicates runtime discovery at `crates/rightkit-tts/src/models.rs:152`. |
| 20 | F36b: sync has unbounded line reads | **Wrong for sync; incomplete for MCP** | Sync uses `take(MAX_LINE_BYTES + 1)` before reading at `crates/rightkit-sync/src/helper/process.rs:124`; cap is defined at `:20`. MCP client limits are conditional on extensions at `src/client.rs:384`; server still uses unbounded `reader.lines()` at `src/server.rs:643`. |
| 21 | F37: every `Command::new` represents unmanaged spawn | **Overstated** | TTS constructs `Command` at `crates/rightkit-tts/src/client.rs:148`, then wraps it in `OwnedCommand` at `:154` before spawning. Count terminal `.spawn/.status/.output` paths & ownership, not constructors. |
| 22 | F39: seven Windows runtime dependencies need alignment | **Overstated** | Speech-client’s declaration is **target-specific dev dependency**, `crates/rightkit-speech-client/Cargo.toml:22`, used by `tests/windows_pipe.rs:14`. Alignment may simplify tests but cannot deliver claimed runtime reduction there. |
| 23 | F40: hotkeys windows-sys swap is “drop-in” | **Overstated** | Code uses wrapper fields/types & ergonomic arguments: `crates/rightkit-hotkeys/src/windows/mod.rs:54`, `:64`, `:72`. It is plausible, but requires adapting signatures, handles, constants & error handling. |
| 24 | F42: four identical license vectors | **Confirmed** | All four files matched SHA-256 `afa2b97a…4319` in this review. Existing comparison names only three paths: `swift/contracts/contracts.test.mjs:9`. Packaging implications matter; see omissions below. |
| 25 | F45: transcript test drags ASR dev graph | **Confirmed** | Unconditional dev dependency at `crates/rightkit-transcript/Cargo.toml:23`; actual ASR test body is macOS-only at `tests/roundtrip.rs:10`. Fixture also reaches sibling source directory at `:31`. |
| 26 | F49: embed couples inference & vector storage | **Confirmed** | Both modules unconditional at `crates/rightkit-embed/src/lib.rs:3`; tokenizers at `Cargo.toml:19`. Vector storage accepts supplied SQLite connection at `src/vector.rs:46`, offering less invasive integration than replacing its whole storage abstraction. |
| 27 | F53: sync fixture is shipped executable | **Overstated** | Auto-discovered `src/bin/rightkit-sync-fake-helper.rs:1` exists & source include covers it at `Cargo.toml:12`. This is an exposed/packageable test binary; it does not establish installation into shipping apps. |
| 28 | F61: remove rightkit-computer | **Confirmed candidate** | Deprecation explicitly names replacement at `crates/rightkit-computer/README.md:3`; unpublished status & removal condition at `:18`. Inspected source/manifests found no external dependency. |
| 29 | F65: tauri should fold into shell | **Overstated** | Small tauri crate depends on getrandom & license at `crates/rightkit-tauri/Cargo.toml:13`; shell unconditionally depends on Tauri at `crates/rightkit-shell/Cargo.toml:26`. Folding would expand consumers’ native dependency surface. |
| 30 | N01: doc-render mermaid/katex unused | **Confirmed** | Dependencies at `packages/doc-render/package.json:34`; no source references found. Removal is justified independently of whether ViewRight retains its own copies. |
| 31 | N02: math output is plain text | **Confirmed; remedy needs care** | Rendering uses `<pre>/<code>` at `packages/doc-render/src/renderers.tsx:71`; parser installs remark-math at `src/parseMarkdown.ts:17`. Parser remains useful for math ASTs even without built-in typesetting. |
| 32 | N03: tauri updates dependency is type-only | **Confirmed** | `packages/tauri/src/index.ts:1`; exact workspace spec at `package.json:19`. Published declarations still require those types to resolve. |
| 33 | N04: image crop dependency only serves image entry | **Confirmed; “optional” remedy underspecified** | Static import & CSS at `packages/media-ui/src/image/ImageCropTool.tsx:2`; separate entry at `src/image.ts:4`. Simply marking an optional dependency does not establish install savings or safe missing-dependency behavior. |
| 34 | N05: update vectors exported at runtime | **Confirmed** | `packages/updates/src/index.ts:12`. This is public API; moving it needs compatibility re-export or breaking release. `package.json:17` already declares side-effect-free modules, so bundle savings need measurement. |
| 35 | N06: duplicated log command names | **Confirmed** | `packages/logs/src/index.ts:1` & `packages/tauri/src/index.ts:18`, repeated again at `:155`. |
| 36 | N16/O01: root test coverage incomplete | **Confirmed for inspected scripts** | Root `package.json:11` runs eight packages. Other scripts are separate; release’s glob at `packages/release/package.json:21` does not include ownership-approval directory. “Not in these entrypoints” is established; “never run anywhere” is not. |
| 37 | A01: agent-room credentials stored as plaintext | **Confirmed; vulnerability classification overstated** | `packages/agent-room-mcp/src/index.ts:110` writes JSON with Unix mode; `:111` assumes launcher applied Windows ACL. Confidentiality depends on launcher/private-directory guarantees. Rust secret-store adoption requires an actual cross-language interface. |
| 38 | S01: Swift README still has listed drift | **Wrong for current checkout** | `swift/README.md:4` now identifies 0.3.8, fourteen products & tagged Git consumption; `:26` explains historical target names. |
| 39 | S02: Swift controller conflicts with extraction rule | **Confirmed** | `LicenseController: ObservableObject` at `swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:860`; rule reserves product-facing controllers to apps at `swift/README.md:41`. This is boundary inconsistency, not proof controller is useless. |
| 40 | S03: Swift archive script is competing publication path | **Overstated** | `swift/scripts/archive-package.mjs:9` explicitly limits output to local archive/checksum; no upload. It may be obsolete, but duplication of publication machinery is not established merely by its existence. |

**2. Completeness — material omissions**

**A. Malformed search silently broadens to every row**

`Query::parse_lenient(")", ...)` encounters parser error, becomes `Query::default()`, then compiles to `1=1`. Its documentation promises malformed input “matches nothing.”

Evidence: [`ast.rs:122`](/Volumes/D/claude/tools/rightkit/crates/rightkit-search/src/ast.rs:122), [`terms.rs:37`](/Volumes/D/claude/tools/rightkit/crates/rightkit-search/src/terms.rs:37), [`sql.rs:177`](/Volumes/D/claude/tools/rightkit/crates/rightkit-search/src/sql.rs:177). Existing test explicitly confirms empty query matches everything at `tests/sqlite_e2e.rs:261`.

This is **confirmed source-level correctness defect**, not demonstrated shipping data exposure. Preserve errors or represent invalid/no-match separately from legitimate empty query. Verify malformed input returns zero results/error while intentional empty input retains documented behavior.

**B. right-ctl installer does not bind installed binary to completed build**

[`scripts/install-right-ctl.sh:10`](/Volumes/D/claude/tools/rightkit/scripts/install-right-ctl.sh:10) builds, then searches **every managed workspace** for `release/right-ctl`, sorts by modification time & signs newest match.

Concurrent/stale output can therefore select binary unrelated to completed request. “Root override supported” misses more important identity problem. Installer remains RightKit-owned; RustRight should provide exact build-output identity/receipt consumed by installer. Verify using two candidate outputs with misleading timestamps.

**C. Audio capture feature leaves platform dependencies active**

`capture` gates cpal/rubato/ringbuf, but macOS CoreAudio/CoreFoundation dependencies remain unconditional at `crates/rightkit-audio/Cargo.toml:30`. Their only source references are inside gated capture code at `src/capture/device.rs:72`.

Windows lifecycle remains unconditional through `src/lib.rs:22`, retaining Windows bindings for pure filter-graph consumers.

Add real feature boundaries: capture-specific macOS dependencies optional; lifecycle explicitly grouped or separately selectable. This is more useful than merely changing capture’s default.

**D. TTS `system` feature currently does nothing**

`crates/rightkit-tts/Cargo.toml:17` advertises default `system`, but `src/lib.rs:11` exports system implementation unconditionally. `--no-default-features` does not remove it; QTS client/model/protocol modules also remain active without engine.

Define actual `system`, `qts-client` & `engine` surfaces, preserving sensible defaults. Do not call features unused simply because consumers use defaults; this feature is specifically ineffective.

**E. Another build-dependency removal opportunity: pregenerate protobuf**

Report proposes pregenerated ggml bindings but only version alignment for protobuf. QTS generates three simple message types from a 25-line schema every build:

- `crates/rightkit-qts/proto/voice_clone_prompt.proto:5`
- `crates/rightkit-qts/build.rs:10`
- `crates/rightkit-qts/src/voice_clone_prompt.rs:7`

Consider checked-in generated Rust plus maintainer regeneration check. Keep runtime prost; remove routine prost-build/protoc execution. Verify encoded voice-clone fixtures against old generated code. Net build savings **unmeasured**.

**F. Golden-vector parity check omits actual Swift package resource**

Four copies are identical today, but `swift/contracts/contracts.test.mjs:9` compares three. It omits `swift/RightKitSwift/Tests/RightKitLicenseTests/Contracts/license-v2.json`, which Swift package processes via `Package.swift:36`.

Keep package-local fixture copies for standalone tests; add fourth to equality check or generate all copies from one source. Deleting copies in favor of sibling paths would damage standalone packaging.

**G. Archive replacement has correctness gap beyond duplication**

`crates/rightkit-models/src/archive.rs:76` deletes existing destination **before** renaming replacement. Rename failure loses previous destination. Deterministic temporary filename at `:49` also deserves review.

Public API is exported at `src/lib.rs:19`; installer currently calls it within staging at `src/install.rs:1022`. Thus source behavior is confirmed; loss of currently active shipping model is **not established**.

Fix replacement semantics within archive consolidation; verify destination survives injected activation failure. Do not characterize every archive use as equally exposed.

**H. Inventory is still incomplete as “every dependency” deliverable**

Rust tables are substantially detailed. npm merged table compresses packages into “12 runtime deps … others NEEDED,” omits most development dependencies & does not provide equivalent per-dependency verdicts. See merged document `:1270` versus actual `packages/doc-render/package.json:30` & `:44`.

Needed additions: npm runtime/dev/peer/optional dependency rows; actual enabled feature/target per shipping consumer; distinction between published availability, manifest declaration, source use & installed integration. One closure count cannot replace these.

**App-local duplication missed or insufficiently pursued**

Byte-identical matches below were verified directly, excluding dependency/build directories. They establish retained implementations, not installed-release reachability.

| App | Additional duplication & evidence | Recommended disposition |
|---|---|---|
| **ScrapeRight** | `apps/scraperight/crates/model-store/src/lock.rs` is byte-identical to `crates/rightkit-models/src/lock.rs`; entry API at `:157`. Variant file also identical to `crates/rightkit-models/src/suite/variant.rs:23`. Engine still depends on local model store at `apps/scraperight/crates/engine/Cargo.toml:27`. | Migrate shared lock/installation primitives; preserve store layout, lease semantics & approved artifact metadata. Do not merge stores blindly. |
| **GenRight** | `apps/genright/crates/genright-qts/src/chunk.rs:18` & `wav.rs:4` exactly match RightKit TTS equivalents. Live client imports both at `src/client.rs:5` & `:9`. | Use shared lightweight TTS helpers after publication; retain app worker/model policy. No need to load inference engine just for chunking. |
| **HeardRight diagnostics** | `apps/heardright/tauri-app-next/heardright-telemetry/src/system_identity.rs:13` exactly matches `crates/rightkit-logs/src/system/identity.rs:13`; caller at app `telemetry_upload/environment.rs:19`. | Reuse identity API; keep consent, telemetry event vocabulary & upload policy app-owned. |
| **HeardRight ASR** | `beam.rs`, `backend.rs`, `scoring.rs` match `crates/rightkit-asr/src/parakeet_core/` byte-for-byte. App exports these modules at `heardright-parakeet-core/src/lib.rs:14`. | Share decoder primitives through lightweight feature surface; do not force CoreML host to acquire unrelated ORT/tokenizer graph. Actual production path coverage needs confirmation. |
| **CutRight audio** | Direct line comparison found all 300 RightKit `chain.rs` lines, all 76 `filter.rs` lines & all 151 `ducking.rs` lines in corresponding app modules. Local crate remains engine dependency at `apps/cutright/crates/cutright-engine/Cargo.toml:35`. | Strong adoption candidate. Preserve extra app code/tests; compare serialized presets & generated FFmpeg graphs before replacing common implementation. |
| **MailRight search/cache** | Query implementation at `apps/mailright/src-tauri/src/db/search/query.rs:189`; attachment key/cache at `attach_cache.rs:44`; image cache at `webimg.rs:191`. RightKit explicitly identifies these donor responsibilities at `crates/rightkit-search/src/terms.rs:2` & `crates/rightkit-cache/src/disk.rs:3`. | Treat search/cache as adoption candidates, not ownerless crates. Keep mail operators/network fetching outside shared primitives. Cache key/layout migration required. |
| **Cockpit filename search** | `apps/cockpit/core/src/storage_browser.rs:403` retains filtering/sorting/pagination matching responsibility extracted into `crates/rightkit-search/src/filename.rs:1`. | Adopt `filename-index` with adapter for scan metadata/incomplete results. No SQLite dependency needed. |
| **CodeRight OCR** | Native OCR remains at `apps/coderight/engine/crates/ocr/src/lib.rs:27`; native-only manifest at `Cargo.toml:13`. RightKit OCR unconditionally includes ONNX dependencies at `crates/rightkit-ocr/Cargo.toml:23`. | Add native-only RightKit feature surface before adoption. Otherwise consolidation reintroduces inference dependencies intentionally absent from app. |

Also found CutRight atomic-write implementations at `cutright-project/src/fsutil.rs:13`, `cutright-qa/src/fsutil.rs:25` & `cutright-engine/src/media_cmd.rs:122`. These belong in F35’s adoption plan, alongside internal RightKit copies.

**3. Prioritisation**

Current ordering violates its own “largest win at lowest risk” principle:

- Proven-unused dependencies sit at **#13**, below unsafe FFI rewrites, speculative feature cuts & HTTP transport replacement.
- doc-render unused packages sit at **#15**, below handwritten base64/error implementations.
- Agent-room split sits at **#5** despite no store-only consumer benefiting.
- MCP transport rewrite sits at **#8**, although async protocol behavior is substantial & alternative ownership does not require rewriting transport immediately.
- Test wiring sits at **#20**, after changes that need those checks.
- App adoption sits behind crate-local optimization, even where shared code remains duplicated in active app sources.
- Search correctness, installer artifact identity & ineffective feature boundaries are absent.

Savings should be ranked using **net consumer graph delta**, build cost, binary/install footprint & maintenance removed. “Exclusive subtree before replacement” is neither replacement saving nor workspace-wide saving.

**4. Optimality — designs I would approve**

| Proposed action | Better design / decision |
|---|---|
| Remove proven-unused dependencies | **Approve**, separately from optional-feature redesign. Keep Windows macro case gated on native compilation. |
| Replace base64, UUID & thiserror with handwritten code | **Reject as general optimization.** Existing callers still need encoding, identifiers & correct error chains. Removing dependencies by owning more correctness-sensitive code conflicts with maintenance goal. UUID replacement additionally risks persisted/wire identifier changes. |
| Replace nix with libc | **Defer.** First consolidate duplicated lock behavior behind tested fs APIs. Preserve POSIX record locks, descriptor registry & error semantics. Dependency count alone does not justify added unsafe surface. |
| Remove dirs | **Reject “std replacement” as stated.** Existing callers use platform data locations at `crates/rightkit-models/src/suite/paths.rs:99`. Preserve OS-specific path semantics; consolidate resolution policy before changing implementation. |
| Feature-gate fs serialization | **Approve with default compatibility.** Separate atomic/private/state primitives from serializable identity & owner metadata. Preserve existing public APIs under default feature; db opts out only after proving its full API needs no gated surface. |
| Consolidate atomic writes/locks/spawns | **Approve incrementally by semantics.** Filesystem owner handles durability, permission & replacement; process owner handles child lifetime. Crash hooks, intentionally detached launchers & process-owned locks need distinct policies, not universal wrappers. |
| Consolidate two models stacks | **Share low-level download/archive/locking first.** Retain revisioned-store versus suite-layout adapters until migration proven. One internal extraction validator is worthwhile; one forced public model-store abstraction is premature. |
| Centralize ORT | **Approve pin/resolution ownership.** Retain model-specific session shapes, provider options & graph workarounds. ASR has DirectML-specific behavior at `src/model_transducer.rs:55`; centralization must not erase it. |
| ASR/backend splitting | Prefer backend features within existing crate first. Pure decoder primitives should be usable without inference frameworks. Preserve new content-pipeline crates & their planned boundaries. |
| Embed split / merge vector store into db | **Feature-split inference from vector storage.** Keep vector semantics in embed or dedicated vector module. Reuse db’s connection/pin policy via existing `VectorStore::from_connection`; generic db should not own embedding algorithms. |
| Search into db; crash into logs | **Reject blanket merges.** Search parses/query-compiles; db owns persistence. Crash owns panic/native-dump lifecycle; logs owns records/transport. Optional adapters preserve responsibilities better. |
| Fold tauri into shell | **Reject Rust fold.** Keep small host-services crate independent of heavyweight shell. TypeScript can share command contracts without forcing package merger. Move app-specific configurations out gradually with compatibility factories. |
| Remove qts-ggml | **Approve phased forwarding-crate retirement.** Make QTS depend directly on sys; keep private wrapper only if actually used. Keep sys as native build/FFI owner. Deprecate published facade & migrate GenRight before source removal. |
| Pregenerate ggml/protobuf | **Approve with reproducible regeneration checks.** Bind generated code to header/schema/tool versions. Keep explicit supported-target matrix; unsupported targets fail clearly or use opt-in maintainer generation. |
| Turn defaults off everywhere | **Reject blanket change.** Published defaults are behavior/API. Add optional boundaries first, then select per consumer. Existing shell consumers already opt out; changing default offers them no win. |
| Agent-room HTTP split | **Defer until library-only consumer exists.** Default-compatible `http` feature is reasonable; server/CLI remains primary product. Keep cryptographic RNG failure propagation when replacing rand. |
| Replace MCP async HTTP with ureq | **Defer rewrite.** First make client transports/server hosting selectable where practical; preserve default compatibility. Consolidate blocking logs/models transport separately. Model download API currently publicly accepts `ureq::Agent` at `src/download.rs:183`, making replacement an API migration. |
| PDF split / OCR adoption | PDF deserves `edit/fonts`, `merge`, `extract` boundaries: merge-only users currently acquire font subsetting stack. OCR deserves `native`, `onnx`, worker boundaries. Preserve app authorization & proven backend policy. |
| Image crop optionality | Optional **peer** with explicit image-entry requirement, or separate image package if independent lifecycle justifies it. Do not assume optional dependency alone prevents installation. |
| doc-render removals/math | Remove unused mermaid/katex. Keep remark-math while exported AST/detector consumers need it; renderer injection or separate math adapter can provide typesetting later. |
| Updates peer & fixture exports | Compatible peer range plus dev dependency for tauri’s declaration tests. Add `updates/testing` subpath; retain old fixture export during migration. |
| Swift controller split | Move generic observable presentation layer into UI target only if consumers need that separation; keep networking/verification in License. Product strings & app policy remain injected. Resolve contradictory documentation explicitly. |
| Swift/Rust duplicate algorithms | **Keep native implementations**, share protocol vectors & behavioral fixtures. Neither FFI nor wholesale language consolidation is justified by matching responsibilities alone. |
| Agent-room MCP/secret storage | Keep TS protocol bridge unless executable consolidation has real benefit. Inject secure credential persistence or use narrow native helper; Rust crate is not directly consumable from TS. Preserve unattended resume requirements. |
| Computer, overlay, unused published crates | Remove computer after reference check. Keep overlay/cache/search/audio/crash/etc. pending concrete adoption/disposition; no-consumer declarations alone do not justify deletion. |
| Fake helper & Swift archive utility | Put fake helper behind explicit test-fixture target/feature. Remove/archive Swift utility only after confirming canonical release path covers its deterministic-archive purpose. |
| Public `notify` re-export | Preserve unless deliberate breaking API redesign is justified. Thin re-export can be valid adapter contract; replacing it with owned types adds conversion/maintenance. |

**RustRight routing**

R01–R10 belong with RustRight **for broker/client/policy infrastructure**. Report is broadly correct here, with shared-boundary corrections:

- `rightkit-versions.json`, app capability ownership catalogs & RightKit release semantics retain RightKit authority; RustRight consumes versioned snapshots/contracts.
- Release-side consumers of `managed-roots`/`managed-cargo` need supported RustRight interface, not undeclared relative imports.
- `scripts/install-right-ctl.sh` remains RightKit installer; its artifact selection fix must accompany RustRight output-contract migration.
- Broker-specific hook/lock/logger consolidation should happen inside RustRight design. Do not force reuse of application libraries merely because names overlap.
- Native-addon security finding remains RustRight-owned. Copying verified bytes only helps if loaded copy’s location/permissions prevent replacement.

**5. Shipping risks & required verification**

| Change family | Exposed consumers / boundary | Required evidence before adoption |
|---|---|---|
| MCP RNG/transport/API | CodeRight pins `=0.3.1` at `apps/coderight/engine/Cargo.toml:156`; current source now `0.5.0`. Source fix alone changes no installed app. | Entropy-failure tests; stdio/HTTP authorization; session renewal; SSE fragmentation; cancellation; response bounds; CodeRight adapter compatibility & installed smoke after explicit delivery. |
| ORT/QTS/ggml | GenRight QTS pins native layers at `apps/genright/crates/genright-qts/Cargo.toml:31`; ASR/VAD pipeline also affected. | Native macOS/Windows inference; packaged dynamic-library lookup; provider selection/fallback; model output parity; ABI/layout checks; offline consumer build. |
| fs/db/models | db exposes rusqlite publicly at `crates/rightkit-db/src/lib.rs:22`; SQLCipher providers explicitly differ at `Cargo.toml:18`. | Concurrent reader/writer & fork tests; owner permissions; interruption/rename failure; migration rollback; SQLite linkage compatibility; existing encrypted database open/rekey without provider change. |
| Windows consolidation | Shell/control select WebView2 generations; speech-client Windows declaration is dev-only. | Both supported WebView2 feature configurations; Job Object descendant cleanup; handle lifetime; pipe authentication; hotkey install/unregister; audio endpoint/sleep lifecycle on native Windows. |
| Feature/default/API removal | Published 0.x crates/packages; exact app pins; public re-exports & TypeScript declarations. | Minimal/default/relevant-feature builds; standalone packaged consumer checks; compatible deprecation or breaking release; explicit repins. Test feasible feature combinations, not mutually exclusive SQLCipher combinations together. |
| App-copy adoption | CutRight audio, ScrapeRight model storage, HeardRight telemetry/ASR, GenRight TTS, MailRight search/cache. | Donor/shared golden comparison; app-specific extras retained; persisted format/key migration; consent/privacy behavior; actual host integration. Manifest declaration alone is insufficient. |
| Swift changes | License controller, packaged vectors & native helper protocol. | macOS/iOS target builds; consumer API checks; resource inclusion; actor/concurrency behavior; native credential persistence & helper wire tests. |
| Tooling/install changes | right-ctl installer & RightKit/RustRight boundary. | Exact request-to-output binding; simultaneous builds; stale output rejection; native signing/install smoke; no reliance on source-tree relative imports in installed tooling. |

**Final ranked action list I would sign off on**

1. **Close MCP security delivery correctly:** validate existing fail-closed fix, identify affected consumer versions & deliver through compatible app migration. Do not implement duplicate fix.
2. **Fix right-ctl artifact identity:** consume exact completed-build output; keep installer work in RightKit & broker-output contract in RustRight.
3. **Fix search malformed-query behavior** & add regression distinguishing invalid input from legitimate empty query.
4. **Correct framing classification, then bound remaining paths:** MCP server/legacy client & TTS; inspect queue/backpressure separately from line-size caps.
5. **Wire focused verification before restructuring**, including fourth license vector, ownership tests & standalone package checks.
6. **Remove proven-unused dependencies:** ASR eyre, playback serde pair, overlay dev serde_json; Windows audio dependency after native check. Remove unused doc-render mermaid/katex.
7. **Finish real feature boundaries:** audio platform capture dependencies, TTS system/client/engine, embed inference/vector, OCR native/ONNX & PDF edit/fonts.
8. **Stop unintended ORT download-feature propagation**, supplying explicit runtime path/load mode & proving packaged inference.
9. **Adopt already-extracted primitives in apps**, starting with exact copies & near-identical CutRight audio. Preserve application policy & persisted formats.
10. **Consolidate archive/atomic-write correctness**, including destination-preservation failure cases; migrate locks & process launches only where semantics match.
11. **Reduce native build dependencies:** pregenerated ggml bindings & protobuf code, with regeneration/ABI/wire checks.
12. **Centralize ORT pin/resolution ownership** without flattening model-specific session/provider behavior.
13. **Apply measured package/dependency alignment:** updates peer, command constants, relevant SHA/getrandom/prost versions & app pins; count net consumer changes.
14. **Consolidate models’ duplicated internals**, retaining compatibility adapters for distinct store layouts.
15. **Retire rightkit-computer & phase out qts-ggml facade** after consumer migration; isolate fixture executables.
16. **Perform Windows consolidation separately**, preserving WebView2 generations & native lifetime semantics.
17. **Defer MCP transport replacement & speculative no-consumer splits** until consumer measurements justify risk.
18. **Reconcile inventory/publication documentation & retain intentional boundaries:** new content-pipeline crates stay; native Swift peers stay; broker infrastructure moves to RustRight; handwritten codec/error replacements leave approved backlog.