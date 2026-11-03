# rightkit-app-update

The native half of Right Suite app updates that the Tauri shell host does not carry.

- **Boot probation** (`BootProbation`): after an update installs, the app calls `begin` before the relaunch, `evaluate_boot` at every start, `report_stage` as boot stages pass and `acknowledge_healthy` once it is healthy. A version that starts more than `max_boot_attempts` times (default 2) without acknowledging is declared failed (`BootDecision::ReinstallRequired`, with the last stage it reached) and the app tells the person to reinstall the release they came from. One sentinel file, written atomically; nothing is backed up or restored.
- **Fetch bridge** (`FetchBridge`): runs a webview `fetch` request natively through rightkit-http under a native `FetchPolicy` (exact https hosts, optional path prefixes). Closed request and response DTOs with base64 bodies, fixed error codes, no transport text, no cookies. Blocking; call it from a blocking task. Registering it as a Tauri command is the shell host's job and the `fetch`-compatible JavaScript export is `@rightkit/tauri`'s.

Update check, download, minisign verification and install are `rightkit-tauri-shell`'s `update` feature, with `@rightkit/app-update/native` in the renderer.

```rust,ignore
use rightkit_app_update::{BootDecision, BootProbation};

let probation = BootProbation::new(state_dir.join("update-sentinel.json"));
if let BootDecision::ReinstallRequired(failure) = probation.evaluate_boot(running_version)? {
    show_reinstall(&failure.from_version);
}
// later, once the UI is healthy:
probation.acknowledge_healthy()?;
```

Not published (0.1.0, unreleased).
