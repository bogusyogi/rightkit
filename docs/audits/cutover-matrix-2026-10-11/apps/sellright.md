# SellRight cutover review

Source head: `8774314134ade8f6197e9791d112382ea13078e2`. Reviewed Node API, admin/storefront browsers, Docker appliance CI, iOS admin/widget source & current RightKit owner APIs. Source-only; no builds, installs, network, or execution.

97 catalog IDs: **2 Y, 4 N, 0 M, 91 NA**.

- **Y — @rightkit/legal:** checkout validates store-configured manifests with `assertLegalManifest` before receipt construction ([manifests.ts](/Volumes/D/claude/sellright/packages/api/src/legal/manifests.ts:19), [legal owner](/Volumes/D/claude/rightkit/packages/legal/src/index.ts:102)).
- **Y — RightKitDiagnostics:** iOS admin declares `RightKitDiagnostics` & records through `JSONLDiagnosticStore` ([project.yml](/Volumes/D/claude/sellright/ios/project.yml:27), [Diagnostics.swift](/Volumes/D/claude/sellright/ios/SellRightAdmin/Core/Diagnostics.swift:7)).
- **N — @rightkit/qa:** storefront uses Playwright traces, screenshots, HTML report & failure artifact upload; no shared evidence consumer. `QaBrowserSession` plus hash-bound evidence APIs fit a staged evidence export adoption ([playwright config](/Volumes/D/claude/sellright/packages/storefront/playwright.config.ts:20), [CI export](/Volumes/D/claude/sellright/.github/workflows/ci.yml:684), [owner evidence API](/Volumes/D/claude/rightkit/packages/qa/src/evidence.ts:73)).
- **N — @rightkit/update:** direct `pnpm up` scripts/workflow remain active. Owner supports pnpm & `project.yml` sources, then check/apply/gate/land stages ([scripts](/Volumes/D/claude/sellright/package.json:25), [workflow](/Volumes/D/claude/sellright/.github/workflows/refresh-dependencies.yml:36), [owner source types](/Volumes/D/claude/rightkit/packages/update/src/types.ts:53)).
- **N — RightKitAppleCredentials:** local Security Keychain wrapper persists admin token; owner has per-app `AppleCredentialStore` ([Keychain.swift](/Volumes/D/claude/sellright/ios/SellRightAdmin/Core/Keychain.swift:6), [owner](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:77)).
- **N — RightKitLiveActivity:** iOS directly updates/ends order activities. Owner covers app-side fenced lifecycle, while SellRight retains product push-to-start token registration ([controller](/Volumes/D/claude/sellright/ios/SellRightAdmin/Core/OrderActivityController.swift:15), [owner](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:34)).

`@rightkit/release` is **NA**, not N. SellRight’s active contract is Linux OCI build/push to GHCR plus keyless cosign by image digest ([CI](/Volumes/D/claude/sellright/.github/workflows/ci.yml:762)); current owner accepts only `win|mac` targets & GitHub Release uploads ([build](/Volumes/D/claude/rightkit/packages/release/release.mjs:134), [upload](/Volumes/D/claude/rightkit/packages/release/upload-release.mjs:35)). Future reuse needs sealed OCI/Linux, GHCR digest, cosign, manifest-hash & appliance qualification contracts.

Remaining Swift rows are NA only after iOS review: no matching declared RightKitSwift product or production caller. Cargo rows are NA: no Rust target or bridge. RightKitSwift currently uses `from: 0.1.0`, not an exact tag pin ([project.yml](/Volumes/D/claude/sellright/ios/project.yml:17)).

