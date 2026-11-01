# rightkit-device-link-types 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-device-link-types =0.1.0` exactly. It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check.

## What it proves

That the published crate exposes the device reference, effect classes, principal and registry record types, parses an older record with the newer fields defaulted, and keeps the wire ids.
