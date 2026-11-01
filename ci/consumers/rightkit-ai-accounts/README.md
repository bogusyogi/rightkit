# rightkit-ai-accounts 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-ai-accounts =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate validates account names, labels second profiles, and refuses account storage when the host app keeps no state folder. It touches no Claude data.
