# rightkit-agent-transcript 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-agent-transcript =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published normalizer pins its parser version, detects a Codex transcript from its first lines, and refuses a missing file.
