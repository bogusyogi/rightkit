# Changelog

## 0.2.2

- Publish the dependency requirements main already declares: `rightkit-fs` 0.5.0, where the published 0.2.1 requires 0.4.9. The 0.2.1 manifest on crates.io stays as published; this version carries the new requirements.

## 0.2.1

Taken from GenRight's shipped `remote.rs` and `rental.rs` so it can delete its copies. Every default keeps RightKit's behaviour; the options are the host names `rightkit-media-gen` already carries.

- Changed: user-visible text names the app through `rightkit_media_gen::host::MediaHostConfig::app_name` (`configure_media`): "save it in {app} Settings" (default "the app"), "a rental needs the Vast API key saved in {app} first" (default "the app"), "{app} never invents a remote workload" (default "this crate"), "no Vast API key is stored for {app}" (default "this app"), "another {app} process is changing the rented-worker records" (default "another process"), the rental cost note ("not known to {app}" / "not known here") and the enforcement limits text. `rental::honest_limits()` returns `HONEST_LIMITS` worded for the host ("while GenRight runs", "saved in GenRight"); the rental list, enforcement and worker responses carry it. `HONEST_LIMITS` stays as the neutral text. With app name `GenRight` every one of these is GenRight's wording byte for byte.
- Changed: the Vast API base literal passed to `base_url_override` is `RIGHTKIT_VAST_BASE` (GenRight's name; was `VAST_API_BASE`). `base_url_override` never honours an override, so nothing is read from either variable: production always uses `https://console.vast.ai`. The doc comment said otherwise and is corrected.
- The workdir default (`~/<namespace>` and `/root/<namespace>`), the Vast `client_id` and the rental label prefix come from `HostConfig::settings_namespace` (default `rightkit`); GenRight's namespace is `genright`, so they are `~/genright`, `/root/genright`, `genright` and `genright-<id>`, exactly the values its copy used.
- Changed: the comment line of the remote lease script is `# <namespace> remote lease: ...` when the host namespace is not the default (`# genright remote lease: self-destruct after {secs}s` for GenRight); with the default namespace it stays `# RightKit remote-compute lease: ...`.
- Kept: the extra `clock_overridden` field of the enforcement result (additive, always `false`).

## 0.2.0

- Removed (breaking): `models` (the local model catalogue, hardware fit, governor, installs, Blender/sd.cpp/Wan2GP doctors and the local `studio.models.*` commands). It is the new crate `rightkit-media-gen-models`; the crate no longer depends on `rightkit-local-gen` or `rightkit-models`. Its copy of `rightkit-local-gen`'s profile catalogue (`data/local-image-profiles.json`) is deleted; the new crate reads `rightkit_local_gen::profiles::CATALOGUE_JSON`.
- Added `commands`: the rented-worker commands (`studio.models.workers`, `rental.search`, `worker.action`, `rentals`, `rentals_enforce`, same names and wire), with the worker actions that were in `models`.
- Added `WorkerStore` (load and save the worker records), `SettingsWorkerStore` (the default: the media-gen settings table under `WORKERS_KEY`, as before) and `set_worker_store` for a host that keeps workers elsewhere. `load_workers`/`save_workers` go through it.
- Removed (breaking): `get_json` and `set_json`; use `rightkit_media_gen::store::setting_json` and `set_setting_json`.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation (docs/audits/2026-11-11-shape-matrix/reports/genright-redraw.md).

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-studio` (genright).
- `models`: the local model catalogue and `studio.models.*` commands, hardware fit, the memory-aware governor for heavy local models (`register_runtime`, `LoadedRuntime`), installs through `rightkit-models` (sd.cpp profiles, QTS), the Blender and Wan2GP doctors and the rented-worker actions, moved from `genright-studio` `model_management/{mod,governor,hardware,install,sdcpp,wan2gp,blender}.rs`; the local-image profile list ships as `data/local-image-profiles.json`. The mapping named `rightkit-models` (EXTEND) for these; they depend on `rightkit-media-gen` and `rightkit-local-gen`, so they live here instead of in the lower-level store crate.
