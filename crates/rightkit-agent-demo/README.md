# rightkit-agent-demo

Consented agent demo mode for Tauri hosts. A person approves a request, an agent drives exactly one catalog app through `rightkit-automation`, and the host records that app's window into a sealed local bundle.

- `session`: the phase machine (idle, awaiting consent, armed, recording, stopping, sealed, killed) and session ids.
- `catalog`: the apps an agent may ask for, and the apps it can never drive. The host product itself is always denied.
- `driver`: one automation engine per session with a policy that allows only the consented app.
- `recorder`: the recorder worker thread (capture, pump, events, verify).
- `host`: consent, orchestration, stop, kill and seal; `service`: the `demo.*` daemon handler.
- `badge`, `kill`, `tray`: the always-visible badge window, the kill hotkey and the tray stop item.
- `store`: the on/off flag, bundle folder and a content-free audit log under the host's app data.
- `commands`: the Tauri commands the host UI calls.

## Using it

Depend on the crate only behind a non-default feature of your app so a release build contains none of it.

```rust,ignore
use rightkit_agent_demo::{plugin, DemoIdentity};

let identity = DemoIdentity::new("MyApp", "myapp", "com.example.myapp", "myapp.exe")
    .with_version(env!("CARGO_PKG_VERSION"));
tauri::Builder::default().plugin(plugin(identity));
```

List the commands from `rightkit_agent_demo::commands` in your own `generate_handler!`, and grant the `demo-badge` window the capability in `data/capability.json`.

`examples/demo-fixture.rs` is a Windows-only window (a text field, a Submit button and a result label) used as the acceptance target. It has no dependencies.
