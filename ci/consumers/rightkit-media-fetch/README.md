# rightkit-media-fetch 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-media-fetch =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate classifies media URLs and local files without network access, builds the yt-dlp argv from overrides, and plans a transcribe job as needing a WAV.

It needs no external tools, network or model assets.
