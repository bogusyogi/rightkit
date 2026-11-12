# Changelog

## 0.3.0

- Breaking for dependents: dependency requirements `rightkit-automation` 0.6 -> 0.7. 0.2.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.3.0 instead of changing 0.2.0 in place (#1572). No source change in this crate since 0.2.0.

## 0.2.0

- Breaking: the crate is the Mac-style shortcut layer only, one handler on `rightkit_hotkeys::windows::hub` (`rightkit-hotkeys` 0.1.4). `keys::apply(Shortcuts { mac, save_find_tab_close, guards })` switches it (was `start`/`apply(mac_shortcuts, screenshot_keys)` plus the `altRemapSaveFindTabClose` and `altAsCtrlGuards` switches read through the services); `keys::current`, `keys::ORDER`. `send_input` and `paste` stay, tagged with the hub's tag.
- The hook hub (one keyboard and one mouse hook, modifier tracking, swallowed-key bookkeeping, `reinstall`, `physically_down`) moved to `rightkit-hotkeys` 0.1.4 as `windows::hub`, a generic ordered handler chain. The launcher's type-ahead capture (`capture_begin`, `capture_take`, `capture_release`) moved to `rightkit-launcher-win` 0.1.0 (`type_ahead`).
- Removed, as Pulse's wiring rather than a desktop tweak: the screenshot chords and session keys, snippet keyword and click forwarding, the tool wheel's mouse handler, the copy burst's paste counting, the strip's foreground check, the launcher hook keep-alive and the single extra Alt-chord handler (`set_extra_handler`, `Chord`, `ExtraHandler`, `ScreenshotRequest`, `Keys`, `screenshots_on`, `launcher_needs_hook`, `snippets_need_hook`, `wheel_needs_mouse`). An application registers such handlers on the hub itself (before `keys::ORDER` when it must see an Alt chord first). `services::Services` keeps only `log_info`.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation.

## 0.1.0 (unreleased)

- First RightKit release, moved from `pulse-windows` (Pulse): `keys`.
