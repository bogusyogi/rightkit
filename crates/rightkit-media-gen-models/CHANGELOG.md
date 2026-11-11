# Changelog

## 0.1.0

- New crate: the media-generation engine's local model management, moved with `git mv` from `rightkit-remote-compute` 0.1.0 `models/` (shape review 2026-11-11: half of that crate was local, not remote). `handle`/`COMMANDS` (`studio.models.hardware`, `catalog`, `describe`, `probe`, `estimate`, `install*`, `route.get`/`route.set`, `unload`, `loaded`, `governor.*`), `governor` (`register_runtime`, `LoadedRuntime`, `admit`, `admit_model`), `hardware`, `install`, `sdcpp`, `wan2gp` and `blender`, with the same names and wire. The rented-worker commands stay in `rightkit-remote-compute` (`commands`).
- The sd.cpp profile list is read from `rightkit-local-gen` (`profiles::CATALOGUE_JSON`), not a copy; settings go through `rightkit_media_gen::store::setting_json`/`set_setting_json`, not private copies.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation (docs/audits/2026-11-11-shape-matrix/reports/genright-redraw.md).
