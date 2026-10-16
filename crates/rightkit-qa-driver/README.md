# rightkit-qa-driver

**Deprecated.** Use [`rightkit-qa`](../rightkit-qa/README.md) with its `control` feature for the in-app QA control server, and [`rightkit-automation`](../rightkit-automation/README.md) for background native input, the admission gate, the input lease and the key and JSON helpers. This crate stays for one release (0.5.0) as a shim so an app compiles unchanged after a version bump, and is then yanked.

Catalog capability: `desktop-control`. Published versions are listed in [INDEX.md](../../INDEX.md).

## What the shim keeps

| Path | Now |
|---|---|
| `admission`, `events`, `lease`, `lease_store` | Re-exports of `rightkit_automation::control::*`. |
| `mac` | Re-export of `rightkit_automation::macos::background` (macOS only). |
| `offscreen` | Re-export of `rightkit_automation::offscreen`, plus the Tauri helpers `start_for_window` and `start_from_env` defined here. |
| `json`, `keys` | Re-exports of `rightkit_automation::json` and `rightkit_automation::keys::appkit`. |
| `embedded` | Re-export of `rightkit_qa::embedded` (the QA-only control server), only with `qa-control`. |
| `webdriver`, `cli` | Still defined here: the WebDriver bridge and the macOS-only `rightkit-qa-driver` CLI. No new code goes here. |

## Cargo features (all map onto the new crates)

| Feature | Maps to |
|---|---|
| `qa-control` | `tauri-plugin`, `rightkit-qa/control`. QA ONLY, NEVER SHIPPED. Binaries carry the marker `RIGHTKIT-QA-CONTROL-SERVER-PRESENT` and `right-release` refuses them, as before. |
| `wv2-0_38`, `wv2-0_39` | The same WebView2 generation features, forwarded to `rightkit-qa/wv2-*` when `qa-control` is on. On Windows `qa-control` needs exactly one. |
| `tauri-plugin` | `tauri`: the window helpers only. It does not compile the control server. |
| `offscreen-private-spi` | `rightkit-automation/offscreen-private-spi`. |

## Migration

Replace the dependency and, for the control server, the import path. The full list is at the top of the [`rightkit-qa` README](../rightkit-qa/README.md).

```toml
# before
rightkit-qa-driver = { version = "0.4", optional = true, features = ["qa-control", "wv2-0_38"] }
# after
rightkit-qa = { version = "0.6", optional = true, features = ["control", "wv2-0_38"] }
```

`use rightkit_qa_driver::embedded::Control` becomes `use rightkit_qa::embedded::Control`. Documentation for the server (protocol, "QA control never ships", viewport sizing) moved to [`../rightkit-qa/docs/CONTROL-SERVER.md`](../rightkit-qa/docs/CONTROL-SERVER.md).

## Tests and examples

`tests/reexports.rs` pins the re-exported paths. `tests/gated_paths.rs`, `tests/serve_auth.rs`, `tests/offscreen_macos.rs` and `tests/offscreen_windows.rs` cover the CLI, the bridge and offscreen launch. `examples/offscreen_fixture*.rs` are the offscreen fixtures. The control-server unit tests and the `control-test-app` example moved to `rightkit-qa`.

Changes are listed in [CHANGELOG.md](CHANGELOG.md).
