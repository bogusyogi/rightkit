# rightkit-media-gen-models

Local model management for the media-generation engine: hardware detection and fit, model installs
through the `rightkit-models` store (sd.cpp image profiles, the QTS voice model), the memory-aware
governor for heavy local models (`governor::register_runtime`, `LoadedRuntime`), per-capability
route choice (`auto`, `local`, `api`, `rented`) and the `studio.models.*` commands that join them
with the provider API models.

It runs against a `rightkit_media_gen::Context`: install records, governor grants and routes live in
that engine's settings table. A host dispatches `rightkit_media_gen_models::handle` for
`COMMANDS`, and `rightkit_remote_compute::commands::handle` for the rented-worker commands
(`studio.models.workers`, `rental.search`, `worker.action`, `rentals`, `rentals_enforce`). Register a
function that routes `studio.models.*` to the two crates with
`rightkit_media_gen::avatars::set_models_handler`, so avatar routes see local and rented engines
(`studio.models.catalog` here, `studio.models.workers` there).

Moved into RightKit from `genright-studio` (`model_management/{mod,governor,hardware,install,sdcpp,wan2gp,blender}.rs`)
on 2026-11-01, and out of `rightkit-remote-compute` on 2026-11-11.
