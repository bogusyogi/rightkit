# rightkit-engine-host 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-engine-host =0.1.0` exactly with its default features. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `CommandRegistry`: registration, word-to-command mapping with positional keys, `dispatch` refusing unknown names, `contract` and `typescript` export.
- `cli::run_with` over in-memory stdin/stdout: a command result, `--version`, and a usage failure exit code.
- `EngineRequest` wire round trip and `EngineEvent` frames.

## What it deliberately does not prove

Anything that needs external assets (model files, provider keys, network, ffmpeg or a desktop session). The crate's own tests cover those, asset-gated.
