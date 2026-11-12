# rightkit-media-fetch 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-media-fetch =0.2.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate classifies media URLs and local files without network access, builds the yt-dlp argv from overrides, and plans a transcribe job as needing a WAV. It also round-trips a job kind through its stored tag, and replaces a file in a per-process temp folder through `extraction::publish_replacing`, which 0.2.0 re-exports from `rightkit-fs` (removed afterwards).

It needs no external tools, network or model assets.
