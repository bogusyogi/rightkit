# RightKitSwift

Shared Apple-platform infrastructure for the Right Suite. Phase 7 of the RightKit shared SDK.

`RightKitLicense` is the first extracted target: it verifies the same Ed25519 entitlement tokens the
Rust and TypeScript sides verify, against the same `contracts/license-v2.json` vector, so a token
accepted on one platform cannot be rejected on another.

Not published. Publication needs both OS gates and Adrian's go.
