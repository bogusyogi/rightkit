# rightkit-agent-tools (`right-ctl`)

One typed tool interface for agents over the Right Suite. A `Toolbox` registers named tools with JSON argument schemas and an effect class, and every call passes one gate: the hard exposure rules, a per-caller allowlist, argument validation, the caller's admission hook, and a journal. The crate also builds the `right-ctl` CLI on the same toolbox.

Catalog capability: `agent-tools`.

## What it owns

| Tool family | Backend | Feature |
|---|---|---|
| `<app>.*` | Each app's in-app control service, over `rightkit-daemon` (always compiled) | none |
| `desktop.*` | `rightkit-automation` (macOS accessibility tree, Windows UI Automation) | `automation` |
| `browser.*` | `rightkit-browser` (Chrome DevTools Protocol) | `browser` |
| `speech.*` | `rightkit-speech-client` (shared speech service) | `speech` |

Exposure rules are built in and cannot be loosened (`exposure` module). No pasteboard or paste tool can be registered or called, for any app. A dictation app (`AppProfile::with_dictation_rules`, set by the built-in HeardRight profile; `Toolbox::protect_dictation` for a hand-built toolbox) also refuses dictation, text-delivery and history names. Other apps keep their own `history` or `delivery` commands.

## When to use it

- Use it when an agent must drive an installed, signed Right Suite app in the background, by process id, through `right-ctl`.
- Use it when a caller needs one allowlisted, journaled tool surface across apps.

Do not use it to take foreground input or to click the screen with computer-use tools on Right Suite apps. Do not use it for dictation or text delivery; the exposure rules block those.

## Cargo features

| Feature | Default | Pulls in | Adds |
|---|---|---|---|
| `automation` | yes | `rightkit-automation` | `desktop.*` tools |
| `browser` | yes | `rightkit-browser`, `tokio` (`rt-multi-thread`, `time`) | `browser.*` tools |
| `speech` | yes | `rightkit-speech-client` | `speech.*` tools |

## Usage

Illustrative CLI calls. Tool names and argument keys match `src/backends/desktop.rs`:

```sh
echo '[{"tool":"desktop.apps","args":{}}]' | right-ctl run
echo '[{"tool":"desktop.snapshot","args":{"pid":1234}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
# act on an element from that snapshot (snapshot id and #id come from desktop.snapshot)
echo '[{"tool":"desktop.act","args":{"snapshot":"<snapshot id>","element":"#e2248d6f4","action":{"kind":"press"}}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
echo '[{"tool":"desktop.screenshot","args":{"pid":1234,"path":"/abs/out.png"}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
```

## Driving an app: send the whole sequence in one call

A single `right-ctl <app> <method>` call is one round trip, and an agent that issues one command per step spends most of its time on process starts and model turns, not on the app. Drive an app by sending the whole sequence at once, in either of two ways:

- `right-ctl run` takes a JSON array of tool calls on stdin and runs them in one session, stopping at the first failure.
- The in-app `batch` method takes `steps:[{method,params}]` and runs them inside the app in one round trip, stopping at the first failure. Each step is still checked against the caller's allowlist and effect gate; a method the app's profile does not expose cannot ride in a batch.

```sh
right-ctl run <<'JSON'
[{"tool":"demo.batch","args":{"steps":[
  {"method":"click","params":{"x":120,"y":40}},
  {"method":"type","params":{"text":"hello"}},
  {"method":"wait","params":{"js":"return document.title==='Done';","timeoutMs":5000}},
  {"method":"screenshot","params":{"inline":true}}]}}]
JSON
```

A quoted heredoc (`<<'JSON'`) keeps single quotes inside the JavaScript intact; inside a single-quoted `echo '...'` they end the string.

Use `wait` (one call that waits in the page until a JavaScript function body returns something truthy) instead of polling `eval`, and `screenshot` with `inline:true` to get the PNG as base64 in the reply instead of a file to read. Input events are sent back to back; pass `paceMs` on `click`, `drag`, `key` or `type` only for a page that needs real time between events.

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

Published version: see [INDEX.md](../../INDEX.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).
