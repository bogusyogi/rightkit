# rightkit-library 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-library =0.1.0` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `artifacts::classify` maps image, video and audio extensions.
- `artifacts::infer_brand_code_with` matches segment sequences and prefixes against a catalogue, and the unregistered default is `NA`.
- `artifacts::index_roots` indexes a temporary folder holding one PNG and reports it.

## What it deliberately does not prove

Anything that needs external assets (model files, provider keys, network, ffmpeg or a desktop session). The crate's own tests cover those, asset-gated.
