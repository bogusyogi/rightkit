# rightkit-captions 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-captions =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

- Cues built from word timings for a platform preset keep within its line, character and duration limits; a pause starts a new cue.
- Cue ids are stable for the same input; SRT export numbers every cue.

See the header of `src/main.rs` for what it deliberately does not prove.
