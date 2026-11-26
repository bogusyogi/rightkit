# Changelog

## 0.1.4

- Fixed: `sync::claude_running_for(root)` counted a Claude Desktop that names no data folder as using every `root`. Such a Desktop runs on the platform default folder (account slots are always started with `--user-data-dir`), so it now counts only when `root` is that default; with no platform default it still counts. With Desktop open on the host, `claude restore` into any other folder refused with `claude_running` (Pulse's `claude_accounts_cli_journey` on the Mac, bogusyogi/pulse#643).

## 0.1.3

- Logic moved from Pulse into this crate (bogusyogi/pulse#643): see the module docs for what each new module holds.

## 0.1.2

- Fixed: the "is Claude Desktop running" check honours an explicit data folder. `sync::claude_running_for(root)` read the process table with a refresh that leaves out each process's executable and command line, so no process showed a `--type=` helper marker or a `--user-data-dir`, and every Desktop main process of any profile counted as using `root`. With Desktop running on the host, a check of a different folder (a test root, another profile) reported "running" and refused the sync, restore or switch. The refresh now reads the command line and the executable, as the Desktop instance listing already does, so a main process counts only when its own or its helpers' `--user-data-dir` is `root`; one that names no folder yet (still launching) still counts, and an unreadable process table still counts as running. No public signature changes. Not compiled or run here; rustfmt checked.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (`pulse-core`: claude_cli, claude_names, claude_sync, claude_desktop, and the Desktop data-folder discovery from chat/deliver_claude). Host state and settings come from `rightkit-app-host`.
