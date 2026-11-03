# @rightkit/send-ui

The nearby-sharing screen for apps built on `rightkit-send`: devices to send to, incoming requests to answer, transfers, and the per-device Allow, Ask and Deny list. It talks to the crate through one JSON command, so a host writes no `share_*` wrapper per action.

| Item | Value |
|---|---|
| Where it runs | App webview UI (React) |
| Rust pair | `rightkit-send` (`rightkit_send::commands`) |
| Version | 0.1.0 |
| License | MIT OR Apache-2.0 |

## Host wiring

Rust, once:

```rust
#[tauri::command]
fn send_invoke(state: State<SendState>, command: String, args: serde_json::Value)
    -> Result<serde_json::Value, rightkit_send::Error> {
    rightkit_send::commands::invoke(&state.service, &command, &args)
}
```

Forward the service's `Event` callback to the webview (for example `app.emit("send-changed", ())`) so the panel reloads on change instead of polling.

TypeScript:

```tsx
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { SendPanel, createSendClient } from "@rightkit/send-ui";
import "@rightkit/send-ui/send-ui.css";

const client = createSendClient((command, args) => invoke("send_invoke", { command, args }));
const subscribe = (onChange: () => void) => {
  const stopped = listen("send-changed", onChange);
  return () => void stopped.then((stop) => stop());
};

<SendPanel client={client} subscribe={subscribe} pickPaths={pickFilesWithTheHostDialog} />;
```

`pickPaths` is the host's file dialog (it returns absolute paths). Omit it and the Send files button is not shown. Every string is overridable through `words`; the default `error` words a typed error by its `code`.

## Entry points

| Import | Contents |
|---|---|
| `@rightkit/send-ui` | `SendPanel`, `useSend`, `createSendClient`, `defaultWords`, `isSendError` and the wire types (`SendSnapshot`, `Transfer`, `Incoming`, `TrustEntry`, ...) |
| `@rightkit/send-ui/send-ui.css` | Layout stylesheet, colours from the host's tokens with fallbacks |

Peer dependency: `react` `>=18 <21`.

## Notes

- The panel holds no state the crate does not: it reads `snapshot`, `trustList` and `trustDefault`, runs a command, and reloads.
- Trust rows are keyed by the stored fingerprint. A device on the network cannot be forgotten (the crate refuses with `device_nearby`), so the button is disabled for it.
- No unit tests (end-to-end only).
