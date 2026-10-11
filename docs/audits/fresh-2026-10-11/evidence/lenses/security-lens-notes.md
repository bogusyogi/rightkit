# reasoning.security review

Scope was bounded to packet `reasoning.security`, plan digest `sha256:9b12c99c2ec21897dd8297a438f3fb1c1a2f405593c6db0fa4e9b89a6c6b5042`, & actual release, trust, runtime, control, secrets, update, HTTP, MCP, ownership, & license sources. No prior audit evidence was used.

No confirmed reachable security finding was established. `complete` remains `false` because packet denominator is 1,996 paths, only 29 generated cache headers were examined, & 1,967 paths were omitted under bounded excerpt coverage; whole-repo coverage is therefore incomplete.

Evidence reviewed:

- `crates/rightkit-control/src/webdriver.rs:167-200,263-300` binds control HTTP to loopback, requires exact Host/origin policy, & constant-time bearer authentication; effect operations pass through admission gates.
- `crates/rightkit-mcp/src/server.rs:1030-1066,1156-1185,1224-1339` requires loopback binding, bearer authentication, loopback Host/origin policy, bounded bodies, method/session checks, & admission before tool execution.
- `packages/update/src/download.ts:36-100,109-131` enforces HTTPS outside loopback, bounded redirects/downloads, digest verification, atomic partial files, & extraction containment.
- `packages/update/src/reports.ts:139-186` rejects unsafe paths & validates candidate roots, assets, digests, & binaries before apply.
- `packages/release/rightkit-signing-agent.mjs` validates private signing roots, request shape, real paths, targets, identities, options, bounded execution, & cleanup.
- `crates/rightkit-ownership/src/trust.rs:208-374` checks protected trust paths, ownership/modes, symlinks, schema, key identity, & enrollment.
- `crates/rightkit-http/src/redirect.rs` & `async_client.rs` constrain redirect origins, strip credential-like headers cross-origin, disable automatic redirects, & vet resolved addresses.
- `swift/RightKitSwift/Sources/RightKitLicense/LicenseToken.swift:105-162,735-770` verifies signatures before claims, then enforces app, device, expiry, & active-state checks.

No product edits, tests, builds, network, installs, or ingest were run.
