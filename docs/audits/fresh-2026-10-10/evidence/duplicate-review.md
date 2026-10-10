# Duplicate-functionality review

Source-body review only; no builds, tests, network, prior audit material, or product code changed.

## Confirmed consolidation targets

### 1. Durable atomic replacement has seven local implementations beside its canonical owner

`rightkit-fs` already exposes sibling-temp creation, exclusive creation, optional private permissions, durable file + parent sync, cleanup, & a streaming API in [`atomic.rs`](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/atomic.rs:50). Matching implementation sequence exists in:

| Owner | Exact matched sequence | Material difference |
|---|---|---|
| `rightkit-logs::atomic::write_atomic` | create parent, unique sibling `create_new` temp, write, `sync_all`, rename, parent sync ([source](/Volumes/D/claude/rightkit/crates/rightkit-logs/src/atomic.rs:12)) | No private mode or hardened open flags. |
| `rightkit-daemon::util::atomic_write` | temp, write, `sync_all`, rename, parent sync ([source](/Volumes/D/claude/rightkit/crates/rightkit-daemon/src/util.rs:67)) | Fixed `.tmp` name & non-exclusive `File::create`. |
| `rightkit-models::store::write_atomic` | parent creation, temp, write, `sync_all`, rename, cleanup ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/store.rs:980)) | Deletes existing Windows target before rename, defeating old-target preservation. |
| `rightkit-models::suite::manifest::write_atomic` | parent, unique `create_new` temp, write, `sync_all`, rename, parent sync ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/manifest.rs:224)) | String error adapter only. |
| `rightkit-models::suite::refs::write_ref_unlocked` | parent, unique `create_new` temp, write, `sync_all`, rename, cleanup ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/refs.rs:112)) | Omits parent sync. |
| `rightkit-service::record::write_atomic` | local temp, write, `sync_all`, rename ([source](/Volumes/D/claude/rightkit/crates/rightkit-service/src/record.rs:121)) | Needs bounded Windows retry because readers hold discovery records open ([source](/Volumes/D/claude/rightkit/crates/rightkit-service/src/record.rs:144)). |
| `rightkit-settings::atomic_write_with` | parent, unique `create_new` temp, write, flush, `sync_all`, replacement, parent sync, cleanup ([source](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/atomic.rs:17)) | Uses `ReplaceFileW` for existing Windows targets & makes non-Windows parent-sync failure visible ([source](/Volumes/D/claude/rightkit/crates/rightkit-settings/src/atomic.rs:59)). |

**Disposition:** consolidation candidate, not proven drop-in compatibility. Extend `rightkit-fs::AtomicWriter` with replacement strategy (plain rename, bounded retry, `ReplaceFileW`) plus strict parent-sync result; then migrate wrappers after focused parity tests. `rightkit-models` already depends on `rightkit-fs` with only `digest`; enable `fs` there. Do not replace cache writes: cache deliberately uses disposable `<root>/.tmp`, no sync, startup deletion, LRU accounting, & is not durable state ([source](/Volumes/D/claude/rightkit/crates/rightkit-cache/src/disk.rs:110), [source](/Volumes/D/claude/rightkit/crates/rightkit-cache/src/disk.rs:241)).

### 2. PID liveness probe is duplicated, with safer service policy

`rightkit-fs::process_alive` & `rightkit-service::transport::pid_alive` both perform Unix `kill(pid, 0)` plus Windows `OpenProcess`/exit-code probing. FS accepts PID `0` & unbounded `u32` Unix casts, while service rejects both; FS treats Windows access denial as dead while service treats it as existing ([fs](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/lib.rs:62), [service](/Volumes/D/claude/rightkit/crates/rightkit-service/src/transport.rs:576)). FS uses result to reclaim stale atomic/state temps, so this difference affects cleanup policy.

**Disposition:** consolidation candidate, not proven drop-in compatibility. Harden `rightkit-fs::process_alive` with service guards & access-denied rule, then have service call it behind its public facade. No dependency cycle exists.

### 3. QTS file digest & model-ownership policy bypass shared model owner

`rightkit-tts::models::sha256_file` and `rightkit-fs::digest::sha256_file` both stream a file through SHA-256 & produce lowercase hex ([tts](/Volumes/D/claude/rightkit/crates/rightkit-tts/src/models.rs:103), [fs](/Volumes/D/claude/rightkit/crates/rightkit-fs/src/digest.rs:33)). TTS also owns QTS identity, artifact size/hash manifest, environment-root resolution, presence checks, & `.verified.json` cache ([source](/Volumes/D/claude/rightkit/crates/rightkit-tts/src/models.rs:12)), while `rightkit-models` is declared shared local-model manifest/installer owner ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/lib.rs:1)).

**Disposition:** `sha256_file` has matching signature, I/O-error type & lowercase-hex output; replace behind existing facade after digest/error fixtures. Buffer size changes from 1 MiB to 64 KiB; no throughput equivalence was measured. QTS catalog migration is a consolidation candidate: add QTS catalog/spec + resolve/verify entrypoint to `rightkit-models`, then retain `rightkit-tts::models` as worker-compatible path/runtime facade. Do not create another model package.

### 4. E2E browser driver copied byte-for-byte across UI packages

`packages/app-shell/e2e/browser.ts`, `packages/onboarding/e2e/browser.ts`, & `packages/theme/e2e/browser.ts` are byte-identical: Chrome discovery/launch, static server, CDP transport, input, waits, screenshots, shutdown ([representative source](/Volumes/D/claude/rightkit/packages/app-shell/e2e/browser.ts:1)). `settings-ui/e2e/browser.ts` began from same driver but adds navigation serials, CDP timeouts, focus emulation, visibility flags, & per-key typing ([source](/Volumes/D/claude/rightkit/packages/settings-ui/e2e/browser.ts:51)); this is a maintained fork, not independent design. All already use `@rightkit/qa/chrome` for browser lifecycle.

**Disposition:** app-shell/onboarding/theme driver is proven identical; move it into `@rightkit/qa` test-only export. Settings-ui migration is a candidate after launch/input/navigation hooks cover its additions; keep suite tests local.

### 5. Ownership-source duplication is intentional generated synchronization

`packages/release/ownership-source.mjs` exactly equals `packages/build-control-policy/src/ownership-source.mjs`. `rightkit-sync.mjs` explicitly copies release version to build-control-policy source ([source](/Volumes/D/claude/rightkit/packages/release/rightkit-sync.mjs:195)); build-control-policy installs that source as runtime policy asset ([source](/Volumes/D/claude/rightkit/packages/build-control-policy/src/install-policy-assets.mjs:1)). Thus it is generated synchronization, not competing ownership logic.

**Disposition:** keep generated copy & exact sync/parity protection. No extraction or enforcement/trust-sync change.

## Same-domain owners that should remain separate

| Area | Evidence & decision |
|---|---|
| `rightkit-models::store` vs `suite` | Revisioned `<root>/<id>/state.json + revisions` & flat trusted-catalog compatibility layout are explicitly independent; one model ID belongs to one layout ([source](/Volumes/D/claude/rightkit/crates/rightkit-models/src/suite/mod.rs:17)). They already share lower-level archive code. Retain layouts; consolidate only atomic writes above. |
| Control / automation | Control owns in-app WebDriver/webview admission & durable control sessions; automation owns external desktop accessibility policy. Similar `admission`, `lease`, `keys`, `json` names serve distinct authority boundaries. No merge. |
| Process / sandbox / daemon / service / speech client | Sandbox reuses `rightkit-process::WindowsJob` directly ([source](/Volumes/D/claude/rightkit/crates/rightkit-sandbox/src/windows_job.rs:1)). Daemon’s TLS/upload state differs from user-local service discovery; speech client delegates service APIs. Retain. |
| FS / watch / fsindex / cache / db / search | Watch converts/coalesces backend events; fsindex maintains index format; cache is rebuildable LRU; DB owns SQLite migration/backup; search owns query semantics. No matched policy loop beyond cache’s intentionally non-durable replacement. |
| Audio / QTS / TTS / ASR | Audio path decoder accepts generic WAV; QTS byte decoder restricts QTS reference input & reports QTS errors ([audio](/Volumes/D/claude/rightkit/crates/rightkit-audio/src/wav.rs:19), [qts](/Volumes/D/claude/rightkit/crates/rightkit-qts/src/pipeline/speaker_encoder.rs:250)). Direct dependency would couple QTS runtime to media package; retain. |
| Shell / app-shell / platform-ui | Shell explicitly declares structural bridge twin to remain dependency-free ([source](/Volumes/D/claude/rightkit/packages/shell/src/index.ts:216)); app-shell consumes platform-ui’s one canonical `WindowControlAdapter` ([bridge](/Volumes/D/claude/rightkit/packages/app-shell/src/bridge.ts:14), [owner](/Volumes/D/claude/rightkit/packages/platform-ui/src/index.ts:186)). Type shape duplication has no runtime policy; retain. |
| Logs / license / legal / updates / tauri | Logs differs by runtime transport; legal is manifest contract while legal-ui renders it; updates owns client policy while tauri adapts runtime. Native Rust/Swift license enforcement intentionally fails closed while JS is documented UI hint ([JS](/Volumes/D/claude/rightkit/packages/license/src/index.ts:273), [Rust](/Volumes/D/claude/rightkit/crates/rightkit-license/src/lib.rs:69), [Swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseClient.swift:763)). Retain language-native verification; keep shared vectors aligned. |
