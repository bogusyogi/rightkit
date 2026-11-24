# Changelog

## 0.4.0

- Disabling default features leaves only the portable detection API. Enable `history` if a consumer disables defaults and uses the persisted model, search, host or native modules. Existing default-feature consumers keep the full API.
- Host hooks for native capture: `Host::capture_reads_enabled()` (Windows; default `true`) lets the app stop the native reader from reading the desktop session while explicit fixture copies still work, and `Host::log_win32_error(op, error, ctx)` (Windows; default the informational log) hands the host the structured Windows error. `burst::burst_preview()` returns the hovered burst item (position, total, selected, kind, snippet, image path) for hosts that draw their own card; `burst_preview_panel()` is built on it and unchanged.
- Paste and capture record the clipboard sequence under the clipboard guard: the own-write marker is taken while the clipboard is open (capture, local paste and shared snippets read and update the same marker), and the sequence number kept for the restore decision is the one the write returned, so an immediate foreign copy is neither marked as the host's own nor overwritten by the restore.
- `Host::log_png_error(error)` (Windows; default keeps the `clipboard_png_failed` event) hands the host the structured WIC error of a failed PNG re-encode.
- Breaking: the packaged strip presentation is removed (`strip`, `strip::{draw, kinds, popover, shot}`, and with it the `rightkit-notch-ui` and `rightkit-hotkeys` dependencies): hosts render their own strip and burst card from `model`, `burst::burst_preview()` and `paste`. `strip::drag` is replaced by the `drag` module: virtual files offered to the Shell as an indexed PNG stream positioned at its end, plus direct PNG and DIBV5; `GetData` and `QueryGetData` share one format validation; real file cards and Send items drag their real paths. `qa-views` now only enables clipboard fixture injection.
- Clipboard pictures are converted in memory: `img::decode_png(bytes, max_side)` decodes a stored PNG from a memory stream, the card thumbnail is made from the bytes, `encode_png` and the DIBV5 conversions no longer go through a plaintext file, and the store's `.tmp` folder and the helpers that fed it (`model::tmp_file`, `seal::with_plain_file`) are gone. Opening the store no longer clears a `.tmp` folder, and clears `.undo` only when this process owns the store.

## 0.3.2

- `strip::drag::{Drag, Landing, Outcome, run}` are `pub` (were `pub(crate)`) and `strip::drag::ole_init()` is new: a once-per-process OLE initialisation for a host's own drag of files out of the notch (Pulse's Send card drag), which carries no strip items and so never goes through the strip's own `init`. Nothing calls it unless the host does; the strip's behaviour is unchanged.
- `strip::{shot, stub}` under the existing `qa-views` feature (off by default, never in a shipped build): the strip as a picture for a host's CI view renderer, and the plain notch stub it hangs from. Nothing changes without the feature.

## 0.3.1

- `Host::product_name` (default `RightKit`): the application's name in the history's window class names (`<name>ClipboardListener`, `<name>ClipStrip`, `<name>ClipPreview`), window titles (`<name> clipboard`, `<name> Clipboard`, `<name> Clipboard Preview`) and thread names (`<name lower case>-clip-...`). The two paste threads were named `pulse-clip-paste` and `pulse-clip-restore` whatever the host; they now follow the product name like the others (`rightkit-clip-paste` by default).

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
