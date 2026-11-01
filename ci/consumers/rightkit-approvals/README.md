# rightkit-approvals 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-approvals =0.1.0` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `parent_is_app`: an unreadable parent and a shell beside the engine are refused.
- `input_hash` is stable for equal inputs and differs for different ones.
- `handle` refuses an unknown approval kind with `invalid_input`, and a registered kind resolves (the challenge then needs OS randomness and a data folder, both local).

## What it deliberately does not prove

Anything that needs external assets (model files, provider keys, network, ffmpeg or a desktop session). The crate's own tests cover those, asset-gated.
