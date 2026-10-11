# RightSites — RightKit migration handoff

Source root: /Volumes/D/claude/rightsites

Use this audit snapshot to plan cutovers; recheck cited callers before edits because other chats may have changed source. Select verified published versions; keep source cutover, packaging & execution qualification separate.

[Full app evidence](/Volumes/D/claude/rightkit/docs/audits/cutover-matrix-2026-10-11/apps/rightsites.json)

## @rightkit/qa — N

N: active post-deploy site probes shell out to a hard-coded external CDP runner; no @rightkit/qa browser session is invoked. This is QA tooling, not production UI.

Replace external-runner invocation with a @rightkit/qa Node harness only after preserving existing probe assertions.

- Local: [QA external runner path](/Volumes/D/claude/rightsites/scripts/qa-site-probes.mjs:14) — probe entrypoint invokes qa-functional.mjs for five sites at desktop & mobile viewports.
- Owner: [page.ts](/Volumes/D/claude/rightkit/packages/qa/src/page.ts:43) — QaBrowserSession owns bounded CDP pages for Node QA callers.

Delivery: QA runner path is Windows-local (D:/Claude), not a published RightKit dependency.

## @rightkit/qa-chrome — N

N: current site probes depend on an externally installed Chrome runner, while @rightkit/qa-chrome’s managed resolver/launcher is not called.

Adopt @rightkit/qa-chrome only with an explicit browser-download & host policy; then wire probes to its resolved executable.

- Local: [probe runner declaration](/Volumes/D/claude/rightsites/scripts/qa-site-probes.mjs:8) — script declares installed Chrome via an external CDP runner.
- Owner: [install.ts](/Volumes/D/claude/rightkit/packages/qa-chrome/src/install.ts:32) — managedChromeForTestingRoot resolves suite-wide Chrome-for-Testing location.

Delivery: Current QA runner is an absolute Windows path.

## @rightkit/theme — N

N: React admin actively persists a light/dark/system choice, applies data-theme, & follows OS changes through its own theme hook; no @rightkit/theme/app-shell theme store is invoked.

Map admin theme IDs & CSS tokens to RightKit theme primitives without changing store-specific palette policy.

- Local: [applyTheme](/Volumes/D/claude/rightsites/packages/admin/src/theme.tsx:23) — sets data-theme & dark class on document root.
- Local: [useTheme](/Volumes/D/claude/rightsites/packages/admin/src/theme.tsx:35) — persists theme/mode in localStorage & subscribes to matchMedia changes.
- Owner: [theme.ts](/Volumes/D/claude/rightkit/packages/app-shell/src/theme.ts:82) — createThemeStore persists choice, applies data-theme, & owns system-theme listener.

## @rightkit/update — N

N: workspace supplies direct pnpm dependency-audit/check/update scripts; no manifest-driven right-update check/apply/gate/land invocation is present.

Create an explicit RightKit update manifest & map existing dependency gates before replacing direct pnpm scripts.

- Local: [deps:check/deps:update scripts](/Volumes/D/claude/rightsites/package.json:16) — root scripts directly run pnpm outdated & pnpm up across workspace packages.
- Owner: [index.ts](/Volumes/D/claude/rightkit/packages/update/src/index.ts:6) — exports runCheck/runCli plus apply, gate, land & schedule mechanisms.

## RightKitLiveActivity — N

N: maintained app & widget extension implement active order ActivityKit contract, push-to-start token observation, update/end behavior & presentation locally; RightKitLiveActivity lifecycle host is not linked or called.

Add RightKitLiveActivity to app target, retain order schema + widget presentation, map local phases/push token policy to shared lifecycle host, then remove overlapping controller guards.

- Local: [OrderActivityController.update](/Volumes/D/claude/rightsites/ios/SellRightAdmin/Core/OrderActivityController.swift:15) — Local app controller finds, updates & ends active order ActivityKit activities.
- Local: [observePushToStartToken](/Volumes/D/claude/rightsites/ios/SellRightAdmin/Core/PushManager.swift:111) — Local manager streams ActivityKit push-to-start tokens for server registration.
- Local: [OrderLiveActivity](/Volumes/D/claude/rightsites/ios/SellRightWidgets/OrderLiveActivity.swift:11) — Maintained widget extension presents Lock Screen & Dynamic Island activity UI.
- Local: [cancelOrder/FulfillSheet.submit](/Volumes/D/claude/rightsites/ios/SellRightAdmin/Features/Orders/OrderDetailView.swift:191) — Live order actions invoke local controller update after API state transition.
- Owner: [LiveActivityHost.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/LiveActivityHost.swift:25) — Shared host owns fenced ActivityKit start/adopt/update/end lifecycle for an app-defined activity type.
- Owner: [ActivityLifecycle.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLiveActivity/ActivityLifecycle.swift:27) — Shared lifecycle guard supplies transition/terminal/session safety.

Delivery: RightKitSwift project dependency lists RightKitDiagnostics only; extension target has no RightKitSwift product dependency.

## RightKitAppleCredentials — N

N: SellRightAdmin actively stores, reads & deletes admin bearer token through local Security Keychain wrapper; RightKitAppleCredentials offers same per-app credential storage but is neither linked nor called.

Add RightKitAppleCredentials product, map service/account/device-only scope, then remove local Keychain wrapper after auth-session tests.

- Local: [Keychain](/Volumes/D/claude/rightsites/ios/SellRightAdmin/Core/Keychain.swift:6) — Local wrapper performs SecItemDelete/Add/CopyMatching for app admin-token service.
- Local: [bootstrap/login/logout](/Volumes/D/claude/rightsites/ios/SellRightAdmin/App/AppSession.swift:41) — Live session reads local credential at launch, writes after login & deletes on logout.
- Owner: [RightKitAppleCredentials.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitAppleCredentials/RightKitAppleCredentials.swift:77) — AppleCredentialStore provides scoped per-app Keychain read/write/delete API.

Delivery: RightKitSwift package currently exposes only RightKitDiagnostics product in project.yml.

Before marking a task complete: migrate matching callers, remove only proven duplicate mechanisms, preserve product-specific schemas/policy/UX, then run affected target checks. Update this inventory from fresh evidence.
