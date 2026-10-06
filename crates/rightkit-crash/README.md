# rightkit-crash

Privacy-first crash capture for Right Suite apps.

- `install_panic_hook` chains the existing hook and writes a redacted, bounded JSON report atomically.
- `record_report` supports panic, hang, and sidecar crash reports with bounded retention.
- `NativeCapture::Auto` uses macOS `sample` or an adjacent Windows dump helper; native artifacts remain local.
- `enqueue_pending` waits for consent, queues each report once through `rightkit-logs`, and moves submitted reports aside.
- `build_support_bundle` creates a redacted ZIP; native artifacts require explicit opt-in.

App owns crash-directory paths and consent UI. No report or native artifact is uploaded implicitly.

## Verification

```text
rightkit cargo test -p rightkit-crash --target <native-target>
```

Windows needs native-host verification for dump-helper compilation, helper discovery, and real minidump output.
