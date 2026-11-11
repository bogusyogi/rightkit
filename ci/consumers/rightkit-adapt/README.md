# rightkit-adapt 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-adapt =0.2.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate (Pulse's governed behavioural learning, which was `rightkit-adapt-taste` until 0.1.2) canonicalizes JSON and derives stable preference ids with the published prefix and sha256 digests. CodeRight's learning crate, published as `rightkit-adapt` until 0.1.2, has its own consumer under `rightkit-agent-learning`.
