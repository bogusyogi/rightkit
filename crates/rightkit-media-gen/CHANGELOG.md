# Changelog

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-gen` (genright).
- `lease` (caller leases that tie a detached local-image runner to the process waiting for it) and `runner` (detached `<engine> job run <id>` runners, their scrubbed stderr logs and resume of recoverable jobs), moved from `genright-engine` `lease.rs` and `worker.rs`. The host engine binary must answer `job run <id>` and `job cancel <id>`.
- `media_commands`: the `media.*` command surface (`media.modalities`, `media.generate`, `media.route`, `media.route_prefs`, `media.still_finish`) and its CLI word map, moved from `genright-engine` `cli_media.rs`; still finishing runs on `rightkit-compose` (feature `still-finish`).
- `voices`: the voice library (`studio.voices.*`: list, presets, provider voices, ElevenLabs and local clones, design, batch, preview, export, legacy voice-folder import), moved from `genright-studio` `voices.rs` with `data/voices/builtin.json`. The local voice-model installer is reached through `voices::set_install_hooks` (the host registers `rightkit-remote-compute`'s model installer). The `studio.voices.import_voiceright` command name is kept for wire compatibility.
- `reference`: `reference.save_png` (store a UI-made PNG such as a painted mask under `<app data>/references/` so a generate request can name it), moved from `genright-engine` `reference_cmd.rs`.
