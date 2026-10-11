# Pulse cutover audit

Reviewed Pulse HEAD `64839ce8b66628b883527bc85b35c9c2a08c0735` across Mac notch, Tauri hub, Windows native, Rust core, & Swift targets.

| Result | Count |
|---|---:|
| Y | 7 |
| M | 0 |
| N | 16 |
| NA | 74 |
| Total | 97 |

## Shared coverage

Y rows use active production callers for `rightkit-control`, `rightkit-db`, `rightkit-fsindex`, `rightkit-search`, `@rightkit/app-shell`, `@rightkit/platform-ui`, & `@rightkit/release`.

Delivery flags: Pulse pins `@rightkit/app-shell` `0.5.0` vs catalog `0.6.1`, & `@rightkit/platform-ui` `0.2.1` vs catalog `0.2.2`. `rightkit-control` is optional QA-native wiring; `rightkit-release` is a release helper loaded by Mac packaging.

## Cutover gaps

N rows show active same-runtime Pulse-local overlap across fs, hotkeys, http, logs, overlay, process, send, settings, watch, settings UI, theme, & Swift Diagnostics, AppleCredentials, Overlay, Watch, UI.

Highest-risk path is send: shared `rightkit-send` exists, but Pulse active path remains `pulse_core::localsend` in Rust plus `NearbySharing.swift`; source hashes & read time are recorded in JSON after concurrent-edit recheck, with no Pulse import or dependency routing send through shared core.

## Migration tasks

1. Route Rust/Tauri & Mac nearby sharing through versioned `rightkit-send`; remove or retire `core/src/localsend` after parity checks.
2. Replace local same-runtime overlap in priority order: fs, settings, http, process, hotkeys, overlay, logs, watch, theme, settings UI, & Swift modules.
3. Reassess NA capabilities only when Pulse gains matching runtime/API callers; cargo OAuth/secrets/service/cache/shell/tauri & npm i18n/logs/shell/tauri/updates currently have incompatible or absent host contracts.
4. Resolve app-shell & platform-ui pins against verified published versions & required APIs, then run focused package tests & standalone app parity checks from primary checkout. Catalog versions alone are not publication proof.

No M rows were supported by source evidence: no shared/local dual path met M rule. Full row-level evidence, owner paths, target applicability, flags, & remaining work are in [pulse.json](./pulse.json).

