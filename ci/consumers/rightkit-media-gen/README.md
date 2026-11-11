# rightkit-media-gen 0.1.2 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-media-gen =0.1.2` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `providers::registry()` lists the provider adapters with unique, non-empty ids (no network, no keys).
- `Context::new` and `Context::with_db_file_name` place the job store and library under the data folder.
- `host::parse_caller` slugs and refuses unsafe caller names; `credentials::service()` is the host app identifier (product-neutral default).
- `gallery::index_roots` (0.1.2, moved from `rightkit-library`) lists an image in a per-process temp folder with its project from the folder name, its caller from `<file>.caller.json` and unknown asset facts; the caller filter narrows the listing; an unknown eyes state is refused; `host::approval::eyes_file_hash` has the gate's `sha256:` plus 16 hex form. The folder is removed afterwards.

## What it deliberately does not prove

Anything that needs external assets (model files, provider keys, network, ffmpeg or a desktop session). The crate's own tests cover those, asset-gated.
