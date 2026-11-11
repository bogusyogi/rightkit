# rightkit-library 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-library =0.2.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate opens an in-memory index, round-trips a record, and enriches text deterministically without network or model access. For the 0.2.0 shape: the host's kind tag and short code are kept as given, the domain comes from the source URL and a local file name is not one, and a kind tag the crate has never seen comes back unchanged from a history page (the crate defines no kinds).

It needs no external tools, network or model assets.
