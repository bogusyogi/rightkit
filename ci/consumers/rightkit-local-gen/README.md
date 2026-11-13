# rightkit-local-gen 0.1.3 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-local-gen =0.1.2` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `host::HostConfig::default()` is product-neutral (`RIGHTKIT` prefix) and `host::env_name` builds prefixed names.
- `memory::parse_requested` accepts `auto`, `performance`, `balanced`, `low` and refuses an unknown mode; `memory::Mode::as_str` round-trips.
- `model_store::sdcpp_quant` and `model_store::sdcpp_revision` give the stable store labels.
- `roots::workspace_root`, `roots::content_root` and `roots::tools_root` resolve from a hand-built `roots::EnvValues` with no process reads.
- `profiles::CATALOGUE_JSON` (0.1.2) is the shipped profile catalogue and parses to the same profiles `profiles::catalogue` returns.

## What it deliberately does not prove

Anything that needs external assets (model files, provider keys, network, ffmpeg or a desktop session). The crate's own tests cover those, asset-gated.
