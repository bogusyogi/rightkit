Sage design, 2026-10-11, for gap ledger row 109 (Pulse acceptance list). Source reading only; nothing compiled.

# rightkit-watch native backends: design for gap ledger row 109

**Decision: option (b).** Add native Windows and macOS backends inside `rightkit-watch`, reached through one new constructor, `TypedWatcher::watch_with`. Every existing constructor stays on notify unchanged. Ship as 0.4.0, purely additive. Nothing was compiled or run; this is from source reading only.

## Why (b)

Notify paths below are relative to `/Users/adrdsouza/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/notify-8.2.0/src/`.

**(a) Patching around notify is not possible without forking it.**

| Acceptance point | notify 8.2.0 source |
|---|---|
| 1. Overflow becomes rescan | `windows.rs:333` names the parameter `_bytes_written` and never reads it. Each request gets a zeroed buffer (`:279`), so a zero-byte completion parses one empty record (`:385-463`) and emits nothing. An unrecognised error code hits `_ =>` (`:357-367`): logs, unwatches, returns, with no event to the handler. `Flag::Rescan` is set only in `fsevent.rs:117` and `inotify.rs:213`. |
| 2. Configurable buffer | `windows.rs:40` `const BUF_SIZE: u32 = 16384`, used at `:52`, `:279`, `:311`. |
| 3. Blocking wait | `windows.rs:154` `WaitForSingleObjectEx(self.wakeup_sem, 100, 1)` inside `loop` (`:124`). |
| 4. Replay, latency, granularity | `fsevent.rs:299-301` hard-codes `kFSEventStreamEventIdSinceNow`, latency `0.0`, and `FileEvents \| NoDefer`. The fields are private (`:66-68`) and there is no setter. Event ids are discarded (`:532` `_event_ids`); `HISTORY_DONE` is swallowed (`:108-110`). |
| Any configuration | `Config` holds only `poll_interval`, `compare_contents`, `follow_symlinks` (`config.rs:39-47`). Both backends ignore it (`windows.rs:563`, `fsevent.rs:581`); `configure` returns `Ok(false)` (`windows.rs:249-252`, `fsevent.rs:500-503`). |

Two further hazards in notify's FSEvents callback: it panics inside an `extern "C"` function on any unknown flag bit (`fsevent.rs:545-547`) and on non-UTF-8 paths (`:539-541`).

**(c) Lifting Pulse's watchers fails the acceptance list as written.**
- **Not fully blocking:** the Windows reader wakes every 500 ms (`watch_windows.rs:87`, `:345`), and both appliers sleep-poll every second (`watch_windows.rs:73`, `:186`; `watch.rs:63`, `:218`).
- **Event ids discarded:** the macOS callback ignores them (`watch.rs:169`).
- **Not reusable:** both are bound to Tauri and Pulse's scanner (`watch_windows.rs:66-68`, `watch.rs:58-60`) and to a process-global generation counter (`watch_windows.rs:91`, `watch.rs:67`), so one watch per process.

Pulse is the logic donor for specific pieces only (listed in the Windows lane).

**(b) fits the existing seam.** The native backends emit `notify::Event` values into the existing `Translator` (`crates/rightkit-watch/src/typed.rs:309`). That already turns `Flag::Rescan` into `WatchEvent::Overflow` (`typed.rs:326-332`) and pairs adjacent untracked rename halves, which is the `ReadDirectoryChangesW` shape (`typed.rs:339-350`). notify must stay a dependency anyway: `pub use notify` (`src/lib.rs:20`) and `WatchEventRecord.raw_kind: Option<notify::EventKind>` (`src/options.rs:67`) are public.

## Public API additions

New file `src/native.rs`, re-exported from `lib.rs`.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WatchBackend { #[default] Auto, Native, Notify }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Granularity { #[default] File, Directory }

/// Opaque resume position. Private fields: volume UUID ([u8; 16]) and event id (u64).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WatchCursor { /* private */ }
impl WatchCursor {
    pub const ENCODED_LEN: usize = 25; // version byte, 16-byte UUID, u64 little-endian
    pub fn to_bytes(&self) -> [u8; Self::ENCODED_LEN];
    pub fn from_bytes(bytes: &[u8]) -> Option<Self>;
    pub fn event_id(&self) -> u64;
}

#[derive(Clone)]
pub struct TypedOptions { /* private */ }
impl Default for TypedOptions { /* same as new() */ }
impl TypedOptions {
    pub fn new() -> Self;
    pub fn backend(self, backend: WatchBackend) -> Self;
    pub fn queue_capacity(self, records: std::num::NonZeroUsize) -> Self;   // default 32_768
    pub fn exclude_prefix(self, path: impl Into<PathBuf>) -> Self;
    pub fn exclude_with(self, excluded: impl Fn(&Path) -> bool + Send + Sync + 'static) -> Self;
    pub fn granularity(self, granularity: Granularity) -> Self;             // default File
    pub fn latency(self, latency: Duration) -> Self;                        // default ZERO
    pub fn replay_from(self, cursor: WatchCursor) -> Self;
    pub fn windows_buffer_bytes(self, bytes: u32) -> Self;                  // default 262_144
    pub fn windows_network_buffer_bytes(self, bytes: u32) -> Self;          // default 65_536
}

impl TypedWatcher {
    pub fn watch_with(path: impl AsRef<Path>, options: TypedOptions) -> Result<Self, WatchError>;
    pub fn cursor(&self) -> Option<WatchCursor>;
    pub fn current_cursor(path: impl AsRef<Path>) -> Option<WatchCursor>;
    pub fn is_replaying(&self) -> bool;
}
```

`WatchMetrics` gains `dropped_total: u64` and `queue_capacity: u64` (0 means unbounded). Mark `WatchMetrics` and `WatchStatus` `#[non_exhaustive]` in the same change. Both arrived in 0.3.3 (`CHANGELOG.md:5`), which `docs/PLATFORM-MATRIX.md:98` shows as unpublished; the implementer confirms against `INDEX.md` before relying on that.

**Option semantics**

- **Backend.** `Auto` is native on Windows and macOS, notify elsewhere. `Native` on another OS returns `Err(WatchError::Backend)`. `Notify` under `watch_with` still gets the bounded queue, exclusions and directory folding, but keeps its translator thread.
- **Queue bound.** When a push finds the queue full, the queue is cleared, `dropped_total` rises, and a gap flag is set. The next receive returns one `WatchEvent::Overflow` with `raw_kind: None`. Backend overflow keeps `raw_kind: Some(EventKind::Other)`, matching `options.rs:1113`, so the two are distinguishable. Later events queue normally. Memory is bounded at capacity plus one record.
- **Exclusions.** Applied on the backend thread after translation and before queueing. `exclude_prefix` is canonicalised like the root and compared with `Path::starts_with`. A rename with one excluded side degrades to `Delete(from)` or `Create(to)`, the same rule as `options.rs:293-299`. `Error` and `Overflow` are never excluded. The predicate must not block.
- **Granularity.** `Directory` on macOS drops the `FileEvents` create flag, so each record is `Modify(dir)` with `raw_kind: Some(EventKind::Any)`. On Windows and notify, a shared function folds each event to `Modify(parent)` with adjacent de-duplication.
- **Latency.** FSEvents batching interval; ignored by other backends. Default zero matches notify (`fsevent.rs:300`).
- **Windows buffers.** Rounded down to a multiple of 4, minimum 4,096. If the first arm fails with `ERROR_INVALID_PARAMETER`, retry once at the network size (Pulse `watch_windows.rs:293-301`).
- **Mapping without new variants.** Dropped events and `MustScanSubDirs` become `Overflow`. A root change or dead handle becomes `WatchEvent::Error(WatchError::Backend { paths: [root] })` and the stream ends.

**Cursor persistence contract**

1. `cursor()` covers every record this handle has returned from any `recv*` call. It is `None` on Windows and notify.
2. Persist `cursor().to_bytes()` only after applying everything received so far. Delivery after resume is at-least-once.
3. On restart, pass the decoded cursor to `replay_from`. History arrives first; `is_replaying()` turns false at FSEvents' history-done marker.
4. The stream starts live and delivers `Overflow` first when the cursor cannot be honoured: volume UUID mismatch, cursor id above the current id, ids wrapped, or a backend without replay. The consumer rescans, then continues.
5. A queue-overflow `Overflow` carries the highest dropped id, so `cursor()` after receiving it and rescanning is safe to persist.
6. Scan-then-watch on macOS: take `current_cursor(root)`, scan, then `watch_with(replay_from(..))`. That is Pulse's order at `scanner.rs:1078` and `:1133`.
7. On Windows: start the watcher first, scan, then drain. Never pass a cursor there. This also closes the gap Pulse documents at `watch_windows.rs:52-54`.
8. When one FSEvents event yields several records, only the last carries the id.

## Semver

0.4.0, additive, no source break. The design avoids three breaks that looked natural:
- **`WatchOptions` gets no new fields.** ViewRight builds it with a full struct literal (`viewright/src-tauri/src/watcher.rs:117-130`).
- **`WatchEvent` and `WatchError` get no new variants.** membrane-blueprint matches `WatchEvent` exhaustively (`coderight/context/engine/crates/membrane-blueprint/src/watch.rs:435-443`).
- **`WatchEventRecord` gets no cursor field.** The cursor is read from the handle instead.

`TypedWatcher` must stay `Send`; add a compile-time assertion.

## What each consumer sees

Consumers per `docs/audits/fresh-2026-10-11/evidence/app-rightkit-dependencies.csv`; a whole-workspace grep timed out, so I re-checked only CodeRight, ViewRight and Pulse directly.

| Consumer | Pin | Uses | Effect |
|---|---|---|---|
| ViewRight | `=0.3.1` | `FileSetWatcher::watch`, `DirWatchSet::with_options` and `.watch` (`watcher.rs:77`, `:114`, `:172`) | None. Same notify code path; compiles unchanged on 0.4.0. |
| CodeRight engine | `=0.1.0` | `subscribe` types (`api_fs_watch.rs:40-42`) | None. `subscribe.rs` is untouched. |
| membrane-blueprint | `=0.3.0` | `FileWatcher::watch_typed`, `try_recv` (`watch.rs:616`, `:744`) | None. `watch` and `watch_typed` stay on notify and unbounded. |
| Pulse | not a dependent | new adopter | Uses `watch_with` only. |
| rightkit-fsindex | not a dependent (`Cargo.toml:21-26`) | own FSEvents stream (`src/backend/macos.rs:495-604`) | None. |

- **Legacy constructors still lose events silently on Windows.** That is the notify defect in row 1 of the first table. I left it so no existing consumer changes behaviour in this release. Flipping them to native is a one-line follow-up once the native backend has budget evidence; say if you want it in 0.4.0 instead.
- **fsindex stays as it is.** Build the internal macOS `Stream` with multi-root, raw flags and ids, and `FSEventStreamFlushSync`, so fsindex can move onto it later without a redesign. Its planned Windows change feed (fsindex `README.md:25-33`, USN journal) could use the native Windows backend unelevated instead.

## Test plan

Real filesystem, on the named host. Rust commands go through `rightkit cargo`; Windows runs go through `win`.

| # | Host | Test | Passes when |
|---|---|---|---|
| 1 | Windows | `windows_buffer_bytes(4096)`, an `exclude_with` predicate that sleeps 200 ms (stalls the reader using only public API), 5,000 files created in a burst | An `Overflow` with `raw_kind == Some(EventKind::Other)` arrives; `overflow_total >= 1`; a file created afterwards still yields `Create`. |
| 2 | Windows | Unit: arm at the default size on a local temp dir; fallback via an injected `ERROR_INVALID_PARAMETER`. `#[ignore]`: real UNC root from `RIGHTKIT_WATCH_TEST_UNC` | Local arm uses 262,144 bytes. Injected failure retries once at 65,536. The UNC run arms and delivers a `Create`; its output is the evidence. |
| 3 | Windows | A hidden counter incremented each time the wait call returns; idle 5 s. Plus the budget bench idle window, native versus notify | Counter delta is 0. Bench shows native idle wake-ups near zero and notify near 10 per second. |
| 4a | macOS | Take `current_cursor`, create A and B with no watcher, start with `replay_from`; then restart from the new `cursor()` and create C. Third run: cursor with a corrupted UUID | Run 1 delivers A and B, `is_replaying()` turns false, cursor advances. Run 2 delivers C and not A or B. Run 3's first record is `Overflow`. Byte round-trip is equal. |
| 4b | macOS | `latency(1 s)`, write one file | No record before 500 ms; a record within 10 s. |
| 4c | macOS | `Granularity::Directory`, 200 files written into one subdirectory. `#[ignore]`: watch `/` for 10 s with exclusions | Every record is `Modify` of an existing directory with `raw_kind == Some(EventKind::Any)`; the subdirectory appears; far fewer than 200 records. |
| 5 | both | Capacity 8, no draining, 500 files. A hidden counter of translator-thread spawns. `exclude_prefix` plus a file-name predicate, including a rename across the boundary | `queue_depth <= 9` at every sample; `dropped_total > 0`; first receive is `Overflow` with `raw_kind == None`; a later create still arrives. Spawn counter is 0 on the native path. Only non-excluded paths appear; the rename arrives as `Create`. |

**Regression guard:** the existing suite (`tests/e2e.rs`, `typed.rs`, `options.rs`, `directory_options.rs`, `metrics.rs`) must pass unmodified on both hosts. Add `tests/consumer_surface.rs` that compiles the three consumers' exact usages: the full `WatchOptions` literal, the exhaustive `WatchEvent` match, and the `subscribe` imports. Apps cannot take a path dependency, so this is the source-compatibility check.

## Implementation plan

Lane 0 lands first; the Windows and macOS lanes then run in parallel on disjoint files; lane 3 is last.

**Lane 0: common seam (the macOS implementer does this first).**
1. Add the bounded queue (mutex, deque, condvar) returning the std receive error types. `TypedWatcher` holds either the existing `mpsc::Receiver` or the bounded queue. Queue entries carry a private event id.
2. Add `src/native.rs` with the types above, shared exclusion and directory-fold functions, and a backend-sink type that runs `Translator` inline.
3. Implement `watch_with` for `WatchBackend::Notify` through the new queue; stub the native arms.
4. Add the metrics fields and `tests/consumer_surface.rs`, and implement test 5 against the notify backend.

**Lane W: Windows (Sonnet, on the Windows host).** Files: `src/native/windows.rs`, `tests/native_windows.rs`. Use `windows-sys` at the version notify already resolves (0.60, notify `Cargo.toml:112-121`), not hand-written FFI.
1. Open the root with `FILE_LIST_DIRECTORY`, full sharing, backup semantics and overlapped. Create two manual-reset events: I/O and stop.
2. Run one reader thread. Arm with subtree watching; apply the buffer fallback on the first arm.
3. Wait on both events with `INFINITE`. Use a finite timeout only while `Translator::next_deadline()` is `Some`, so a pending rename half can expire.
4. On completion, zero bytes or `ERROR_NOTIFY_ENUM_DIR` emits a `Flag::Rescan` event. Any other failure emits `Error` and ends the thread.
5. Copy the bytes and re-arm before parsing (Pulse `watch_windows.rs:375-379`). Parse with bounds checks (Pulse `:416-438`).
6. File granularity uses notify's filter set (`windows.rs:284-290`) and its kind mapping (`:425-454`). Directory granularity uses Pulse's filter (`watch_windows.rs:521`) and drops "modified" reports whose target is a directory, with a bounded stat (Pulse `:476-485`).
7. On drop: signal stop, `CancelIoEx`, wait for completion, and leak the buffer rather than free it if the wait times out (Pulse `:304-322`).
8. Implement tests 1, 2, 3 and 5.

**Lane M: macOS (Sonnet, on the Mac).** Files: `src/native/macos.rs`, `tests/native_macos.rs`.
1. Build the stream on a serial dispatch queue; the in-repo reference is fsindex `src/backend/macos.rs:504-604`. No crate thread: the callback translates, excludes and enqueues.
2. Mask unknown flag bits and convert paths lossily. Never panic in the callback.
3. Guard the callback context against a late callback after stop (fsindex leaks its sink for this reason, `macos.rs:598-600`).
4. Create flags: `NoDefer | WatchRoot`, plus `FileEvents` for file granularity. Reuse notify's flag-to-kind mapping (`fsevent.rs:95-243`) for file events.
5. Read the volume UUID through `FSEventsCopyUUIDForDevice` for `current_cursor` and for validating `replay_from`.
6. Implement tests 4a, 4b, 4c and 5.
7. If the code is derived from fsindex (itself adapted from fsearch, MIT, `macos.rs:1-7`) or from Pulse (adapted from Petal, MIT, `watch.rs:8-9`), add a `THIRD-PARTY-NOTICES` file and list it in `Cargo.toml` `include`.

**Lane 3: integration (either implementer).**
1. Extend `benches/budget.rs` with a native-backend run. Record measurements on both hosts from committed code using the `record` command.
2. Regenerate the README `## Runtime budget` section and rewrite README "Platform support".
3. Update `CHANGELOG.md`, the `docs/PLATFORM-MATRIX.md` row and gap ledger row 109 in the same change.

## Findings outside row 109

- **The runtime-budget artefacts are not publishable as they stand.**
  - README has no `## Runtime budget` section, though `CHANGELOG.md:7` says the figures are there.
  - `runtime-budget.json` has null ceilings and no measurements, and its status note is stale against `typed.rs:175`.
  - The one evidence file was recorded with uncommitted changes (`runtime-budget.evidence/macos-arm64-0.3.3.txt:6`), is named without a scope, and has no Windows counterpart.
  - Ceilings must be declared by the lead; I did not invent any.
- **The crate has its own idle wake-ups on the non-typed paths.** `options.rs:520` and `:584` wake every 50 ms per `FileSetWatcher::watch_with_options` or `DirWatchSet` watch, and ViewRight holds up to 32 of the latter. The bench measures only `TypedWatcher`, so this is invisible in the evidence.
- **`rightkit-watch` depends on `rightkit-process`** (`Cargo.toml:22`), which ledger row 107 says requires `windows` 0.62 against Pulse's 0.61.3 pin. Pulse will meet that when it adopts this crate.

## Not verified

- **Windows API behaviour:** that overflow surfaces as a zero-byte completion or `ERROR_NOTIFY_ENUM_DIR`, and that network shares reject buffers above 64 KiB, rest on Pulse's comments (`watch_windows.rs:38-40`, `:82-83`). Test 1 and the UNC run settle both.
- **`FSEventsCopyUUIDForDevice`** and FSEvents' behaviour when the cursor predates retained history are from memory of Apple's API, not a fetched source.
- **Wake-up counter on Windows:** I did not check whether `rightkit_process` reports one there; the wait-return counter in test 3 does not depend on it.
- **Default queue capacity of 32,768** is my choice, not derived from a stated ceiling. Pulse would set 20,000 to match its `MAX_DIRS`.
