# rightkit-agent-tools (`right-ctl`)

One typed tool interface for agents over rightkit-automation (desktop), rightkit-browser and each Right Suite app's own commands, plus the `right-ctl` CLI. **This is how agents drive installed, signed apps in the background**: by process id through the macOS Accessibility tree and per-process input. No debug build, no QA plugin, no focus change, no real mouse or keyboard. Never use screen-clicking computer-use tools on Right Suite apps.

## Installed

`right-ctl` lives in the managed agent-bin (`/Volumes/D/.rightkit-managed/agent-bin/right-ctl`, on every agent's PATH), Developer-ID signed as `com.orthiclabs.rightkit.right-ctl`. Rebuild and reinstall with `tools/rightkit/scripts/install-right-ctl.sh`.

## Desktop driving

Actions only touch apps named in `RIGHT_CTL_DESKTOP_ALLOW` (comma-separated bundle ids, per call). Reads such as `desktop.apps` / `desktop.windows` / `desktop.status` need no allowlist.

```sh
echo '[{"tool":"desktop.apps","args":{}}]' | right-ctl run
echo '[{"tool":"desktop.snapshot","args":{"pid":1234}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
# act on an element from that snapshot (snapshot id + element #id from desktop.snapshot's result)
echo '[{"tool":"desktop.act","args":{"snapshot":"<snapshot id>","element":"#e2248d6f4","action":{"kind":"press"}}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
echo '[{"tool":"desktop.screenshot","args":{"pid":1234,"path":"/abs/out.png"}}]' | RIGHT_CTL_DESKTOP_ALLOW=dev.orthic.cockpit.hub right-ctl run
```

Tools: `desktop.status`, `desktop.apps`, `desktop.windows`, `desktop.snapshot` (AX outline with stable `#ids`), `desktop.act` (`action.kind`: press, focus, set_value, type_text, keys, click, scroll, menu_press; risky actions need `confirm`), `desktop.screenshot` (per-window, redacted, absolute path), `desktop.launch` (background launch of an allowlisted app), `desktop.terminate`, and `desktop.lease_acquire` / `lease_status` / `lease_release` (exclusive input lease). Argument schemas: `right-ctl <app> --tools` (JSON). Use `right-ctl run` to keep one session (snapshot ids stay valid) across a JSON array of calls. Input escalates through tiers: semantic AX action, in-app, per-pid events; foreground input is opt-in only.

Exit codes: 0 ok, 1 tool error, 2 usage, 3 refused (exposure, allowlist, admission). Results go to stdout as JSON; errors are `{"error":{code,message,retryable}}` on stderr.

## Permissions

macOS Accessibility and Screen Recording are attributed to the responsible process (the agent app or terminal running `right-ctl`). `desktop.status` reports both; if either is false, grant it in System Settings, Privacy & Security, for that app.
