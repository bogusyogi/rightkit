# Changelog

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-gen` (genright).
- `lease` (caller leases that tie a detached local-image runner to the process waiting for it) and `runner` (detached `<engine> job run <id>` runners, their scrubbed stderr logs and resume of recoverable jobs), moved from `genright-engine` `lease.rs` and `worker.rs`. The host engine binary must answer `job run <id>` and `job cancel <id>`.
- `media_commands`: the `media.*` command surface (`media.modalities`, `media.generate`, `media.route`, `media.route_prefs`, `media.still_finish`) and its CLI word map, moved from `genright-engine` `cli_media.rs`; still finishing runs on `rightkit-compose` (feature `still-finish`).
