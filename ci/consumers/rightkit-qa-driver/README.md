# rightkit-qa-driver 0.7.0 registry consumer

PK14 supplemental compatibility consumer, newly reviewed 2026-10-23. Original
PK14 packet names QA owner packages; it does not name this candidate. This
recipe supplies additional shim compatibility evidence, not PK14 capability
closure.

Standalone consumer pins `rightkit-qa-driver =0.7.0` exactly with default
features. No path, Git, workspace dependency, patch, foreign registry, or
test seam. No committed `Cargo.lock`; qualification harness retains its lock
outside this recipe. Empty `[workspace]` makes this consumer independent of
repository workspace.

## Assertions

| Public surface | Executed behavior |
|---|---|
| `admission`, `events` | Denying approval never calls executor, returns `Denied` with `executed = false` & no value, retains matching settlement, & emits exactly one matching approval-denied event. |
| `lease` | Current epoch & granted sequence apply supplied value once; snapshot advances next sequence; next input applies with matching acknowledgement. `MemoryLeaseStore` remains usable through old reexport. |
| `json`, `keys`, `offscreen` | JSON escaping, string-field lookup & base64 retain values; Cmd+A & Delete retain AppKit codes; offscreen environment constant retains name. |

Assertions derive from `crates/rightkit-qa-driver/tests/reexports.rs` & public
reexports in `crates/rightkit-qa-driver/src/lib.rs`. Runs entirely in memory;
does not read/write user stores, launch processes, bind servers, access network,
or deliver OS input.

## Excluded coverage

No `qa-control`, `tauri-plugin`, `wv2-*`, or `offscreen-private-spi` feature is
enabled. Embedded control server, WebDriver bridge, CLI, macOS AX/AppKit input,
Windows UIA/WebView2, offscreen window placement, private SPI, durable file
storage, & actual desktop effects need separate qualification. AppKit helper
assertions concern pure key mappings on every host, not native event delivery.

Root integration runs recipe validation plus standalone registry qualification
through existing consumer harness, whose executable row uses
`cargo run --locked --offline` after harness lock/fetch preparation. Source
authoring ran no build, test, or qualification command.
