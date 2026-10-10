# Apple core product source review — 2026-10-10

## Scope & method

Fresh read-only review of all source & test bodies in eight `RightKitSwift` products: `RightKitLicense`, `RightKitDiagnostics`, `RightKitArtifacts`, `RightKitPurchases`, `RightKitCloudSync`, `RightKitStore`, `RightKitImportQueue`, & `RightKitAppleCredentials`. Review covered public API boundaries, iOS 17/macOS 14 availability, actor isolation, task lifetime/cancellation, persistence/cross-process behavior, secure storage, dependency use, decomposition, duplication, & test evidence. `Package.swift` declares iOS 17/macOS 14 at [Package.swift:9](/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:9); StoreKit, CloudKit, Security, & UIKit use availability/conditional-import guards where relevant.

This section records source analysis. Executed native checks are consolidated in [Apple verification](/Volumes/D/claude/rightkit/docs/audits/fresh-2026-10-10/evidence/apple-verification.md); source findings remain distinct from reproduced runtime failures.

## Findings

### P2 — file-share intake can bypass configured byte cap

**Source-confirmed.** [`ShareItemExtractor.stageExternalFile`](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/ShareItemExtractor.swift:118) rejects an input only when `fileSizeKey` is present & already exceeds `maxFileBytes` ([lines 121–125](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/ShareItemExtractor.swift:121)). It then calls `copyItem` with no streamed count or post-copy cap ([lines 126–132](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/ShareItemExtractor.swift:126)).

**Caller & failure.** `payload(for:)` reaches this path for both provider file representations & file-URL fallback ([lines 74–88](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitImportQueue/ShareItemExtractor.swift:74)). A provider with an absent, stale, or changed size can stage an arbitrarily large file into app-group/temporary storage, exhausting extension or device storage despite caller-configured cap.

**Repair.** Stream/copy into a unique temporary file while counting bytes; stop above cap, remove partial output, then atomically publish. Preserve security-scoped access through entire copy.

**Test gap.** [`extractorStagesFilesWithinCapAndRefusesOversizedOnes`](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitImportQueueTests/ImportQueueTests.swift:49) covers regular files whose `fileSizeKey` is known. Add unknown-size & source-growth fixtures asserting no oversized staged file survives.

### P2 — concurrent diagnostics writers can lose records

**Source-confirmed.** `JSONLDiagnosticStore` is `Sendable` ([JSONLDiagnosticStore.swift:21](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:21)) but performs directory creation, trim, `seekToEnd`, & write without serialization ([lines 53–66](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:53)).

**Caller & failure.** Any concurrently used copies targeting one `fileURL` can both seek same end offset before either write; later write replaces earlier bytes. A simultaneous `trimIfNeeded` atomic replacement ([lines 87–98](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitDiagnostics/JSONLDiagnosticStore.swift:87)) can also orphan another writer's append on replaced inode. Observable result: missing diagnostic events or malformed/torn log around capacity trimming.

**Repair.** Serialize full read/trim/append transaction per canonical file path; use an app-group-compatible file lock if callers share path across processes. Keep write/trim failure visible through existing `false` return.

**Test gap.** Tests cover sequential stability, trim, & torn trailing write ([DiagnosticsTests.swift:42](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitDiagnosticsTests/DiagnosticsTests.swift:42)); add concurrent same-path writers, including forced trim, & assert all committed JSON lines remain readable.

### P3 — CloudSync subscription task can outlive owner through bounded retry lifetime

**Source-confirmed finite-lifetime concern.** `CloudSyncEngine` retains `subscriptionTask` ([CloudSyncEngine.swift:90](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudSyncEngine.swift:90)); task invokes `await self?.runSubscription(...)` ([line 714](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudSyncEngine.swift:714)). That async call retains actor throughout bounded retry sleeps/network calls ([lines 717–742](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudSyncEngine.swift:717)), while no `deinit` cancels task.

**Caller & failure.** `prepareZoneIfNeeded()` starts subscription after a sync pass ([CloudSyncEngine.swift:680](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitCloudSync/CloudSyncEngine.swift:680)). Releasing last app owner during transient subscription failure can leave engine, adapter, transport, & retries alive until retry loop ends; background CloudKit work outlives feature lifecycle. Source establishes bounded retention, not an unbounded persistent leak.

**Repair.** Add `deinit { subscriptionTask?.cancel() }` & avoid long-lived strong actor capture across awaited subscription work, or expose explicit shutdown called by composition root. Ensure task clears state on cancellation.

**Test gap.** Subscription retry test proves bounded retry ([CloudSyncEngineTests.swift:158](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitCloudSyncTests/CloudSyncEngineTests.swift:158)), but no owner-release/cancellation assertion exists.

### Conditional API risk — shared license-removal state has no cross-process transaction

**Hypothesis; no inspected caller proves shared storage.** Default `UserDefaultsLicenseDeviceState` uses bundle-default defaults ([LicenseRemovalQueue.swift:18](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseRemovalQueue.swift:18)), which alone does not establish app/extension sharing. However, public `LicenseDeviceStateStoring` supports caller-supplied shared persistence; `PendingLicenseRemovalQueue.enqueue` does unsynchronized `load` → append → `set` ([lines 84–90](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseRemovalQueue.swift:84)). Its actor & `RemovalDeliveryLock` serialize only process-local work ([lines 65–73](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseRemovalQueue.swift:65), [158–185](/Volumes/D/claude/rightkit/swift/RightKitSwift/Sources/RightKitLicense/LicenseRemovalQueue.swift:158)).

**Schedule.** Two processes read `[A]`; each appends a distinct removal; last storage write wins, dropping one server-seat removal. Resolve consumer storage before raising priority; if app-group defaults/keychain state is intentionally shared, provide lock/CAS-backed storage & cross-process test.

**Test gap.** Existing test covers two queues over `InMemoryLicenseDeviceState` in one process ([LicenseClientTests.swift:1460](/Volumes/D/claude/rightkit/swift/RightKitSwift/Tests/RightKitLicenseTests/LicenseClientTests.swift:1460)), not separate processes or shared durable state.

## Per-product verdict & coverage

| Product | Verdict | Source/test coverage |
| --- | --- | --- |
| RightKitLicense | Conditional persistence limitation above; otherwise strong serialization & verifier review. | Read `LicenseClient.swift`, `LicenseRemovalQueue.swift`, `LicenseToken.swift`; tests cover verifier contracts, Keychain remove/load race, serialized controller operations, & single-flight retry. Gap: shared-storage/process interleaving. |
| RightKitDiagnostics | P2 concurrent-write loss. | Read JSONL store & redactor; tests cover redaction, stable lines, trimming, torn-tail recovery. Gap: concurrent writers/trim. |
| RightKitArtifacts | No source-confirmed finding. | Read tar/gzip verifier, staged activation/rollback & all artifact tests. Traversal, symlink, checksum, inflated size, truncation, rollback covered. Gap: simultaneous installs to one destination root. |
| RightKitPurchases | No source-confirmed finding. | Read ledger, controller, StoreKit backend, storage, models, access resolver & tests. Main-actor controller owns UI mutation; StoreKit backend actor owns pending transactions; iOS-only scene paths/17.2 offer fields are guarded. Unit coverage spans entitlement, refresh-hint, token inheritance, outcomes, restore/refund; StoreKit suite covers simulator flow. Gap: real macOS StoreKit catalog behavior & controller teardown while update stream is active. |
| RightKitCloudSync | P3 bounded subscription lifetime concern. | Read engine, coordinator, triggers, transport, state store, models, adapter, retry. Actor owns engine/coordinator mutable state; account fencing, retry, token replay, failures, push watchdog, & state recovery have broad unit coverage. Gap: engine release during subscription retry; multi-process same-file state store. |
| RightKitStore | No source-confirmed finding. | Read cross-process lock, command queue, owner lease, revisioned store. File-lock fail-closed behavior, real separate lock-holder process, owner epoch checks, queue leasing, & persistence tests exist. Gap: crash between multi-key acknowledgement writes is designed toward at-least-once semantics but lacks crash-process validation. |
| RightKitImportQueue | P2 staging-cap bypass. | Read payload/configuration/extractor/durable queue. Tests cover queue restart, migration, corruption, atomic write fault seams, basic staging & known-size reject. Gap: unknown/stale file size & capped streaming. |
| RightKitAppleCredentials | No source-confirmed finding. | Read store, in-memory backend, Security backend; scoped sync/device identities, passive probes, status propagation, broad deletion, & macOS legacy-Keychain query construction covered. Gap: native entitlement-provisioned iOS/macOS integration coverage remains unrun. |

## Architecture & dependency review

Package remains decomposed by product, with dependencies limited to Apple SDK/Foundation/CryptoKit plus internal `RightKitPurchases`→`RightKitLicense` and `RightKitImportQueue`→`RightKitStore` edges declared in [Package.swift](/Volumes/D/claude/rightkit/swift/RightKitSwift/Package.swift:9). No third-party package dependency appears. `@MainActor` owns UI-facing controllers; actors own CloudSync, StoreKit backend, & removal waves. Existing App Group primitives should be reused for diagnostics or any intentionally cross-process state rather than adding parallel locking implementations.
