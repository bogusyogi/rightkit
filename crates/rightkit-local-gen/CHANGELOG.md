# Changelog

## 0.1.3

- Added `roots::configure_process_defaults(ProcessDefaults { known_workspace, workspace_folder_name })`: set once at startup, it changes what `EnvValues::from_process()` uses besides the environment, for every caller in the process (`service_host::Host`, `rightkit-media-gen`'s local image provider, `rightkit-media-pipeline`, `rightkit-media-gen-models`). Without a call the defaults stay no known workspace and `<home>/RightKit`. A host that shipped another default workspace (`<home>/<App>`, or a fixed workspace folder) passes it here and keeps its users' folders.


## 0.1.2

- `profiles::CATALOGUE_JSON` is public: the bundled profile catalogue as shipped. `rightkit-media-gen-models` reads it instead of keeping a copy (`rightkit-remote-compute` 0.1.0 shipped one as `data/local-image-profiles.json`). Additive. Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation (docs/audits/2026-11-11-shape-matrix/reports/genright-redraw.md).

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `genright-local-image` (genright).
- `service_host`: the `local_image.*` named commands over one lazily started `LocalImageService` per host (`Host::dispatch`, `Host::warm`, `Host::shutdown`), with `HostError` codes taken from the service's `snake_case:` message prefix. Moved from `genright-engine` `local_image_service.rs`.
- `probe`: the debug-build-only machine-lock probe (`<engine> local-model acquire|status|identity|sleep|spawn-orphan|tied-owner|memory-launch`), now taking the data folder from the caller. Moved from `genright-engine` `probe.rs`.
