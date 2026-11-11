# rightkit-media-gen-models 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-media-gen-models =0.1.0` exactly with its default features, beside `rightkit-media-gen =0.2.0` (the crate runs against its `Context`). It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `COMMANDS` holds the local model commands (`studio.models.catalog`, `install`, `governor.status`, `route.get`, ...) and none of the rented-worker commands, which `rightkit-remote-compute` answers.
- `handle` answers `studio.models.route.get` against a `Context` in a per-process temp folder: no route is set, and the valid routes are `auto`, `local`, `api` and `rented`. The folder is removed afterwards.
- `handle` refuses a command it does not own with `unknown_command`.

## What it deliberately does not prove

Anything that needs external assets (model files, provider keys, network, ffmpeg or a desktop session), and installs, which download.
