# rightkit-agent-tools (`right-ctl`)

One typed tool interface for agents over the Right Suite. A `Toolbox` registers named tools with JSON argument schemas and an effect class, and every call passes one gate: the hard exposure rules, a per-caller allowlist, argument validation, the caller's admission hook, and a journal. The crate also builds the `right-ctl` CLI on the same toolbox.

Catalog capability: `agent-tools`. Not published (`publish = false`).

## What it owns

| Tool family | Backend | Feature |
|---|---|---|
| `<app>.*` | Each app's in-app control service, over `rightkit-daemon` (always compiled) | none |
| `desktop.*` | `rightkit-automation` (macOS accessibility tree, Windows UI Automation) | `automation` |
| `browser.*` | `rightkit-browser` (Chrome DevTools Protocol) | `browser` |
| `speech.*` | `rightkit-speech-client` (shared speech service) | `speech` |

HeardRight's rules are built in and cannot be configured: no dictation, text delivery, pasteboard or history tool can be registered or called (`exposure` module).

## When to use it

- Use it when an agent must drive an installed, signed Right Suite app in the background, by process id, through `right-ctl` or the MCP host.
- Use it when a caller needs one allowlisted, journaled tool surface across apps.

Do not use it to take foreground input or to click the screen with computer-use tools on Right Suite apps. Do not use it for dictation or text delivery; the exposure rules block those.

## Cargo features

| Feature | Default | Pulls in | Adds |
|---|---|---|---|
| `automation` | yes | `rightkit-automation` | `desktop.*` tools |
| `browser` | yes | `rightkit-browser`, `tokio` (`rt-multi-thread`, `time`) | `browser.*` tools |
| `speech` | yes | `rightkit-speech-client` | `speech.*` tools |
| `mcp` | no | `rightkit-mcp` | Hosts the toolbox over MCP (`right-ctl mcp-serve`). The `e2e` test requires it. |

## Usage

Illustrative CLI calls. Tool names and argument keys match `src/backends/desktop.rs`:

```sh
echo '[{"tool":"desktop.apps","args":{}}]' | right-ctl run
echo '[{"tool":"desktop.snapshot","args":{"pid":1234}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
# act on an element from that snapshot (snapshot id and #id come from desktop.snapshot)
echo '[{"tool":"desktop.act","args":{"snapshot":"<snapshot id>","element":"#e2248d6f4","action":{"kind":"press"}}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
echo '[{"tool":"desktop.screenshot","args":{"pid":1234,"path":"/abs/out.png"}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
```

Other CLI modes from the usage text: `right-ctl --list` (live services), `right-ctl <app> --tools` (JSON tool schemas for that app), `right-ctl <app> <command> [args]`, and `right-ctl run` (one session, so snapshot ids stay valid across a JSON array of calls).

## Desktop tools

- `desktop.status`, `desktop.apps`, `desktop.windows`: observe. No allowlist needed.
- `desktop.snapshot`: accessibility outline with stable `#ids`.
- `desktop.act`: `action.kind` is `press`, `focus`, `set_value`, `type_text`, `keys`, `click`, `scroll` or `menu_press`. Risky actions need `confirm`.
- `desktop.screenshot`: per-window, redacted, absolute path.
- `desktop.launch`, `desktop.terminate`: launch or quit an allowlisted app.
- `desktop.lease_acquire`, `desktop.lease_status`, `desktop.lease_release`: exclusive input lease.

Actions touch only apps named in `RIGHT_CTL_DESKTOP_ALLOW` (comma-separated bundle ids, read per call). Input escalates through tiers: semantic accessibility action, in-app, then per-pid events. `desktop.act` accepts `semantic`, `in_app` or `targeted` as `max_tier` (default `targeted`). `foreground` is refused with `ERR_NOT_ALLOWED` (`src/backends/desktop.rs:38-48`), so agents cannot reach foreground input through this interface.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | OK |
| 1 | Tool error |
| 2 | Usage or invalid arguments, or unknown tool |
| 3 | Refused by exposure, allowlist or admission |

Results go to stdout as JSON. Errors go to stderr as `{"error":{code,message,retryable}}`.

## Installation and permissions

`rightkit/scripts/install-right-ctl.sh` builds and installs `right-ctl` into the managed agent-bin. `desktop.status` reports whether macOS Accessibility and Screen Recording are granted. If either is false, grant it in System Settings, Privacy & Security. The install script signs with a stable identifier so the grants survive rebuilds (`rightkit/scripts/install-right-ctl.sh:4-5`).

## Platform support

Desktop tools need macOS Accessibility. No test in this crate calls a `desktop.*` tool (`tests/e2e.rs` has none); the desktop backend's tests live in `rightkit-automation`. The Windows backend comes through `rightkit-automation`; see that crate's README for its status.

## Version and changes

Published version: not published. See [INDEX.md](../../INDEX.md). No CHANGELOG.md in this crate.
