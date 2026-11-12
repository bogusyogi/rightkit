# Changelog

## 0.3.0

- Breaking for dependents: dependency requirements `rightkit-app-settings` 0.3 -> 0.4. 0.2.0 was published before these requirements were raised; a semver-breaking dependency is a breaking change, so they ship as 0.3.0 instead of changing 0.2.0 in place (#1572). No source change in this crate since 0.2.0.

## 0.2.0 (unpublished)

- rightkit#1483: sysinfo `=0.39.6` to `=0.40.0`. Decision (sysinfo 0.40 makes `System::new`, `Disks::new_with_refreshed_list_specifics` and `Networks::new_with_refreshed_list` return `Result`; `LiveReading` has no unavailable form, so a defaulted infallible constructor would publish a fake reading): constructors are fallible. BREAKING: `LiveSampler::new() -> Result<LiveSampler, SamplerError>` and the `Default` impl is removed; `process_sampler::with_processes` returns `Result<R, SamplerError>`. New public `SamplerError` (Display, Error). Readings that already carry an unavailable form degrade instead of failing: `system_status()` returns Unavailable CPU, memory and swap metrics with the reason as label; `read_network_counters()` is an unavailable observation; `procs()`, `list_disks()`, `mount_points()` and the Windows process details are empty. `live_identity` returns `None`. Callers of `LiveSampler::new` or `with_processes` (Pulse) must handle the `Result` and move to sysinfo 0.40.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from Pulse (`pulse-core`: system status and process rows, monitor, processes, process inspection and sampler, GPU temperature, drive health, Windows process details).
- The state folder and the per-process GPU switch are parameters the host app passes instead of reads of its settings.
