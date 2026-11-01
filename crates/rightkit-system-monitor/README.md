# rightkit-system-monitor

Live system monitoring for desktop apps.

- Crate root: `system_status`, `procs`, `procs_gpu_settled`, `LiveSampler`, memory pressure, and a disk listing that never blocks on a hung mount (stale drives are reported as not responding).
- `monitor`: extended status (battery, network counters, listening ports) over the process rows.
- `processes`, `process_inspect`, `process_sampler`: process groups with evidence, per-process inspection with a deadline, and one shared process table.
- `gpu_temp`, `drive_health`: GPU temperature and drive health (SMART) with history.
- `process_details_windows` (Windows): thread counts, paths and per-process GPU use.

The host app passes its state folder with `set_state_directory` (to remember the last drive listing) and its GPU setting with `process_details_windows::set_process_gpu_enabled`.

Moved into RightKit from Pulse's shared core on 2026-11-01.
