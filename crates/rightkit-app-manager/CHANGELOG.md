# Changelog

## 0.2.1

- Added `set_privileged_helper_probe`: the host supplies a function asked on every helper check, for a helper state that changes while the process runs. Without it `set_privileged_helper_enabled` keeps working as before.
- The two update refusals that said "this app" now name the host app: "The feed is not a Sparkle appcast <display name> can read." and "...no update source <display name> can use." The display name is `HostIdentity::display_name`; unset keeps "this app".

## 0.2.0

- Breaking for dependents: dependency requirements `rightkit-disk-cleanup` 0.1 -> 0.2, `rightkit-system-monitor` 0.2 -> 0.3. 0.1.2 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.2.0 instead of changing 0.1.2 in place (#1572). No source change in this crate since 0.1.2.

## 0.1.2 (unpublished)

- rightkit#1483: sysinfo 0.40 and rightkit-system-monitor 0.2.0. `System::new()` and `process_sampler::with_processes` now return `Result`; where the OS gives no process table: listings are empty, `procs.list` refuses with `unavailable`, an action-time identity check finds nothing to act on, and a process that cannot be shown gone counts as alive.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (`pulse-core`: app_manager, apps, apps_windows, process_control, subprocess).
- The host identity, state folder and helper approval are parameters the host app passes instead of fixed names and reads of its settings.
