# rightkit-crash

Privacy-first crash capture for Right Suite apps: panic capture, redacted crash reports, optional native dumps kept local, and user-initiated support bundles. The app owns the crash directory and the consent UI. Nothing is uploaded implicitly.

Catalog capability: `crash-reporting`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## What it owns

- `install_panic_hook(CrashConfig)`: chains the existing panic hook and writes a redacted, bounded JSON report atomically. The hook still runs under `panic = "abort"`.
- `record_report`: records panic, hang and sidecar crash reports, with bounded retention (`Retention`, `prune`).
- `NativeCapture`: `Off` (default) or `Auto`. `Auto` uses macOS `sample` or the adjacent Windows dump helper (`rightkit-dump-helper` binary, or an app's own binary calling `rightkit_crash::dump_helper::run`). Native artifacts stay local.
- `enqueue_pending(crash_dir, outbox)`: waits for consent, queues each report once through `rightkit-logs`' outbox, and moves submitted reports aside. `list_pending` lists what is waiting.
- `build_support_bundle(SupportBundleOptions)`: a redacted ZIP. Native artifacts need explicit opt-in.

## When to use it

- Use it for panic and crash reports that must be redacted before they reach disk, and sent only after consent.

Do not use it to upload reports on its own. Upload goes through the consented outbox in `rightkit-logs`.

## Cargo features

`support-bundle` (default): `build_support_bundle`, `SupportBundleOptions`, `BundleManifest`, `create_zip` and the `zip` (deflate) dependency. `window-errors` (off): renderer/webview error capture, `record_window_error`, `WindowErrorGate`, `WindowErrorReport`; adds no dependency. Other dependencies: `rightkit-fs` (`fs` feature only), `rightkit-logs` (without its default `http` feature), `serde`, `serde_json`. Windows-only dependencies apply on Windows.

## Usage

In release builds the panic hook prints one fixed line instead of the previous hook's message, so the panic payload never reaches stderr (rightkit#513). Builds with debug assertions chain to the previous hook by default. `CrashConfig::chain_default_hook(bool)` overrides either default.


Taken from the setup in `tests/crash_e2e.rs`:

```rust
use rightkit_crash::{install_panic_hook, CrashConfig};

fn main() {
    install_panic_hook(CrashConfig::new("MyApp", "1.2.3", crash_dir));
    // ... app code ...
}
```

`CrashConfig::new(app_name, app_version, crash_dir)` defaults to `NativeCapture::Off`. Set `native_capture` to `NativeCapture::Auto` to also keep a native sample or dump locally. The crash directory is app-owned; RightKit never chooses a location.

## Process kinds and renderer errors

Each process boundary of an app declares its kind. Reports from it carry the fixed code in `process_kind`: `window`, `engine` or `helper` (`ProcessKind`, `src/hook.rs:33-50`). The field is omitted when no kind was declared (`src/report.rs:37-40`).

- `CrashConfig::process_kind(kind)` declares the kind on a configuration (`src/hook.rs:124`).
- `install_process_hook(kind, config)` installs the panic hook for one boundary; it is the same as setting `process_kind` and calling `install_panic_hook` (`src/hook.rs:300`). Each process calls it once, early in `main`.
- `record_helper_exit(&config, helper, exit, restart_count)`, called by the supervising process, records a supervised helper's abnormal exit as a `sidecar_crash` report with `process_kind` `helper` (`src/hook.rs:309-319`).

With the opt-in feature `window-errors`, `record_window_error(&config, &gate, &report)` records a renderer or webview error as a `window_error` report, or a `window_rejection` for an unhandled rejection, with `process_kind` `window` (`src/window.rs:121-151`). It never takes a native sample or dump (`src/window.rs:140-142`). The app registers its own command that calls it; nothing is recorded until it does (`src/window.rs:3-8`).

- `WindowErrorReport` carries `kind`, `message`, `stack` and `source` (`src/window.rs:24-34`).
- `WindowErrorGate::default()` allows 5 reports per 60 seconds and 25 per process; `WindowErrorGate::new(window, max_per_window, max_total)` sets its own limits (`src/window.rs:54-61`).
- The message is capped at `MAX_WINDOW_MESSAGE_CHARS` (1,000 characters) and the stack at `MAX_WINDOW_STACK_BYTES` (8 KiB) (`src/window.rs:17-19`, `src/window.rs:134-135`). The source keeps no query or fragment (`src/window.rs:136-139`).

## Hang watchdog

A process can hang without crashing: every window reports Responding, logs keep being written, and no panic report exists. `HangWatchdog` (`src/hang.rs`) catches an event loop or UI thread that stops pumping (#882). One thread, `rightkit-hang-watchdog`, compares each watched loop's last beat with a monotonic clock. A loop silent for the threshold (default 10 s, minimum 2 s) is one episode, handled once:

- a `hang` report file through `record_report` (redacted, owner-only, to the config's crash folder; the config's sink is not called for hangs), then the optional `HangOptions::on_hang(Fn(&HangEvent))` callback for the app (loop name, stall duration, report path, recovered flag; no secrets), plus one `rightkit-crash: hang loop=<name> stalled_secs=<n>` line on stderr;
- an all-threads native capture: a Windows minidump through the dump helper, or a macOS `sample` (scrubbed). Off with `HangOptions::native_capture(false)`;
- the last `log_tail_bytes` (default 64 KiB) of each `log_file` in `sample_hang_logs_*.txt`, owner-only and written atomically. Known secrets (`redact_values`) and the pattern scrub run before any byte reaches disk, and a first partial line is dropped. If scrubbing fails the file holds only a withheld marker.

Captures (native and log tail) repeat for the same loop at most once per cooldown (default 10 minutes). When beats resume the watchdog logs `hang-recovered`, calls `on_hang` with `recovered` set, and re-arms. Nothing is recorded while `set_recording_enabled(false)`. A beat is one atomic store, safe on a UI thread; the watchdog takes no lock a beating thread holds. The last clone of a `Heartbeat` dropping unregisters the loop: the drop never waits for a report, it briefly takes the registry lock, and a report already in progress when it drops may still complete (the loop is checked right before each write step). `on_hang` runs synchronously on the watchdog thread and must return promptly. Dropping `HangWatchdog` or calling `stop()` sets stop and waits for the watchdog thread, including any `on_hang` callback and native capture in progress; once it returns, no callback runs and nothing is written. Report, sample and dump file names carry a process-wide sequence number, so reports within one second never overwrite each other.

Tauri: the app cannot beat from a blocked main thread, so post a beat to it. If the closure never runs, the main loop is hung. At most one closure is outstanding.

```rust
use rightkit_crash::{CrashConfig, HangOptions, HangWatchdog};

let watchdog = HangWatchdog::start(
    CrashConfig::new("MyApp", "1.2.3", crash_dir),
    HangOptions::new().log_file(log_dir.join("app.log")),
)?;
let handle = app.handle().clone();
watchdog.pump_via("main", move |beat| handle.run_on_main_thread(beat).is_ok())?;
// Keep `watchdog` alive for the life of the app (for example in `app.manage`).
```

Tokio (or any loop you own): beat as the loop turns, and drop the heartbeat when it ends.

```rust
let heartbeat = watchdog.heartbeat("engine");
tokio::spawn(async move {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    loop {
        tick.tick().await;
        heartbeat.beat();
        // ... the loop's work ...
    }
});
```

A task that awaits one long call without ticking looks hung; beat from a separate ticking task only if that is the question you want answered.

## Platform support

- macOS: `sample`-based native capture.
- Windows: an out-of-process minidump through the dump helper. The Windows-only test `capture_native_writes_a_minidump_through_an_app_named_helper` covers it (`tests/crash_e2e.rs:403`); it passed on Windows 11 (build 26300, the Dell) on 2026-10-20 with `--all-features`.
- Android: no in-process handler. `record_android_exits` takes the OS exit history the Kotlin shell reads at launch (`getHistoricalProcessExitReasons`: crash, native crash tombstone, ANR), keeps header and backtrace text only, and records each death once through the normal redacted report path. Not yet run on a device (docs/design/android-minimum.md).
- Other platforms: structured reports only.

`capture_native(dir, kind)` takes the same native capture on demand, for hang, UI-stall or
sidecar-crash paths that are not panics, and returns the artifact path.

`capture_native_for_pid(&config, kind, pid)` does the same for a child process the parent names (call it while the helper is still alive, before killing it). Known app secrets set with `CrashConfig::redact_values` are removed from reports and from the macOS sample text. When `crash_dir` is unusable, reports go to the private folder at `fallback_crash_dir(app_name)` in the OS temp directory; list it with `list_pending`.

### What a native artifact contains, and what is scrubbed

- **macOS sample** (`sample_<kind>_<stamp>_<pid>_<seq>.txt`): the call stacks and loaded binary image paths of the process, as text. Image paths include the user's home folder name, so the text is scrubbed (home folder names, emails, credentials, and any `redact_values` secret) before it is placed in the crash folder, which is where a support bundle is built from. `sample` writes to a private `0700` directory in the OS temp directory, never inside the crash folder; the scrubbed bytes are published atomically, owner-only, under the `sample_*.txt` name, and the raw directory is deleted in every case. No unscrubbed byte is ever written under the crash folder. The support bundle scrubs `sample_*` text again when it copies it.
- **If scrubbing fails** (raw file unreadable, scrubber panic, or a known secret still present), the sample is dropped and `sample_*.txt` holds only `sample withheld: scrub failed`; `native_capture_note` is `sample_scrub_failed` or `sample_read_failed`. If even the marker cannot be written, nothing is published: the capture returns `None`, `native_artifacts` stays empty and the note is `sample_publish_failed` (plus `sample_raw_not_removed <directory name>` if the raw directory could not be deleted). The same code goes to stderr as a `rightkit-crash:` line. `prune` and the next macOS capture remove raw directories older than five minutes.
- **Windows minidump** (`dump_<kind>_<stamp>_<pid>_<seq>.dmp`): process memory, **not scrubbed** (there is no sound way to scrub a binary dump). It stays local, is never queued, and enters a support bundle only when the app sets `include_native_dumps` after an explicit user opt-in.

### Bundling the Windows dump helper

The helper is this crate's `rightkit-dump-helper` binary. An app builds it and ships it next to
its executable:

```text
rightkit cargo build --release -p rightkit-crash --bin rightkit-dump-helper
```

With rightkit-crash from crates.io, `rightkit cargo install rightkit-crash --bin rightkit-dump-helper --root <dir>` builds the same binary.

The helper can ship under the app's own name, for example as a Tauri `externalBin` or a
renamed copy. Tell the crate which name to use before the first capture:

```rust
rightkit_crash::set_dump_helper("heardright-dump-helper.exe"); // next to the app executable
```

A bare name resolves next to the current executable, and an absolute path is used as given.
The default is `DEFAULT_DUMP_HELPER` (`rightkit-dump-helper.exe`). When the helper is missing,
the capture returns `None` and the structured report is still written.

## Tests

`tests/crash_e2e.rs` runs a real panic in a child process. Reports travel through the consented outbox to a loopback mock server.

## Verification

Run through the managed Rust wrapper (see `docs/rules/rightkit.md`):

```text
rightkit cargo test -p rightkit-crash --target <native-target>
```

## Version and changes

Published version: see [INDEX.md](../../INDEX.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).
