# rightkit-system-monitor

Live system monitoring for desktop apps.

- Crate root: `system_status`, `procs`, `procs_gpu_settled`, `LiveSampler`, memory pressure, and a disk listing that never blocks on a hung mount (stale drives are reported as not responding).
- `monitor`: extended status (battery, network counters, listening ports) over the process rows.
- `processes`, `process_inspect`, `process_sampler`: process groups with evidence, per-process inspection with a deadline, and one shared process table.
- `gpu_temp`, `drive_health`: GPU temperature and drive health (SMART) with history.
- `system_history`: bounded rolling CPU, memory, swap & network samples. Host owns its runner thread; `run(wall_ms)` samples without preference polling. Admission starts enabled. `pause()` rejects leases, wakes normal/warmup waits & suppresses in-flight publication while preserving retained history & cursor. Serialize preference transitions: pause, join old or pending runner handle, call `resume()`, then spawn replacement only if it returns true. Join pending handles too: admission cannot detect a thread before `run` starts. For an initially disabled preference, pause before spawning. Resume refuses terminal stop or an admitted runner; each new run initializes fresh CPU/network baselines. `stop()` is terminal: call it at shutdown, then join before dropping host callback resources. In-flight OS reads/callbacks must return before joining finishes. Visibility lease IDs are at most 256 bytes & at most 128 leases can be active.
- `process_details_windows` (Windows): thread counts, paths and per-process GPU use.

The host app passes its state folder with `set_state_directory` (to remember the last drive listing) and its GPU setting with `process_details_windows::set_process_gpu_enabled`.
