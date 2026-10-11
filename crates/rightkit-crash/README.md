# rightkit-crash

Privacy-first crash capture for Right Suite apps: panic capture, redacted crash reports, optional native dumps kept local, and user-initiated support bundles. The app owns the crash directory and the consent UI. Nothing is uploaded implicitly.

Catalog capability: `crash-reporting`. Published to crates.io (see [INDEX.md](../../INDEX.md)).

## What it owns

- `install_panic_hook(CrashConfig)`: chains the existing panic hook and writes a redacted, bounded JSON report atomically. The hook still runs under `panic = "abort"`.
- `record_report`: records panic, hang and sidecar crash reports, with bounded retention (`Retention`, `prune`).
- `NativeCapture`: `Off` (default) or `Auto`. `Auto` uses macOS `sample` or the adjacent Windows dump helper (`rightkit-dump-helper` binary). Native artifacts stay local.
- `enqueue_pending(crash_dir, outbox)`: waits for consent, queues each report once through `rightkit-logs`' outbox, and moves submitted reports aside. `list_pending` lists what is waiting.
- `build_support_bundle(SupportBundleOptions)`: a redacted ZIP. Native artifacts need explicit opt-in.

## When to use it

- Use it for panic and crash reports that must be redacted before they reach disk, and sent only after consent.

Do not use it to upload reports on its own. Upload goes through the consented outbox in `rightkit-logs`.

## Cargo features

`support-bundle` (default): `build_support_bundle`, `SupportBundleOptions`, `BundleManifest`, `create_zip` and the `zip` (deflate) dependency. Other dependencies: `rightkit-fs` (`fs` feature only), `rightkit-logs` (without its default `http` feature), `serde`, `serde_json`. Windows-only dependencies apply on Windows.

## Usage

Taken from the setup in `tests/crash_e2e.rs`:

```rust
use rightkit_crash::{install_panic_hook, CrashConfig};

fn main() {
    install_panic_hook(CrashConfig::new("MyApp", "1.2.3", crash_dir));
    // ... app code ...
}
```

`CrashConfig::new(app_name, app_version, crash_dir)` defaults to `NativeCapture::Off`. Set `native_capture` to `NativeCapture::Auto` to also keep a native sample or dump locally. The crash directory is app-owned; RightKit never chooses a location.

## Platform support

- macOS: `sample`-based native capture.
- Windows: an out-of-process minidump through the dump helper. The Windows-only test `capture_native_writes_a_minidump_through_an_app_named_helper` covers it (`tests/crash_e2e.rs:296`); the crate records no Windows 11 run.
- Other platforms: structured reports only.

`capture_native(dir, kind)` takes the same native capture on demand, for hang, UI-stall or
sidecar-crash paths that are not panics, and returns the artifact path.

### What a native artifact contains, and what is scrubbed

- **macOS sample** (`sample_<kind>_<stamp>_<pid>.txt`): the call stacks and loaded binary image paths of the process, as text. Image paths include the user's home folder name, so the text is scrubbed (home folder names, emails, credentials) before it can be bundled. `sample` writes to a hidden, owner-only raw file (`.sample_raw_<kind>_<stamp>_<pid>.raw`) that the support bundle, `prune` retention and `list_pending` never list. The scrubbed bytes are then published atomically, owner-only, under the `sample_*.txt` name, and only then does the capture return the path and the report name the file in `native_artifacts`. The raw file is deleted in every case.
- **If scrubbing or publishing fails** (the raw file cannot be read, or the scrubbed file cannot be written), nothing is published: the capture returns `None`, the raw file is deleted, the report's `native_artifacts` stays empty, `native_capture_note` carries a fixed code (`sample_read_failed`, `sample_publish_failed`, `sample_raw_create_failed`, and `sample_raw_not_removed <file name>` if the raw file could not be deleted), and the same code is written to stderr as a `rightkit-crash:` line. A leftover raw file cannot be exported, and `prune` (and the next macOS capture) removes raw files older than five minutes.
- **Windows minidump** (`dump_<kind>_<stamp>_<pid>.dmp`): process memory, **not scrubbed** (there is no sound way to scrub a binary dump). It stays local, is never queued, and enters a support bundle only when the app sets `include_native_dumps` after an explicit user opt-in.

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
