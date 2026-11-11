# rightkit-media-pipeline 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-media-pipeline =0.2.0` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `commands()` lists the `studio.pipelines.*` and `studio.production.*` command names, and `handle` returns `None` for a name it does not own.
- `studio.pipelines.templates` and `studio.production.templates` answer from embedded data in a temporary data folder (no provider, no network).

## What it deliberately does not prove

Anything that needs external assets (model files, provider keys, network, ffmpeg or a desktop session). The crate's own tests cover those, asset-gated.
