# Changelog

## 0.3.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7. 0.2.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.3.0 instead of changing 0.2.0 in place (#1572). No source change in this crate since 0.2.0.

## 0.2.0

- Requires `rightkit-notch-ui` 0.2.0 (host-defined notch cells), whose `card::CardContent` and `layout::Edge` appear in this crate's Windows API (`burst::burst_preview_panel`, the strip's placement): breaking for that reason only; the strip and burst code is unchanged.
- `json` is the launcher core's module (`rightkit_launcher::json`, `rightkit-launcher` 0.2.0) re-exported under the old path; this crate's identical copy is deleted. Same API.
- Taken on the recommended option of the 2026-11-11 shape review (launcher and notch redraws), pending the owner's confirmation.

- The crate-level documentation link to `detect` names the function (`fn@detect`): `detect` is both a module and a function, and rustdoc refused the ambiguous link under `-D warnings` (Mac release compile 2026-11-07).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): `detect`, `model`, `search`, `seal`.
- Added Windows `win` (clipboard calls), `img` (picture conversion) and `shelf` (nearby shelf).
- Added Windows `strip` (the clipboard strip, from `clip_strip/*`) on rightkit-notch-ui.
- Added Windows `burst` (the copy burst, from `clip_paste/burst.rs`); the preview now returns the card content and the host's `burst_changed` replaces the send-panel call.
