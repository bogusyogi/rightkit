# SellRight — RightKit migration handoff

Source root: /Volumes/D/claude/sellright

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/sellright.json)

## @rightkit/qa — N

N: storefront production e2e uses Playwright HTML reports plus failure-only traces/screenshots uploaded by GitHub Actions; it never calls @rightkit/qa. Current owner APIs can attach to CDP browser sessions & create hash-bound evidence bundles, so a source-compatible evidence cutover exists.

Add an @rightkit/qa consumer that records Playwright HTML report, trace & screenshot bytes with createEvidenceBundle/addEvidenceArtifact/addEvidenceCheck/writeEvidenceBundle; retain commerce-specific Playwright scenarios.

- Local: [defineConfig](/Volumes/D/claude/sellright/packages/storefront/playwright.config.ts:20) — Production-build E2E config selects Chromium, retain-on-failure trace & screenshot, plus CI HTML reporter.
- Local: [Run the storefront e2e suite](/Volumes/D/claude/sellright/.github/workflows/ci.yml:684) — Active CI E2E job runs storefront Playwright suite then uploads playwright-report/ & test-results/ on failure.
- Owner: [QaBrowserSession.connect](/Volumes/D/claude/rightkit/packages/qa/src/page.ts:42) — QaBrowserSession connects to a supplied CDP endpoint & owns CDP-created pages.
- Owner: [addEvidenceArtifact](/Volumes/D/claude/rightkit/packages/qa/src/evidence.ts:73) — addEvidenceArtifact records SHA-256 & size of browser artifacts before a check may reference them.
- Owner: [addEvidenceCheck](/Volumes/D/claude/rightkit/packages/qa/src/evidence.ts:101) — addEvidenceCheck rejects artifact references that were not hash-recorded.

## @rightkit/update — N

N: root scripts & scheduled workflow directly run pnpm up for root/admin workdirs. @rightkit/update currently supports pnpm manifest sources, project.yml SwiftPM sources, explicit gates, staged apply/discard & gated land, but SellRight has no .rightkit/update.json or shared invocation.

Create .rightkit/update.json with root/admin pnpm manifests, ios/project.yml, held-policy rules & SellRight verification gates; replace direct pnpm up workflow/scripts with right-update check/apply/gate/land.

- Local: [deps scripts](/Volumes/D/claude/sellright/package.json:25) — Root exposes direct outdated/audit/up scripts; deps:update & deps:update:latest call pnpm up for workspace plus admin.
- Local: [refresh job](/Volumes/D/claude/sellright/.github/workflows/refresh-dependencies.yml:36) — Manually dispatched workflow runs pnpm up in root & packages/admin, then verifies & commits.
- Owner: [PnpmSource/SwiftpmSource](/Volumes/D/claude/rightkit/packages/update/src/types.ts:53) — PnpmSource accepts a package.json manifest/glob; SwiftpmSource accepts Package.swift or project.yml.
- Owner: [USAGE](/Volumes/D/claude/rightkit/packages/update/src/run.ts:30) — CLI exposes check, apply, gate, land, discard & staged run commands.
- Owner: [command semantics](/Volumes/D/claude/rightkit/packages/update/src/run.ts:42) — Documented stages compare pins, apply non-breaking rewrites, run gates, then commit/push per policy.

## RightKitLiveActivity — N

N: iOS admin directly enumerates, updates & ends order ActivityKit activities. RightKitLiveActivity supplies typed fenced start/adopt/update/end plus generic lifecycle guards, but project.yml does not declare it. SellRight push-to-start token registration remains product-specific because owner host starts activities with pushType nil.

Declare RightKitLiveActivity; route local app-side update/end lifecycle through its host while retaining order attributes/widget UI & SellRight push-to-start token registration.

- Local: [OrderActivityController.update](/Volumes/D/claude/sellright/ios/SellRightAdmin/Core/OrderActivityController.swift:15) — Local controller finds order activity then updates or ends it.
- Local: [observePushToStartToken](/Volumes/D/claude/sellright/ios/SellRightAdmin/Core/PushManager.swift:111) — Local manager observes push-to-start token updates & ends all activities on logout.
- Owner: [LiveActivityHost](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:34) — LiveActivityHost owns fenced ActivityKit lifecycle sequencing.
- Owner: [LiveActivityHost.update/end](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:75) — Owner exposes update & end calls for hosted activity.
- Owner: [LiveActivityHost.start](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:65) — Owner start currently requests pushType nil, excluding SellRight push-to-start registration.

## RightKitAppleCredentials — N

N: iOS admin’s active Keychain wrapper persists, reads & deletes adminToken directly with Security APIs. Current RightKitAppleCredentials supplies AppleCredentialStore read/write/delete APIs for the same per-app Keychain role, but project.yml does not declare it.

Declare RightKitAppleCredentials, map adminToken to explicit device scope, then replace local Keychain get/set/delete callers while preserving SellRight service/account policy.

- Local: [Keychain](/Volumes/D/claude/sellright/ios/SellRightAdmin/Core/Keychain.swift:6) — Local Keychain wrapper directly deletes/adds/reads kSecClassGenericPassword items.
- Local: [AppSession](/Volumes/D/claude/sellright/ios/SellRightAdmin/App/AppSession.swift:40) — Bootstrap, login & logout call local Keychain get/set/delete for adminToken.
- Owner: [AppleCredentialStore](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:77) — AppleCredentialStore is per-app Keychain storage.
- Owner: [AppleCredentialStore API](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:102) — Owner exposes read, presence, write & deleteEverywhere operations.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
