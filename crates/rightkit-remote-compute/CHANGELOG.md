# Changelog

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
