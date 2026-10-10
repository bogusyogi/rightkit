# RightKit crates audit, group 2 (read-only)

Scope: `/Volumes/D/claude/rightkit/crates/{rightkit-db, -embed, -framed-sidecar, -fs, -fsindex, -hotkeys, -http, -jobs, -license, -llm, -logs, -mcp, -media, -models}`.
Method: Read/Grep/`wc -l` only. No cargo, no builds. `rightkit-fsindex` was audited **as its working tree stands** (uncommitted: Cargo.toml, README, benches/footprint.rs, backend/mod.rs, col.rs, index.rs, names.rs, store.rs modified; CHANGELOG.md and LICENSE untracked). `cargo-machete` flagged nothing; every dependency below was checked by grep of `src/`, `tests/`, `benches/`, `examples/`.

Conventions:
- "pub items" = line-anchored `pub fn|struct|enum|trait|type|const|static` counts in `src/` (includes methods and items in private modules, so it over-counts the re-exported API slightly).
- "Publish" = Cargo `publish` key (none of the 14 sets `publish = false`; only fsindex sets `publish = true` explicitly; the rest are default-publishable). Registry state from `/Volumes/D/claude/rightkit/INDEX.md:17-45`.
- "In-repo dependents" = `[dependencies]`/`[dev-dependencies]` of `crates/*/Cargo.toml`, `desktop-app-template/**/Cargo.toml`. No `packages/*/package.json` names any of these crates (the JS packages are separate halves; `@rightkit/license` and `@rightkit/logs` are JS siblings, not dependents). Products outside this repo cannot be seen from here.
- Verdicts: needed / unused / feature-gate-it / replace-with / heavy.

## Cross-cutting findings (read first)

| # | Finding | Evidence |
|---|---|---|
| X1 | Two `sha2` majors in the lock: 0.10.9 (embed, mcp, jobs dev, ed25519-dalek) and 0.11.0 (fs, models dev). Five crates already reach SHA-256 through `rightkit_fs::digest`; embed and mcp bypass it. | `Cargo.lock:6450-6462`; embed `embedder.rs:10,683,706`; mcp `trust.rs:6,65` |
| X2 | Three HTTP stacks in the group: `ureq` direct (models, mcp client, logs upload), `rightkit-http` sync (llm), `reqwest` via `rightkit-http[async]` (mcp). `rightkit-http` is built on ureq, so models/mcp/logs bypass its timeouts, retry, redirects, redaction. | models `download.rs:184-297,532-543`, `suite/store.rs:1363`; mcp `client.rs:251,774`; logs `upload.rs:79-110` |
| X3 | Lock-file primitive implemented 4 times: `rightkit_fs::lock` (POSIX `F_SETLK` + in-process registry, `lock.rs:187,274,291-356`), `jobs::runner::try_lock_file` (`runner.rs:55-100`, same technique), `models::lock` (`lock.rs:93`, std `File::try_lock`), `models::suite::{lease.rs:239, refs.rs:72}` (std `try_lock`). | as cited |
| X4 | Atomic write (temp + fsync + rename) implemented 5 times: `rightkit_fs::atomic_write`; `logs::atomic::write_atomic` (`atomic.rs:14-61`); `models::store::write_atomic` (`store.rs:981`); `models::suite::manifest::write_atomic` (`manifest.rs:225`); `media::staging` (own tmp+rename, `staging.rs` doc lines 8-9). fsindex does bare `fs::rename` with no fsync (`index.rs:755,1424`). | as cited |
| X5 | Two-date-crate split: `time` 0.3 (jobs `store.rs:7-8,55`; qa) vs `chrono` 0.4 (logs `collect.rs:3,261`; models `rightapps.rs:495,498`). Each is a single call site. | as cited |
| X6 | `getrandom` split: 0.3 (http, mcp, models) vs 0.4 (logs, and `uuid` via jobs). Cargo.lock carries 0.2/0.3/0.4. | `Cargo.lock`; each Cargo.toml |
| X7 | Hardware/OS probing duplicated: `models::hardware` (macOS `sysctl` subprocess, kernel32 `GlobalMemoryStatusEx` FFI, `system_profiler`, `reg`, `nvidia-smi`; `hardware.rs:402-565`) vs `logs::system::{identity,metrics}` (`sysctlbyname`, windows-sys `GlobalMemoryStatusEx`; `identity.rs:151-220`, `metrics.rs:75-119`). | as cited |
| X8 | Hand-rolled Windows FFI vs `windows-sys` 0.61.2: fs (`lib.rs:73`, `private.rs:202-332`, `identity.rs:109,479`, `state/windows.rs:98-139`), db (`host.rs:70-84`), models (`hardware.rs:423`) declare `extern "system"` by hand; hotkeys and logs use windows-sys. | as cited |
| X9 | pid-alive implemented in `fs::process_alive` (`lib.rs:64`) and 6 more places outside this group (automation, qa, sandbox x2, service x2, speech-client). | grep `fn process_alive\|pid_alive\|is_alive` |
| X10 | Bounded newline-JSON framing duplicated: `framed-sidecar` (`lib.rs`) vs `mcp::client::read_stdio_frame` (`client.rs:272-300`) and `mcp::server` bounded line reader (`server.rs:281`). | as cited |
| X11 | Metadata drift: fsindex `repository`/`homepage` point at `bogusyogi/claude` (all others `bogusyogi/rightkit`), license is `MIT` only (others `MIT OR Apache-2.0`), and fsindex has no row in INDEX.md. mcp declares `MIT OR Apache-2.0` but ships no LICENSE-* files and its `include` omits them. INDEX.md "Source" versions are stale vs Cargo.toml for fs (0.3.1 vs 0.3.3), http (0.2.2 vs 0.2.4), mcp (0.5.1 vs 0.5.2). hotkeys and llm: INDEX flag "public package has no release record". Root `[workspace.dependencies]` (serde, serde_json) is used by none of these crates (all pin their own). | `crates/rightkit-fsindex/Cargo.toml`, `rightkit-mcp/` dir listing, `INDEX.md:33-45`, root `Cargo.toml` |

---

## rightkit-db

### Profile
| Field | Value |
|---|---|
| Purpose | Correct-by-default SQLite/SQLCipher connection bootstrap, migrations, backup, recovery. |
| Version / publish | 0.3.2; publishable (no `publish` key); INDEX: published 0.3.2 ok. |
| src LOC (1,581) | lib.rs 750, migrate.rs 419, backup.rs 172, recover.rs 128, host.rs 112 |
| Pub API | 47 fn, 10 struct, 6 enum, 1 trait, 6 type/const |
| Features | default `["bundled"]`; `bundled`; `sqlcipher` (vendored OpenSSL, implies `cipher`); `sqlcipher-system` (implies `cipher`); `cipher` (internal gate); `hooks` (pure passthrough to `rusqlite/hooks`, no code in crate uses it) |
| In-repo dependents | `rightkit-jobs` (normal). `desktop-app-template` has it commented out (`src-tauri/Cargo.toml:39`, `crates/engine/Cargo.toml:17`). |

### Dependencies
| Dep | Spec | Optional | Use (src hits / files) | Verdict | Reason |
|---|---|---|---|---|---|
| rusqlite | 0.40.1, no defaults, `backup` | no | 21 / 4 (+3 test files) | needed | Core. `bundled` default compiles SQLite from C; sqlcipher features swap provider. Re-exported `pub use rusqlite` (`lib.rs:22`), so version is public API. |
| rightkit-fs | 0.3.0 path, `fs` only | no | 10 / 4 | needed | `atomic_write`, `restrict_owner_only`, `sync_dir`, `TEMP_MARKER`, `is_network_filesystem`, `FsError`. Already minimal feature set. |
| thiserror | 2 | no | 1 / 1 (`lib.rs:41`) | needed | One derive; also pulled by fs `fs` feature, zero marginal cost. |
| zeroize | 1.8 | no | 2 / 2 (+2 tests) | needed | `Zeroizing` appears in public `key`/`raw_key` fields (`lib.rs:125-127`) and `recover.rs:38`. |
| libc (unix) | 0.2 | no (cfg) | 1 / 1 (`host.rs:60` `gethostname`) | needed | Tiny; fs already enables libc on unix. |
| tempfile (dev) | 3 | - | 4 test files | needed | |

Counts: needed 6 (5 + 1 dev), others 0.

### Overlap / split notes
- **db vs embed (SQLite):** `embed::vector::VectorStore::open` calls plain `rusqlite::Connection::open` (`vector.rs:36-37`): no WAL, busy timeout, owner-only file, network-fs check, migration table. It does expose `from_connection`, so the intended wiring (db-bootstrapped connection into the vector store) works, but embed pins its own rusqlite 0.40.1 and does not depend on db. Same pattern in `rightkit-search` and `agent-room-core` (own rusqlite).
- **db vs jobs:** jobs re-exports `rightkit_db::rusqlite` (`jobs/lib.rs:13`) and wraps db errors; jobs also re-implements busy-retry backoff (`jobs/store.rs:1517-1540`) although db owns `busy_timeout` (`lib.rs:121-150`).
- `host.rs` (112 LOC, per-host DB path suffix) is the only module unrelated to bootstrap; fine as is.
- Split/merge: no. Cohesive.

---

## rightkit-embed

### Profile
| Field | Value |
|---|---|
| Purpose | Local ONNX text embeddings (EmbeddingGemma) plus SQLite cosine vector store. |
| Version / publish | 0.3.1; publishable; INDEX: staged 0.3.1, published 0.3.0. |
| src LOC (1,221) | embedder.rs 958, vector.rs 246, lib.rs 17 |
| Pub API | 31 fn, 4 struct, 3 enum, 0 trait, 7 type/const |
| Features | default `["inference","vector"]`; `inference` = rightkit-ort + tokenizers + sha2 + serde_json; `vector` = rusqlite |
| In-repo dependents | none (only referenced by its own tests/README and ownership catalogs). |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-ort | 0.2.2 path | yes (inference) | 22 / 1 | needed | Session/env/EP setup. |
| tokenizers | 0.22.2, no defaults, `onig` | yes (inference) | 2 / 2 | heavy | Needed, but pulls onig C build + regex/rayon tree for one `Tokenizer::from_file` + `encode_batch`. rightkit-asr uses the same spec, so the cost is shared; no lighter option for HF tokenizer.json. |
| sha2 | 0.10.9 | yes (inference) | 1 / 1 (`embedder.rs:10,683,706,718`) | replace-with `rightkit_fs::digest` | Only `Sha256::digest(text)` and a file hash loop (`hash_file`, `embedder.rs:703-713`). `sha256_bytes`/`sha256_file` cover both and collapse the 0.10/0.11 split (X1). |
| serde_json | 1.0.150 | yes (inference) | 1 / 1 (`embedder.rs:719`) | needed | Reads `config.json` `model_type`. tokenizers already depends on it: zero marginal cost. |
| rusqlite | 0.40.1, no defaults | yes (vector) | 2 / 1 | needed | No `bundled` feature on the normal dep: a `vector`-only consumer must supply it (dev-dep adds `bundled`, hiding this in tests). |
| thiserror | 2 | **no** | 2 / 2 | feature-gate-it | Non-optional but both users are behind `inference`/`vector`; with both off the crate is empty yet still builds thiserror. Trivial. |
| rusqlite (dev) | `bundled` | - | 1 test | needed | |
| tempfile (dev) | 3 | - | 1 test + `embedder.rs:933` (in `cfg(test)` from 774) | needed | |

Counts: needed 5, replace-with 1 (sha2), feature-gate-it 1 (thiserror), heavy 1 (tokenizers).

### Overlap / split notes
- **db vs embed:** see rightkit-db. `vector.rs` (246 LOC, rusqlite only) and `embedder.rs` (958 LOC, ort+tokenizers) share nothing but the crate; features already isolate them. Splitting into `rightkit-embed` + `rightkit-vectorstore` is justified only if the vector store gains non-embedding users; no in-repo dependents today, so low priority.
- **embed vs models:** embed hashes model files itself (`hash_file`) and encodes bundle layout (`bundle_is_gemma2`, `embedder.rs:716-724`); models owns "revisioned verified model files". Digest should come from `rightkit_fs::digest`.
- No in-repo dependents: embed is currently orphaned inside the workspace (consumers must be external apps).

---

## rightkit-framed-sidecar

### Profile
| Field | Value |
|---|---|
| Purpose | Bounded newline-JSON and length-prefixed-JSON framing plus one typed wire envelope for sidecars. |
| Version / publish | 0.1.0; publishable; INDEX ok. |
| src LOC (635) | lib.rs 385, wire.rs 150, length_prefixed.rs 100 |
| Pub API | 17 fn, 3 struct, 3 enum, 2 type/const |
| Features | none |
| In-repo dependents | jobs, qa, service, record, speech-client (all normal); `desktop-app-template/crates/protocol` (`=0.1.0`); `rightkit-shell/example-app`. |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| serde | 1.0.228 derive | no | 19 / 3 | needed | |
| serde_json | 1.0.150 | no | 8 / 3 | needed | Core of the format. |

No dev-dependencies. Counts: needed 2.

### Overlap / split notes
- **Underused:** `mcp` re-implements bounded line reading (X10) instead of `read_json_line`/`ReadLimits`; MCP stdio does have extra needs (startup-prefix skipping `StdioPrefixTooLarge`, `client.rs:272-300`), but the bounded read + discard-oversize core is the same.
- Keep as is; smallest crate in the group and correctly minimal.

---

## rightkit-fs

### Profile
| Field | Value |
|---|---|
| Purpose | Safe state dirs, atomic writes, cross-process locks, owner-only files, stable file identity, SHA-256 helper, macOS clonefile/swap. |
| Version / publish | 0.3.3; publishable; INDEX: source 0.3.1 (stale), published 0.3.0, staged 0.3.1. |
| src LOC (3,953) | state.rs 766 (+ state/windows.rs 632, state/windows_tests.rs 336), identity.rs 505, lock.rs 455, private.rs 384, atomic.rs 292, clone.rs 128, lib.rs 97, digest.rs 95, netfs.rs 71, error.rs 70, swap.rs 55, bin/rightkit-swap.rs 35, bin/rightkit-clonefile.rs 32 |
| Pub API | 106 fn, 16 struct, 4 enum, 0 trait, 5 type/const |
| Features | default `["fs","lock","json","digest"]`; `fs` (libc, thiserror); `lock` (implies fs); `json` (implies lock, serde, serde_json: owner records, identity ledger, `rightkit-clonefile` bin); `digest` (sha2, independent of fs) |
| In-repo dependents | db (`fs`), http / jobs / media / models / qa (all `default-features=false, features=["digest"]`). No in-repo crate uses `lock`, `json`/identity, or `StateDir`. |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| libc (unix) | 0.2 | yes (fs) | 75 / 8 | needed | `fcntl`, `statfs`, `*at` calls, `O_NOFOLLOW`; `state.rs` alone 46 refs. |
| thiserror | 2 | yes (fs) | 2 / 2 | needed | Two error derives (`error.rs:6`, `clone.rs:12`); hand-written `Display` would save a proc-macro dep, marginal. |
| serde | 1 derive | yes (json) | 2 / 2 | needed | `LockOwner`, identity ledger. |
| serde_json | 1 | yes (json) | 6 / 3 | needed | Owner records (`lock.rs:413,425`), ledger (`identity.rs:247,260`). |
| sha2 | 0.11 | yes (digest) | 1 / 1 | needed | `digest.rs`. 0.11 vs the 0.10 used elsewhere (X1). |
| tempfile (dev) | 3 | - | 5 tests; `digest.rs:78` in cfg(test) | needed | |

Counts: needed 6.

### Overlap / split notes
- **fs vs jobs vs logs (atomic write/lock/digest):** fs is the canonical owner and is under-adopted. jobs copies the lock (`runner.rs:55-100`, X3); logs copies atomic write (`logs/atomic.rs`, X4); models copies both (2 atomic, 3 lock). Digest is the only fs feature most siblings use.
- **Digest-only consumers:** 5 crates (http, jobs, media, models, qa) take fs solely for `digest` (one call site each: `http/untrusted.rs:34`, `jobs/store.rs:1501`, `media/lib.rs:645`, `models/hash.rs:10` + `suite/manifest.rs:346,351`). The feature gate already prevents extra deps, so a separate `rightkit-digest` crate is optional; the real cost is the sha2 version split (X1) and conceptual coupling.
- **Split candidates:** (a) macOS `clone.rs` + `swap.rs` + the two diagnostic bins (~250 LOC, no relation to the rest); (b) `state/windows.rs` + `private.rs` Windows halves use hand-written FFI (X8). Neither is large enough to justify its own crate; feature-gating (a) behind `macos-fs` is the cheaper move.
- `process_alive` in `lib.rs:64` is a general process helper that belongs with rightkit-process (X9).

---

## rightkit-fsindex  (WORKING TREE, uncommitted changes)

### Profile
| Field | Value |
|---|---|
| Purpose | Live whole-disk file index: fuzzy name search plus clone/hard-link-aware folder-size rollups on mmap'd columns; macOS FSEvents backend. |
| Version / publish | 0.1.0; `publish = true` explicit; license `MIT`; not in INDEX.md; repository URL differs from siblings (X11). |
| src LOC (4,476) | index.rs 1,500, store.rs 1,090, backend/macos.rs 569, query.rs 399, names.rs 213, backend/mod.rs 162, config.rs 150, content.rs 144, col.rs 125, backend/windows_stub.rs 55, error.rs 37, lib.rs 32 |
| Pub API | 84 fn, 28 struct, 6 enum, 2 trait, 23 type/const |
| Features | default `[]`; `content` (opt-in trigram search, module `content.rs` and ~14 `cfg` sites in index.rs/config.rs) |
| In-repo dependents | none. Has no rightkit-* dependencies either (fully standalone). |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| memchr | 2 | no | 7 / 2 (query.rs 6, content.rs 1) | needed | Hot-path byte search (`query.rs:230-282`); `memmem` in content.rs:143 is gated with `content`. |
| memmap2 | 0.9 | no | 3 files (col.rs, store.rs, names.rs) | needed | Core storage model. |
| libc (unix) | 0.2 | no (cfg unix) | 39 / 1 (`backend/macos.rs` only) | feature-gate-it | Declared `cfg(unix)` but every use is inside the macOS backend (`backend/mod.rs:13` `cfg(target_os="macos")`). Narrow to `cfg(target_os = "macos")`. |
| tempfile (dev) | 3 | - | 4 test/bench files | needed | |

Counts: needed 3, feature-gate-it 1.

### Overlap / split notes
- **Platform half:** `backend/macos.rs` (569 LOC, FSEvents + getattrlistbulk) vs an `UnsupportedBackend` stub for everything else (`windows_stub.rs`); the `Backend` trait is the seam. A split into `rightkit-fsindex` (engine) + `rightkit-fsindex-macos` is possible but not needed until a Windows backend exists.
- **vs rightkit-fs:** no dependency, so index persistence swaps files with plain `fs::rename` (`index.rs:755,1424`), no fsync, no owner-only permissions, no `is_network_filesystem` guard; `fs::identity` (stable file identity) is a different problem (dev/ino ledger) and does not overlap.
- Needs license/metadata alignment before publish (X11); LICENSE and CHANGELOG are untracked in the working tree.

---

## rightkit-hotkeys

### Profile
| Field | Value |
|---|---|
| Purpose | Chord parsing, conflict registry, push-to-talk state machine, macOS CGEventTap/Carbon and Windows WH_KEYBOARD_LL/RegisterHotKey primitives. |
| Version / publish | 0.1.0; publishable; INDEX: "MISMATCH public package has no release record". |
| src LOC (2,455) | chord.rs 310, ptt.rs 318, macos/fn_hotkey.rs 476, macos/tap.rs 252, macos/ffi.rs 237, macos/global.rs 188, macos/synth.rs 154, macos/middle_mouse.rs 123, windows/mod.rs 268, conflict.rs 73, lib.rs 26, decision.rs 16, macos/mod.rs 14 |
| Pub API | 99 fn, 23 struct, 17 enum, 0 trait, 67 type/const (large: many key/modifier constants) |
| Features | none |
| In-repo dependents | none. |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| windows-sys (windows) | 0.61.2, 5 features (Foundation, LibraryLoader, Threading, KeyboardAndMouse, WindowsAndMessaging) | no (cfg) | 5 / 1 (`windows/mod.rs:13-24`) | needed | Every listed feature is used; list is already minimal. Same version as logs. |

macOS side has zero crates (hand-rolled `ffi.rs` with `#[link]` to CoreGraphics/CoreFoundation/ApplicationServices/Carbon/AppKit). Counts: needed 1.

### Overlap / split notes
- **CGEvent FFI x3:** `hotkeys/macos/ffi.rs` (`CGEventTapCreate:33`, `CGEventPost:59`), `automation/macos/ffi.rs:97-98` + `events.rs`, `control/mac.rs:64,211-299` each declare CoreGraphics event bindings. A shared macOS CG sys module would remove the triplication; hotkeys additionally has `synth.rs` synthetic input that overlaps `automation::events`.
- Platform halves are cfg-gated modules in one crate and compile independently; pure modules (`chord`, `conflict`, `ptt`, `decision`; ~720 LOC) are reusable everywhere. No split needed.
- Orphan: no in-repo consumer and no release record; confirm an external consumer exists before maintaining.

---

## rightkit-http

### Profile
| Field | Value |
|---|---|
| Purpose | Blocking (ureq) and optional async (reqwest) HTTP client with timeouts, retry/Retry-After, SSE, redacted secrets, untrusted-text wrapping, mock server. |
| Version / publish | 0.2.4; publishable; INDEX: source 0.2.2 (stale), published 0.2.1, staged 0.2.2. |
| src LOC (1,844) | client.rs 503, async_client.rs 458, testing.rs 284, sse.rs 214, untrusted.rs 107, secret.rs 97, retry.rs 83, error.rs 62, lib.rs 36 |
| Pub API | 77 fn, 21 struct, 3 enum, 1 trait, 5 type/const |
| Features | default `["gzip"]`; `gzip` (ureq/gzip, reqwest?/gzip); `async` (reqwest, tokio, futures-util, bytes); `testing` (no deps) |
| In-repo dependents | llm (default features), mcp (`async`, no defaults), oauth (normal + dev `testing`), daemon (dev `testing`). |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-fs | 0.3 path, digest only | no | 1 / 1 (`untrusted.rs:34`) | feature-gate-it | Sole purpose is the sha256 label inside `wrap_untrusted_web_text`. HTTP clients should not need fs/sha2; gate behind an `untrusted` feature or move the module (see below). |
| serde | 1.0.228 derive | no | 3 / 2 | needed | |
| serde_json | 1.0.150 | no | 6 / 2 (client.rs 2, testing.rs 4) | needed | |
| ureq | 3.4, no defaults, `rustls` | no | 8 / 1 (`client.rs`) | needed | Blocking transport. |
| getrandom | 0.3 | no | 1 / 1 (`retry.rs:70`) | replace-with std | Only jitter (`random_unit`). `std::hash::RandomState` or `SystemTime` nanos is adequate for backoff jitter and removes a crate and the 0.3/0.4 skew (X6). |
| reqwest | 0.12, `rustls-tls`, `stream` | yes (async) | 16 / 2 | heavy | Hyper + h2 + tokio + rustls tree for the async client. Correctly optional, but mcp enables it unconditionally. Lock also contains reqwest 0.13.5 from another crate (two majors). |
| tokio | 1, `time` | yes (async) | 5 / 2 | needed | `sleep`/`timeout` only. |
| futures-util | 0.3 | yes (async) | 1 / 1 | needed | `StreamExt`. |
| bytes | 1 | yes (async) | 3 real / 1 | needed | `BodyStream::Item = Bytes` is public. |
| tokio (dev) | rt, macros, time | - | 3 tests | needed | |
| rightkit-http (dev, self) | `testing` | - | tests | needed | Standard self-dev-dep trick. |

Counts: needed 8, feature-gate-it 1 (rightkit-fs), replace-with 1 (getrandom), heavy 1 (reqwest, already optional).

### Overlap / split notes
- **http vs mcp vs llm vs models (HTTP clients):** see X2. mcp has *two* transports: sync ureq `HttpConn` (`client.rs:251,774-800`, `max_redirects(0)`, same intent as http's `ClientConfig`) and async via http (`advanced_http.rs:9`). models has its own `ureq::Agent` configuration and re-implements status classification (`download.rs:277-297,505-515`) and backoff (`download.rs:517-521`) that http's `RetryPolicy` already provides. logs `HttpTransport` is a 25-line ureq POST with `Retry-After` parsing (`upload.rs:96-117`) that duplicates `parse_retry_after`.
- **Split candidates (≥2 independent responsibilities):** `untrusted.rs` (prompt-injection wrapping, 107 LOC) and `secret.rs` (redacted `Secret` + `SecretStore`, 97 LOC) are unrelated to HTTP transport; they belong in llm/agent-tools or a small `rightkit-secret`. `mcp::auth::Secret` (`auth.rs:5-20`) is a byte-for-byte duplicate of `rightkit_http::Secret` although mcp already depends on http. `testing.rs` (mock server, 284 LOC) duplicates `rightkit-qa::mock`, which models uses (dev) for Range/dropped-connection tests.

---

## rightkit-jobs

### Profile
| Field | Value |
|---|---|
| Purpose | SQLite job store, runner with per-job locks, stdio/CLI command contract, engine client and supervisor. |
| Version / publish | 0.2.6; publishable; INDEX: staged 0.2.6, published 0.2.5. |
| src LOC (5,388) | store.rs 1,540, contract.rs 940, supervisor.rs 933, runner.rs 623, client.rs 554, config.rs 517, error.rs 99, sched.rs 88, service.rs 57, lib.rs 37 |
| Pub API | 222 fn, 34 struct, 7 enum, 4 trait, 20 type/const |
| Features | none |
| In-repo dependents | `rightkit-shell` (optional dep). `desktop-app-template` has it commented out as "UPCOMING" (`src-tauri/Cargo.toml:40`). |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-fs | 0.3, digest only | no | 1 / 1 (`store.rs:1501`) | needed | Hash helper. Should also supply the lock (see libc). |
| rightkit-db | 0.3.0 path (default `bundled`) | no | 12 / 4 | needed | Store bootstrap; re-exports rusqlite. |
| rightkit-framed-sidecar | 0.1.0 | no | 2 / 2 | needed | `read_json_line`, `write_json_line`. |
| rightkit-process | 0.3.0 | no | 2 / 2 | needed | `OwnedChild`, restart/health state machines in client/supervisor. |
| rightkit-service | 0.1.0 | no | 2 / 1 (`service.rs`, 57 LOC) | feature-gate-it | Only the `DispatcherHandler` adapter uses it; a `service` feature would let store/runner-only users skip it. |
| serde | 1.0.228 derive | no | 6 / 1 | needed | |
| serde_json | 1.0.150 | no | 18 / 9 | needed | |
| time | 0.3, `formatting`,`macros` | no | 14 / 6 (1 real call: `store.rs:55-60` `format_description!`) | replace-with | One RFC3339-style timestamp. Consolidate on one date crate across the repo (logs/models use chrono, qa uses time) or hand-format from `SystemTime` (X5). |
| uuid | 1, `v4` | no | 3 / 1 (`store.rs:436,1189,1203`) | replace-with | Random ids only. Pulls `getrandom 0.4.3` (+js-sys/wasm-bindgen on wasm). 16 random bytes via the already-used `getrandom` would do (X6). |
| libc (unix) | 0.2 | no (cfg) | 8 / 1 (`runner.rs:65-90`) | replace-with `rightkit_fs::FileLock` | Hand-written POSIX record lock with in-process held-path registry; fs `lock.rs` implements exactly this (X3). Removes the crate's only `unsafe` (`runner.rs:56` `allow(unsafe_code)` against `#![deny(unsafe_code)]`). |
| sha2 (dev) | 0.10 | - | `tests/genright.rs:14` | needed | Intentional independent reference; but 0.10 vs fs's 0.11 gives two sha2 builds (X1). |
| tempfile (dev) | 3 | - | 8 tests | needed | |

Counts: needed 8, feature-gate-it 1, replace-with 3.

### Overlap / split notes
- **Two independent responsibilities (split candidate):** (1) job persistence: `store.rs`, `runner.rs`, `sched.rs`, `config.rs` (~2,770 LOC; deps db, fs, serde, time, uuid) and (2) sidecar engine hosting: `contract.rs`, `client.rs`, `supervisor.rs`, `service.rs` (~2,480 LOC; deps framed-sidecar, process, service). They share only `JobCommands` plumbing in `contract.rs`. rightkit-shell consumes it as one optional dep today; the second half overlaps `rightkit-service`/`rightkit-process` lifecycle code (supervisor.rs docs describe it as "the shared form of five live copies").
- **fs vs jobs vs logs:** jobs owns a lock copy (X3) and a SQLite busy backoff copy (`store.rs:1517-1540`) beside db's `busy_timeout`.

---

## rightkit-license

### Profile
| Field | Value |
|---|---|
| Purpose | Offline verification of server-signed Ed25519 entitlement tokens (Rust half of `@rightkit/license`). |
| Version / publish | 0.1.3; publishable; INDEX ok. |
| src LOC (510) | lib.rs 510 (single file; unit tests start `lib.rs:363`) |
| Pub API | 13 fn, 2 struct, 3 enum |
| Features | none |
| In-repo dependents | `rightkit-tauri` (normal). |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| ed25519-dalek | 2 (default features) | no | 2 / 1 | needed | `Verifier`/`VerifyingKey`. Tests also use `SigningKey`/`Signer` from the same dep (`lib.rs:365`). |
| base64 | 0.22 | no | 3 / 1 | needed | base64url no-pad. |
| serde | 1 derive | no | 8 / 1 | needed | |
| serde_json | 1 | no | 1 / 1 | needed | |

No dev-dependencies. Counts: needed 4.

### Overlap / split notes
- **license vs models::rightapps:** `models/rightapps.rs:5-6` also decodes base64url and verifies Ed25519 (`VerifyingKey`, `Signature`) for runtime-artifact envelopes behind the `rightapps` feature. Two independent signature-verification implementations; a shared "verify signed envelope" helper is the merge candidate (low priority).
- Small, single-purpose; keep as is.

---

## rightkit-llm

### Profile
| Field | Value |
|---|---|
| Purpose | BYOK Anthropic Messages + OpenAI-compatible chat clients, streaming, tool use, failover chain with circuit breaker, pricing, prompt-cache ledger. |
| Version / publish | 0.1.0; publishable; INDEX: "MISMATCH public package has no release record". |
| src LOC (1,627) | anthropic.rs 410, openai.rs 331, failover.rs 235, types.rs 186, pricing.rs 106, cache.rs 102, error.rs 91, presets.rs 73, credentials.rs 49, lib.rs 29, provider.rs 15 (+ `assets/model-prices.json` embedded at `pricing.rs:10`) |
| Pub API | 44 fn, 20 struct, 8 enum, 2 trait, 5 type/const |
| Features | none |
| In-repo dependents | none (README and ownership catalog only). |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-http | 0.2.0 path, default (`gzip`) | no | 6 / 6 | needed | Uses sync `Client`, `HttpError`, `Secret`. Gets `untrusted` + `fs` + `sha2` transitively without using them. |
| serde | 1.0.228 derive | no | 8 / 2 | needed | |
| serde_json | 1.0.150 | no | 8 / 4 | needed | |
| rightkit-http (dev) | `testing` | - | `tests/e2e.rs` | needed | |

Counts: needed 4.

### Overlap / split notes
- Cleanest dependency set in the group. The only overlap is inbound: `http::untrusted` (prompt-injection wrapping) is LLM-input hygiene and would fit llm better than http; moving it would also let `http` drop `rightkit-fs`.
- No in-repo dependents and no release record: confirm external consumers.

---

## rightkit-logs

### Profile
| Field | Value |
|---|---|
| Purpose | Privacy-safe local JSONL logging, consent store, consented outbox uploader, diagnostics bundle collection, system identity/metrics. |
| Version / publish | 0.2.0; publishable; INDEX ok. |
| src LOC (2,665) | upload.rs 476, system/metrics.rs 370, collect.rs 367, outbox.rs 331, system/identity.rs 246, scrub.rs 197, testing.rs 129, store.rs 109, consent.rs 82, writer.rs 81, redact.rs 74, atomic.rs 61, event.rs 44, identity.rs 44, lib.rs 42, system/mod.rs 12 |
| Pub API | 61 fn, 26 struct, 5 enum, 1 trait, 3 type/const |
| Features | default `["http"]`; `http` (ureq HTTPS transport); `testing` (mock ingest server) |
| In-repo dependents | `rightkit-crash` (`default-features=false`; dev `testing`), `desktop-app-template/src-tauri` (`=0.2.0`). |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| chrono | 0.4, no defaults, `std` | no | 1 / 1 (`collect.rs:261` `parse_from_rfc3339`) | replace-with | Single timestamp parse; jobs uses `time` for formatting (X5). Pick one date crate repo-wide or parse with a tiny hand parser. |
| serde | 1 derive | no | 12 / 5 | needed | |
| serde_json | 1 | no | 21 / 10 | needed | |
| ureq | 3.4, `rustls` | yes (http, default) | 2 / 1 (`upload.rs:79-110`) | replace-with `rightkit-http::Client` | Optional and gated already, but bypasses http's timeout/retry/redaction (X2). Adopting rightkit-http would add fs+sha2+serde to a crate meant to be dependency-light, so weigh before moving; at minimum keep gated. |
| getrandom | **0.4** | no | 1 / 1 (`identity.rs:11`, uuid bytes) | replace-with (align) | Only crate on 0.4 among direct deps (X6). Also implements `random_uuid` by hand while jobs pulls `uuid`. |
| objc2-foundation (macos) | 0.3, default features | no (cfg) | 1 / 1 (`system/metrics.rs:201-224`: `physicalMemory`, thermal state) | heavy | Two Objective-C calls justify an objc2 + block2 tree, and `identity.rs:172-178` already reads `hw.memsize` through `libc::sysctlbyname`. Replace physical memory with sysctl and thermal state with a narrow `objc2`/sysctl call, or minimize features. (Feature surface not verified without cargo.) |
| libc (macos) | 0.2 | no (cfg) | 36 / 2 | needed | host_statistics/sysctl in metrics/identity. |
| windows-sys (windows) | 0.61.2, 5 features (Foundation, ProcessStatus, Registry, SystemInformation, Threading) | no (cfg) | 6 / 2 | needed | All features used. |
| rightkit-logs (dev, self) | `testing` | - | tests | needed | |

Counts: needed 5 (serde, serde_json, libc, windows-sys, dev self), replace-with 3 (chrono, ureq, getrandom align), heavy 1 (objc2-foundation). Note: `#![deny(unsafe_code)]` at `lib.rs:1` while `system/` uses libc/windows-sys unsafe blocks (must carry `allow`; not audited per site).

### Overlap / split notes
- **fs vs jobs vs logs (atomic write):** `atomic.rs` (61 LOC, `write_atomic`: temp name + `sync_all` + rename + parent fsync) is `rightkit_fs::atomic_write` minus Apple `durable_sync`; used by consent, identity, outbox (`consent.rs:80`, `identity.rs:42`, `outbox.rs:261`). logs does not depend on fs; fs `fs` feature is only libc+thiserror, so adopting it is cheap.
- **Split candidate:** `system/` (identity + metrics, 628 LOC, all platform FFI) is independent of logging and duplicates `models::hardware` (X7). A shared `rightkit-sysinfo` would serve logs, models, and crash.
- `upload.rs` (consented uploader, 476 LOC) plus `HttpTransport` is a second responsibility (network) already feature-gated via `http`.

---

## rightkit-mcp

### Profile
| Field | Value |
|---|---|
| Purpose | MCP client (stdio, streamable HTTP, legacy SSE), trust gate, connection pool, OAuth hook, and server host (stdio + loopback HTTP, per-tool admission). |
| Version / publish | 0.5.2; publishable; INDEX: source 0.5.1 (stale), published 0.5.1. No LICENSE-* files in the crate dir despite `MIT OR Apache-2.0` (X11). |
| src LOC (5,161) | client.rs 1,427, server.rs 1,459, advanced_http.rs 939, client_features.rs 287, types.rs 230, pool.rs 226, trust.rs 177, error.rs 115, events.rs 98, config.rs 96, lib.rs 40, sanitize.rs 36, auth.rs 31 |
| Pub API | 93 fn, 36 struct, 12 enum, 6 trait, 6 type/const |
| Features | default `[]`; `exact-json-ids` (serde_json/arbitrary_precision). README states "no other Cargo features". |
| In-repo dependents | `rightkit-agent-tools` (optional). |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| getrandom | 0.3 | no | 7 / 1 (`server.rs`, session id + loopback token) | needed | Security tokens must come from the OS RNG. Same major as http/models. |
| serde | 1 derive | no | 23 / 3 | needed | |
| serde_json | 1 | no | 31 / 10 | needed | |
| sha2 | 0.10 | no | 1 / 1 (`trust.rs:6,65`) | replace-with `rightkit_fs::digest::sha256_bytes` | Single fingerprint hash; collapses the 0.10/0.11 split (X1) at the cost of an fs dep (digest feature only). |
| ureq | 3.4, `rustls` | no | 3 / 1 (`client.rs:251,774,824`) | replace-with `rightkit_http::Client` | Legacy HTTP transport; mcp already depends on rightkit-http for the other transport. Removes the direct ureq dependency and a second redirect/timeout policy. |
| rightkit-http | 0.2.3, no defaults, `async` | no | 1 / 1 (`advanced_http.rs:9`) | heavy | Forces reqwest + hyper + h2 + rustls tree for every consumer, including stdio-only ones. Gate the streamable-HTTP client (`advanced_http.rs`, 939 LOC) behind a feature. |
| http | 1 | no | 16 / 6 (real: `HeaderMap`, `StatusCode`; `advanced_http.rs:8,661`) | feature-gate-it | Only the advanced HTTP path. |
| tokio | 1, `rt-multi-thread`,`time`,`sync`,`macros` | no | 15 / 2 (`advanced_http.rs` 13, `client_features.rs` 2) | heavy | Builds a private multi-thread runtime inside a sync API (`advanced_http.rs:307-310`, `new_multi_thread().worker_threads(1)`); `macros` only for `select!`/`pin!`. `client_features.rs` uses `tokio::sync::Notify`/`pin!` just for cancellation tokens (could be `Condvar`/`AtomicBool` to decouple). |
| futures-util | 0.3 | no | 1 / 1 (`advanced_http.rs:7`) | feature-gate-it | `StreamExt` only. |
| url | 2 | no | 1 production use (`client_features.rs:94` `Url::parse`; the other counted hits are the word "url") | needed | Endpoint origin policy; already in tree through reqwest, zero marginal cost. |
| tempfile (dev) | 3 | - | 3 tests | needed | |

Counts: needed 5 (getrandom, serde, serde_json, url, dev tempfile), replace-with 2 (sha2, ureq), heavy 2 (rightkit-http[async], tokio), feature-gate-it 2 (http, futures-util).

### Overlap / split notes
- **http vs mcp:** see X2; plus `mcp::auth::Secret` duplicates `rightkit_http::Secret` (`auth.rs:5-20` vs `http/secret.rs`), and mcp keeps its own `AuthProvider` returning that type, so apps holding a `SecretStore` must convert.
- **framing:** see X10 (read_stdio_frame, server line reader).
- **Split candidate (≥2 responsibilities):** client side (client, client_features, advanced_http, pool, trust, config; ~3,260 LOC) vs server host (server.rs 1,459 LOC, loopback TcpListener HTTP with hand-parsed HTTP/1.1 at `server.rs:1193,1309`). The server half needs none of ureq/reqwest/tokio; today both halves ship together and agent-tools only uses the client. Splitting into `rightkit-mcp-client` / `rightkit-mcp-server` (or `client`/`server` features) would cut the dependency surface for server-only apps.

---

## rightkit-media

### Profile
| Field | Value |
|---|---|
| Purpose | Verified FFmpeg/FFprobe artifact catalog, resolution, process lifecycle, probes, loudness normalisation; also a suite-wide native-library catalog (ONNX Runtime, PDFium, etc.). |
| Version / publish | 0.2.4; publishable; INDEX ok. |
| src LOC (5,089) | lib.rs 1,643 (catalog/verify/resolve/lifecycle ~958 + tests), loudnorm.rs 652, native.rs 557, unverified.rs 520, runner.rs 385, staging.rs 353, quickprobe.rs 338, probe.rs 299, command.rs 180, dev_resolve.rs 162 |
| Pub API | 106 fn, 39 struct, 20 enum, 2 trait, 16 type/const |
| Features | none |
| In-repo dependents | audio, playback (normal); asr, ort, vad (**dev-dependencies only**, all for `media::native`). Also data files `catalog/*.json`, `recipes/`, `third-party/`. |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-fs | 0.3, digest only | no | 1 / 1 (`lib.rs:645` `sha256_file` wrapper) | needed | Could also replace `staging.rs`'s manual temp+rename with `atomic_write` (needs `fs` feature). |
| rightkit-process | 0.3.0 path | no | 6 / 2 | needed | `OwnedCommand`. |
| serde | 1.0.228 derive | no | 34 / 3 | needed | Catalog parsing. |
| serde_json | 1.0.150 | no | 8 / 3 | needed | |
| tempfile (dev) | 3 | - | 3 test files; src uses are in `cfg(test)` (`staging.rs:176/280`, `unverified.rs:376/393`) | needed | |

Counts: needed 5.

### Overlap / split notes
- **models vs media (artifact download/verify):** media never downloads; it verifies local artifacts against a pinned catalog (`lib.rs` ArtifactCatalog, `staging.rs`) while models downloads and verifies model files and has its own trusted catalog (`suite/catalog.rs` 436 LOC, `suite/manifest.rs`). Two pinned-digest catalog formats and two verify/stage paths for "approved artifact + sha256 + platform + licence".
- **Split candidate (clear):** `native.rs` (557 LOC + `catalog/native-libraries.json`, `native-tools.json`) is a suite-wide catalog of non-FFmpeg libraries and is consumed by asr/ort/vad only as a dev-dependency, i.e. test-time verification. It has nothing to do with FFmpeg process control. Move to its own crate (e.g. `rightkit-native-catalog`) so ort/asr/vad stop depending on the media crate (and its `rightkit-process`/serde catalog code) for tests. Further, `loudnorm.rs` (652) and `quickprobe.rs` (338, pure-Rust container probe, no process) are separable from the catalog/verify core in `lib.rs`.
- `staging.rs` duplicates atomic write (X4).

---

## rightkit-models

### Profile
| Field | Value |
|---|---|
| Purpose | Local-model manifest and installer (revisions, quantizations, resumable verified downloads, archive extraction, atomic activation, rollback, hardware fit) plus the flat on-disk "suite" contract (catalog, grants, refs, runtime leases, migration, GC). |
| Version / publish | 0.2.7; publishable; INDEX: staged 0.2.7, published 0.2.6. |
| src LOC (9,413) | install.rs 1,292, store.rs 1,003, rightapps.rs 599 (feature), hardware.rs 596, download.rs 543, archive.rs 284, lock.rs 208, archive_core.rs 186, ids.rs 164, spec.rs 149, alias.rs 112, platform.rs 62, disk.rs 58, error.rs 51, lib.rs 46, hash.rs 12; `suite/` 4,048 total: store.rs 1,365, catalog.rs 436, manifest.rs 386, refs.rs 363, lease.rs 360, paths.rs 211, install.rs 175, handle.rs 166, archive.rs 133, discover.rs 133, grant.rs 105, migration.rs 100, mod.rs 61, variant.rs 54 |
| Pub API | 202 fn, 54 struct, 16 enum, 0 trait, 10 type/const |
| Features | default `[]`; `rightapps` (ed25519-dalek, base64, url, chrono; module `rightapps.rs`) |
| In-repo dependents | none. |

### Dependencies
| Dep | Spec | Optional | Use | Verdict | Reason |
|---|---|---|---|---|---|
| rightkit-fs | 0.3, digest only | no | 3 / 2 | needed | Digest only. Should also supply atomic write and locks (X3, X4) by enabling `fs`/`lock`. |
| ed25519-dalek | 2 | yes (rightapps) | 1 / 1 | needed | Gated already; duplicates license verification pattern. |
| base64 | 0.22 | yes (rightapps) | 1 / 1 | needed | Gated. |
| url | 2 | yes (rightapps) | 1 production (`rightapps.rs:13`) | needed | Gated; already in tree via mcp/reqwest. |
| chrono | 0.4, no defaults, `std` | yes (rightapps) | 2 / 1 (`rightapps.rs:495,498`) | needed (consolidate) | Gated; X5. |
| serde | 1 derive | no | 70 / 16 | needed | |
| serde_json | 1 | no | 36 / 11 | needed | |
| ureq | 3.4, `rustls` | no | 26 / 6 | replace-with `rightkit-http::Client` | All download, grant, catalog and rightapps traffic. `pub fn http_agent() -> ureq::Agent` (`download.rs:532`, `suite/store.rs:1363`) leaks ureq types in the public API, so the swap is a breaking change; the two `http_agent` fns are also duplicates. Needs Range/streaming-to-file and 401/403/404/412/416 status handling that http's `Client::stream` + `HttpError::Status` can express. |
| tar | 0.4 | no | 9 prod (`archive_core.rs:27,47`; rest in `cfg(test)` from `archive.rs:102`) | needed | |
| flate2 | 1 | no | 1 (`archive_core.rs:43`) | needed | `zip`'s deflate pulls flate2 anyway, so this adds nothing. |
| dirs | 7 | no | 10 / 5 (`suite/paths.rs`, `handle.rs`, `install.rs`, `discover.rs`) | needed | Small. Lock also has dirs 6 elsewhere. |
| getrandom | 0.3 | no | 1 (`suite/install.rs:121`) | needed | Device id. |
| zip | 2, no defaults, `deflate` | no | 1 prod (`archive.rs:51`) | needed | Zip extraction. |
| sha2 (dev) | 0.11 | - | 6 test files | needed | Independent reference. |
| tempfile (dev) | 3 | - | 9 test files + `archive.rs:121` | needed | |
| rightkit-qa (dev) | 0.2.8 path | - | 4 tests (`qa::mock::MockServer`) | heavy | Pulls qa (and its fs/framed-sidecar chain) into the dev graph to get a mock server; `rightkit-http` has its own (`testing.rs`). Dev-only, no publish impact, but orders publication (comment in Cargo.toml). |

Counts: needed 14 (incl. 4 optional + 2 dev), replace-with 1 (ureq), heavy 1 (rightkit-qa dev).

### Overlap / split notes
- **models vs media:** see rightkit-media.
- **Reimplemented fs primitives:** `store.rs:981` and `suite/manifest.rs:225` (two private `write_atomic`), three lock designs (`lock.rs` TTL + guard file, `suite/lease.rs:239`, `suite/refs.rs:72`) (X3, X4). Highest-impact internal dedupe in the group.
- **Split candidates (≥2 independent responsibilities):**
  1. `suite/` (4,048 LOC, 43% of the crate): flat on-disk contract shared by ScrapeRight/HeardRight/CutRight (catalog, grants, refs, leases, GC). Independent of the per-model installer; could be `rightkit-model-suite`.
  2. `hardware.rs` (596 LOC) plus `disk.rs`: host probe duplicates `logs::system` (X7); candidate for shared sysinfo.
  3. `rightapps.rs` (599 LOC, feature-gated) is a runtime-artifact client with signature verification: overlaps license (Ed25519 verify) and the media native catalog (pinned artifact digests).
- No in-repo dependents: consumed externally only.

---

# Verdict tally

| Crate | needed | unused | feature-gate-it | replace-with | heavy | Total entries |
|---|---|---|---|---|---|---|
| rightkit-db | 6 | 0 | 0 | 0 | 0 | 6 |
| rightkit-embed | 5 | 0 | 1 (thiserror) | 1 (sha2) | 1 (tokenizers) | 8 |
| rightkit-framed-sidecar | 2 | 0 | 0 | 0 | 0 | 2 |
| rightkit-fs | 6 | 0 | 0 | 0 | 0 | 6 |
| rightkit-fsindex | 3 | 0 | 1 (libc) | 0 | 0 | 4 |
| rightkit-hotkeys | 1 | 0 | 0 | 0 | 0 | 1 |
| rightkit-http | 8 | 0 | 1 (rightkit-fs) | 1 (getrandom) | 1 (reqwest, optional) | 11 |
| rightkit-jobs | 8 | 0 | 1 (rightkit-service) | 3 (time, uuid, libc) | 0 | 12 |
| rightkit-license | 4 | 0 | 0 | 0 | 0 | 4 |
| rightkit-llm | 4 | 0 | 0 | 0 | 0 | 4 |
| rightkit-logs | 5 | 0 | 0 | 3 (chrono, ureq, getrandom) | 1 (objc2-foundation) | 9 |
| rightkit-mcp | 5 | 0 | 2 (http, futures-util) | 2 (sha2, ureq) | 2 (rightkit-http[async], tokio) | 11 |
| rightkit-media | 5 | 0 | 0 | 0 | 0 | 5 |
| rightkit-models | 14 | 0 | 0 | 1 (ureq) | 1 (rightkit-qa dev) | 16 |
| **Total** | **76** | **0** | **6** | **11** | **6** | **99** |

No dependency is outright unused (consistent with cargo-machete). The findings are consolidation and gating, not dead deps.
