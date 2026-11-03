# Changelog

## 0.1.2 (unpublished)

- rightkit#1483: sysinfo 0.40 and rightkit-system-monitor 0.2.0. `System::new()` and `process_sampler::with_processes` now return `Result`; where the OS gives no process table: listings are empty, `procs.list` refuses with `unavailable`, an action-time identity check finds nothing to act on, and a process that cannot be shown gone counts as alive.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (`pulse-core`: app_manager, apps, apps_windows, process_control, subprocess).
- The host identity, state folder and helper approval are parameters the host app passes instead of fixed names and reads of its settings.
