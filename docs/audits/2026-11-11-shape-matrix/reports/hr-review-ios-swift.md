# HeardRight iOS and Mac pill versus RightKitSwift

Read-only comparison, 2026-11-11. Nothing was built or run.

- LIVE = `survey/heardright-live` (commit 12b7d8ce, 30 Sep). MAIN = `survey/heardright-main`.
- RK = `/home/user/rightkit/swift/RightKitSwift` (`VERSION` 0.9.1; `CHANGELOG.md:3` says 0.9.1 is unreleased and not tagged; `packages/release/rightkit-versions.json:715-719` records 0.8.1 as the staged Swift version).
- HeardRight paths are relative to `ios-native/` unless they start with `tauri-app-next/`. RK paths are relative to `Sources/`.
- Line counts are from the inventory JSON in each tree (LIVE / MAIN). A dash means the file does not exist in that tree.
- Each claim is marked **[read]** (I read the code on both sides), **[grep]** (declarations and call sites only, bodies not read), **[suspicion]**, or **not verified**.

## Headline

1. LIVE uses one RightKit Swift product (`RightKitUI`, two call sites). Everything else in LIVE is HeardRight's own code. **[read]**
2. MAIN already declares and imports 13 RightKitSwift products on iOS and one on the Mac pill. The line-match found nothing because RightKit's Swift is rewritten code, not copied code. **[read]**
3. MAIN's adoption is partial. For at least ten areas RightKit already has the owner type and HeardRight MAIN still keeps its own copy (about 6,700 lines, listed in section 5, step 2).
4. Of the six missing Swift packages, HeardRight needs only two: `RightKitDictation` and `RightKitLibrary`. `RightKitAgent`, `RightKitNotch`, `RightKitSystemUI` and `RightKitMedia` have no HeardRight code mapped to them.
5. Two planned targets have the wrong meaning. `RightKitWatch` is a macOS FSEvents file watcher, not Apple Watch code. `RightKitHubQA` is a set of fakes for the desktop Hub, not an iOS test home.
6. The Mac pill uses none of RightKit's Swift `RightKitOverlay`. That module is a panel and click-through layer with no pill content, no animation, no C bridge and no splash.

## 1. What HeardRight depends on today

### LIVE (the released version)

| Where | Evidence | Status |
|---|---|---|
| iOS package pin | `project.yml:16-19`: `https://github.com/bogusyogi/rightkit-swift.git`, `exactVersion: "0.1.1"` | [read] |
| iOS products | `project.yml:127-128`: `RightKitUI` only, on the `HeardRight` app target | [read] |
| iOS imports | `import RightKitUI` in `HeardRight/RecordingPillView.swift:2` and `HeardRight/ContentViewHomeControls.swift:2` | [read] |
| Mac pill | `tauri-app-next/macos-native/HeardRightPillMac/Package.swift`: no `dependencies:` block at all; platform `.macOS(.v13)` | [read] |

The LIVE pin points at the private mirror that RightKit records as retired (`rightkit-versions.json:709-713`).

### MAIN

| Where | Evidence | Status |
|---|---|---|
| iOS package pin | `project.yml:16-19`: `https://github.com/Orthic-Labs/rightkit`, `exactVersion: "0.8.1"` | [read] |
| `HeardRight` app | `project.yml:133-158`: Diagnostics, UI, Purchases, CloudSync, Store, ImportQueue, LiveActivity, WatchBridge, Speech, License, Intents, AppleCredentials, Credentials (13) | [read] |
| `HeardRightWidgets` | `project.yml:201-216`: Diagnostics, Store, ImportQueue, WatchBridge, LiveActivity, Intents, License, AppleCredentials | [read] |
| `HeardRightWatch` | `project.yml:261-266`: Store, WatchBridge, Intents | [read] |
| `HeardRightWatchWidgets` | `project.yml:297-300`: Store, Intents | [read] |
| `HeardRightShare` | `project.yml:335-338`: ImportQueue, Store | [read] |
| Test helper packages | `Tests/RightKitSmokeDeps/Package.swift` and `Tests/RightKitSpeechE2E/Package.swift`, both `exact: "0.8.1"` | [read] |
| Mac pill | `HeardRightPillMac/Package.swift:27`: `https://github.com/Orthic-Labs/rightkit.git`, `exact: "0.9.0"`; `:67` product `RightKitAppleCredentials` on the `HeardRightAppleCredentials` helper only; platform `.macOS(.v14)` | [read] |
| Mac CloudKit helper | `tauri-app-next/right-release.config.mjs:235` builds RightKit's `rightkit-cloudkit-helper` product from the pill package's pin | [read] |

Imports in MAIN app code (file count per module): Store 6, Speech 6, WatchBridge 4, CloudSync 4, License 3, ImportQueue 3, UI 2, Credentials 2, AppleCredentials 2 (one iOS, one Mac helper), Diagnostics 1, Intents 1, LiveActivity 1, Purchases 1. All 13 declared products are imported. **[read]**

Not declared anywhere in MAIN: `RightKitAudio`, `RightKitAccount`, `RightKitOverlay`, `RightKitWatch`, `RightKitHubQA`, `RightKitArtifacts` (reached only through `RightKitSpeech`), `RightKitHTTP`, `RightKitLLM`, `RightKitUpdate`. The pill library target `HeardRightPillMac` has no RightKit dependency. **[read]**

One repository carries two Swift pins: 0.8.1 for iOS and 0.9.0 for the pill. The pill's `Package.swift:26` says "ios-native/project.yml moves separately". Several owner types HeardRight iOS needs arrived in 0.9.0 (`RightKitAudio`, the Core ML validator surface, the model download owner; `CHANGELOG.md:34-73`), so iOS cannot use them until its pin moves.

## 2. Planned Swift targets, one by one

"Uses" refers to MAIN. LIVE uses its own code for everything except the two `RightKitUI` call sites.

### RightKitDictation (planned NEW; does not exist)

RK has no such module. `RightKitSpeech/Dictation.swift:5` is a 40-line enum of composer draft rules from CodeRight and is unrelated. **[read]**

HeardRight files (25 files / 8,516 lines LIVE; 30 files / 10,514 lines MAIN):

| File | LIVE | MAIN |
|---|---:|---:|
| HeardRight/RecordingAudioSession.swift | 1633 | 1829 |
| HeardRight/RecordingSystemSurface.swift | 697 | 1192 |
| HeardRight/RecordingAudioPolicy.swift | 1074 | 1074 |
| HeardRight/WarmAudioCapture.swift | 596 | 772 |
| HeardRight/DictationShortcuts.swift | 595 | 595 |
| HeardRight/HardwareRecordingControls.swift | - | 492 |
| HeardRight/RecordingLiveActivityController.swift | 481 | 447 |
| HeardRight/SharedRecordingTranscriber.swift | 327 | 427 |
| HeardRight/DictationMicrophone.swift | 390 | 398 |
| HeardRight/SharedRecordingCoordinator.swift | 254 | 333 |
| HeardRight/LongFormRecordingLifecycle.swift | 322 | 322 |
| HeardRight/DictationRecording.swift | 285 | 310 |
| Shared/SharedDictationSnapshot.swift | 231 | 277 |
| HeardRight/RecordingPillView.swift | 188 | 225 |
| HeardRight/PendingImportProcessor.swift | - | 205 |
| HeardRight/RecordingOwnershipLease.swift | 86 | 199 |
| HeardRight/DictationController.swift | 197 | 197 |
| Shared/SharedDictationResultWaiter.swift | - | 155 |
| Shared/SharedDictationCommand.swift | 321 | 142 |
| HeardRight/RecordingCommandBridge.swift | 137 | 137 |
| HeardRight/DocumentImportCoordinator.swift | 108 | 120 |
| HeardRight/RecordingCheckpointStore.swift | 120 | 120 |
| HeardRight/DictationPresentation.swift | 111 | 111 |
| HeardRight/BackgroundTranscriptionGuard.swift | 86 | 102 |
| HeardRight/DictationSupport.swift | 97 | 97 |
| Shared/AudioCaptureRecoveryPolicy.swift | 75 | 75 |
| HeardRight/SavedAudioStore.swift | 71 | 71 |
| Shared/SharedImportQueue.swift | - | 43 |
| HeardRight/AudioImportCoordinator.swift | 34 | 30 |
| Shared/RecordingResultPreview.swift | - | 17 |

Main types: `SharedRecordingCoordinator` (`HeardRight/SharedRecordingCoordinator.swift:26`), `DictationController` (`HeardRight/DictationController.swift:49`), `RecordingAudioPolicy` (`HeardRight/RecordingAudioPolicy.swift:17`), `LongFormRecordingLifecycle` (`:24`), `RecordingCheckpointStore` (`:22`). **[grep]**

MAIN already builds parts of this on RightKit primitives: `SharedDictationCommand` conforms to `AppGroupCommand` and wraps `AppGroupCommandQueue` (`Shared/SharedDictationCommand.swift:11,45`); `SharedDictationSnapshot` conforms to `RevisionedState` and uses `AppGroupOwnerLease` and `RevisionedStateStore` (`Shared/SharedDictationSnapshot.swift:23,129,231`); `RecordingOwnershipLease` uses `AppGroupOwnerLease` (`:34-68`); `RecordingLiveActivityController` uses `LiveActivityHost` and `ActivityLifecycle` (`:15,36,53`); `SharedImportQueue` wraps `RightKitImportQueue`. **[read]**

Still duplicated inside this group:
- `HeardRight/WarmAudioCapture.swift:18` (772 lines) versus RK `RightKitAudio/WarmAudioCapture.swift:108` (546 lines). Both are a warm `AVAudioEngine` tap with a converter to 16 kHz mono, rebuild on configuration change, raw tap health and a dead-tap verdict. HeardRight's copy adds in-memory sample accumulation (`beginAccumulation` `:461`, `endAccumulation` `:488`, `samples(from:)` `:589`), a speech-activity snapshot fed by the RK voice activity detector (`:134`) and `PCMWavWriter` (`:741`). RK's hands buffers to a handler and keeps no samples. HeardRight does not link `RightKitAudio`. **[read: declarations and header comments on both sides; bodies not compared line by line]**
- `RecordingAudioSession.swift` and `RecordingAudioPolicy.swift` reference `AVAudioSession` 42 and 10 times. RK has `AudioSessionController` (`RightKitAudio/AudioSession.swift:88`) and `AudioSessionMonitor` (`AudioSessionEvents.swift:76`). Whether RK's session configuration can express HeardRight's policy ladder (mix policies, retry ladder, Bluetooth transport, warm hold; `RecordingAudioPolicy.swift:26-454`) is **not verified**. **[suspicion]** that it cannot without additions: RK's configuration is one category, one mode and an option set (`AudioSession.swift:91-121`).

### RightKitLibrary (planned NEW; does not exist)

HeardRight files (14 files / 5,353 lines LIVE; 12 files / 4,984 lines MAIN):

| File | LIVE | MAIN |
|---|---:|---:|
| HeardRight/ContentViewSettings.swift | 859 | 1025 |
| Shared/SharedTranscriptStore.swift | 645 | 919 |
| HeardRight/ContentViewHistory.swift | 768 | 793 |
| HeardRight/TranscriptLibrary.swift | 592 | 617 |
| HeardRight/TranscriptCloudSync.swift | 655 | 437 |
| HeardRight/ContentViewSettingsAdvanced.swift | 375 | 375 |
| HeardRight/TranscriptImportExport.swift | 224 | 225 |
| Shared/SharedTranscriptContracts.swift | 217 | 221 |
| HeardRight/WeeklyInsightsStore.swift | 195 | 198 |
| HeardRight/TranscriptCloudSyncCoordinator.swift | 222 | 116 |
| Shared/TranscriptCloudRecordIdentity.swift | 36 | 36 |
| Shared/TranscriptHistoryTools.swift | 22 | 22 |
| HeardRight/TranscriptCloudSync+RemoteFetch.swift | 383 | - |
| HeardRight/TranscriptCloudSync+Deletions.swift | 160 | - |

Main types: `TranscriptLibrary` (`HeardRight/TranscriptLibrary.swift:69`), `SharedTranscriptStore` (`Shared/SharedTranscriptStore.swift:4`). **[grep]**

The cloud sync part is already adopted in MAIN: `HeardRightTranscriptCloudAdapter: CloudSyncAdapter` (`TranscriptCloudSync.swift:31`) drives RK's `CloudSyncEngine` (`RightKitCloudSync/CloudSyncEngine.swift:48`) over `CloudKitSyncTransport`, and the coordinator wraps RK's `CloudSyncCoordinator` and `CloudSyncTriggerSources` (`TranscriptCloudSyncCoordinator.swift:50-69`). The four LIVE sync files (1,420 lines) became two (553 lines). **[read]**

The master plan line 118 says `RightKitLibrary` is shared with ScrapeRight and line 378 folds a `RightKitLibraryUI` target into it. Whether ScrapeRight's library and HeardRight's transcript library share a data model is **not verified** (ScrapeRight was not read).

### RightKitSpeech (exists; 28 files, 4,967 lines)

HeardRight files (17 files / 3,914 lines LIVE as mapped; 19 files / 4,813 lines MAIN):

| File | LIVE | MAIN | Mapped action |
|---|---:|---:|---|
| HeardRight/ModelArtifactInstaller.swift | 780 | 951 | ADOPT |
| HeardRight/LocalASRRuntimeStore.swift | 408 | 489 | EXTEND |
| HeardRight/ASRBenchRunner.swift | 426 | 426 | ADOPT |
| HeardRight/OnDeviceTranscriber.swift | 325 | 346 | EXTEND |
| HeardRight/ModelDownloadLifecycle.swift | 270 | 337 | ADOPT |
| HeardRight/ModelDownloadTransport.swift | 290 | 283 | ADOPT |
| HeardRight/RustKWSBridge.swift | - | 274 | EXTEND |
| HeardRight/CoreMLModelValidator.swift | 256 | 259 | ADOPT |
| HeardRight/ModelInstallation.swift | 242 | 257 | ADOPT |
| HeardRight/ModelDownloadSupport.swift | 87 | 252 | ADOPT |
| HeardRight/ComputePlanProbe.swift | 175 | 175 | ADOPT |
| HeardRight/ModelGrantClient.swift | 162 | 162 | ADOPT |
| HeardRight/ModelDownloadStore.swift | 109 | 120 | ADOPT |
| HeardRight/ANECacheMaintenance.swift | 112 | 112 | ADOPT |
| HeardRight/KeywordCommandDetector.swift | - | 87 | EXTEND |
| HeardRight/RustSpeechProviders.swift | - | 84 | EXTEND |
| HeardRight/SharedModelPublisher.swift | 80 | 80 | ADOPT |
| HeardRight/KeywordCommandAudioCut.swift | 54 | 61 | EXTEND |
| HeardRight/RealtimeTranscriptCoordinator.swift | 58 | 58 | EXTEND |
| HeardRight/SherpaKeywordCommandDetector.swift | 80 | - | EXTEND |

LIVE additionally holds the whole ASR runtime as its own code, mapped (wrongly) to `app-shell`: `ParakeetCoreMLRuntime` 242, `ParakeetCoreMLArray` 301, `ParakeetDecoder` 219, `ParakeetBeamSearch` 211, `ParakeetWindowSeams` 194, `ParakeetCoreMLModel` 150, `ParakeetAudioLoader` 54, `ParakeetDecoderTypes` 9, `WhisperCoreMLRuntime` 398, `WhisperMelSpectrogram` 149, `WhisperTokenizer` 96, `VoiceActivityDetector` 69, `AudioConditioning` 95, `ASRVocabularyBias` 135 (2,322 lines). All are deleted in MAIN.

Coverage:
- **ASR engines: covered and used.** MAIN's `LocalASRRuntimeStore` holds RK `ParakeetEngine` and `WhisperEngine` (`HeardRight/LocalASRRuntimeStore.swift:23,42,441`; RK `RightKitSpeech/SpeechEngines.swift:67,209`). `WarmAudioCapture.swift:134` uses RK `VoiceActivityDetector`; `SharedRecordingCoordinator.swift:145-146` uses RK `IncrementalStatic15Decoder` and `IncrementalStatic15Trigger` (`RightKitSpeech/IncrementalStatic15Decoder.swift:14,289`). **[read]**
- **Model grant: covered, not used.** HeardRight `enum ModelGrantClient` (`HeardRight/ModelGrantClient.swift:17`) and RK `public struct ModelGrantClient` (`RightKitSpeech/ModelGrantClient.swift:64`) have the same `GrantRequest` fields and the same error cases and wording; RK adds `rateLimited` and a no-redirect transport. HeardRight calls its own (`ModelDownloadTransport.swift:83`). **[read]**
- **Core ML validation: covered, not used.** HeardRight has two copies: `HeardRight/CoreMLModelValidator.swift:108` (259 lines) and `Packages/SharedModelRoot/Sources/SharedModelRoot/CoreMLModelValidator.swift:5` (146 lines). RK has `RightKitSpeech/CoreMLModelValidator.swift:154` with the same report, pipeline-config and vocabulary types (`:6,22,35,122`). **[grep]**
- **Download and install: covered in shape, not used.** RK has a background-session `ModelDownloader` (`ModelDownloader.swift:16,67-70`), `ModelInstaller` with a pinned SHA-256 and a ready marker (`ModelInstaller.swift:13,47,79-90`), `ModelDownloadCoordinator` (`ModelDownloadCoordinator.swift:42`) and `ModelDownloadStaging` (`:10`). HeardRight keeps `ModelArtifactInstaller` (`:440`), `ModelDownloadStore` (`:14`), its own background session and staging (`ModelDownloadSupport.swift:21,75`) and its own gzip/tar extraction (`ModelArtifactInstaller.swift:677-746`). **[grep; install bodies not compared]** Gaps to check before deleting the app copy: the Files-import path with its 4 GB inflate cap (`ModelArtifactInstaller.swift:743`), `ModelDownloadTelemetry` (`ModelDownloadTransport.swift:229`), `UltraOnboardingReset` (`ModelArtifactInstaller.swift:316`).
- **Not covered:** `ANECacheMaintenance` (no equivalent found by name in RK Swift; **[grep]**), `RustKWSBridge` and `KeywordCommandDetector` (keyword spotting over the Rust core; RK Swift has no keyword-spotting type), `ASRBenchRunner` and `ComputePlanProbe` (SwiftUI bench screens, see section 4).
- Product data that stays: `ASRModelOption` with routes and digests (`ModelArtifactInstaller.swift:53-87,133`).

### RightKitLicense (exists; 5 files, 2,339 lines) and RightKitPurchases (exists; 7 files, 874 lines)

| File | LIVE | MAIN |
|---|---:|---:|
| HeardRight/LicenseController.swift | 723 | 346 |
| HeardRight/ProEntitlementResolver.swift | 383 | 393 |
| HeardRight/StoreKitProController.swift | 355 | 340 |
| Shared/LicenseEntitlement.swift | 591 | 194 |
| Shared/ProBridge.swift | 117 | 126 |
| Shared/ProAccessStore.swift | 57 | 57 |

Coverage and use in MAIN:
- Activation, refresh, lease, removal, token verification: **used.** `HeardRightLicenseClient` (`HeardRight/LicenseController.swift:180`) delegates each call to RK `LicenseActivationClient` (`RightKitLicense/LicenseClient.swift:640`); `LicenseKeyStore` (`:249`) is a facade over RK `KeychainLicenseStore` (`LicenseClient.swift:268`). **[read]**
- StoreKit: **used.** `StoreKitProController` owns an RK `PurchaseController` with `StoreKit2Backend` (`HeardRight/StoreKitProController.swift:55,116-118`; RK `RightKitPurchases/PurchaseController.swift:18`). **[read]**
- Still the app's own code although RK has the owner:
  - `LicenseController: ObservableObject` (`HeardRight/LicenseController.swift:12`) versus RK `LicenseController` (`LicenseClient.swift:1115`), which has the same published state, a mutation generation and a pending-removal queue. **[read]**
  - `PendingLeaseRemoval` plus three UserDefaults keys (`LicenseController.swift:6,22-24`) versus RK `PendingLicenseRemovalQueue` and `BridgeConsumptionGate` (`RightKitLicense/LicenseRemovalQueue.swift:115,291`). **[read]**
  - Windows link code: hand-written request (`LicenseController.swift:233-244`, comment at `:177` "endpoints not yet in RightKit") versus RK `requestWindowsLinkCode` (`LicenseClient.swift:816,826`). **[read]**
  - Pro bridge: `ProBridge` URLs and payloads (`Shared/ProBridge.swift:23`) and a private `ProBridgeClient` (`HeardRight/ProEntitlementResolver.swift:316`) versus RK `ContinuityBridgeClient` (`RightKitLicense/ContinuityBridge.swift:119`). RK `CHANGELOG.md:27` states the client is on main. **[read]**
  - `ProAccessStore` (`Shared/ProAccessStore.swift:15`) versus RK `AccessCache` and `AccessResolver` (`RightKitPurchases/Storage.swift:31`, `LicenseAccess.swift:24`). **[grep]**
- Stays in the app: product identifiers (`StoreKitProController.swift:30-33`), URLs, key prefix, the `ProEntitlementResolver` policy that combines StoreKit and license sources.

Verdict: covers what HeardRight does; the remaining gap is on HeardRight's side (pin at 0.8.1, facades not yet removed).

### RightKitAppleCredentials (exists; 5 files, 1,735 lines)

| File | LIVE | MAIN |
|---|---:|---:|
| Shared/AppleSyncedCredentialStore.swift | 356 | 300 |
| tauri-app-next/.../HeardRightAppleCredentials/main.swift (mapped to `rightkit-overlay`) | 357 | 309 |

- iOS MAIN imports it (`Shared/AppleSyncedCredentialStore.swift:4`) but still defines `SyncedCredentialAccount`, `CredentialSyncOutcome` and `AppleSyncedCredentialStore` (`:31,54,82`). RK has `AppleCredentialStore` (`RightKitAppleCredentials/RightKitAppleCredentials.swift:123`), `CredentialSyncOutcome` (`SyncedCredentialPolicy.swift:6`) and `SyncedCredentialReconciler` (`:67`). How much of the 300 lines is a thin facade was **not verified** line by line. **[grep]**
- Mac helper MAIN: Keychain access goes through RK `AppleCredentialStore` (`main.swift:173`), but the helper still reads the request line and decodes it itself (`main.swift:252-258`). RK `AppleCredentialHelperHost` (`RightKitAppleCredentials/AppleCredentialHelperHost.swift:70`) owns that wire protocol and its header says it was "moved out of HeardRight's `HeardRightAppleCredentials` CLI" (`:3-4`). **[read]** So the helper should shrink to its service name, access group, account allowlist and a call to the host.

### RightKitDiagnostics (exists; 2 files, 449 lines)

| File | LIVE | MAIN |
|---|---:|---:|
| Shared/DogfoodTraceStore.swift | 149 | 223 |
| Shared/DiagnosticsReport.swift | 179 | 160 |
| Shared/MobileDiagnosticSubscriber.swift | 81 | 80 |

- MAIN uses one symbol: `PrivacyRedactor().redact` (`Shared/DiagnosticsReport.swift:158`). **[read]**
- `DogfoodTraceStore` (`Shared/DogfoodTraceStore.swift:3`) is still a hand-written bounded JSONL store (256 KB cap, serial write queue, backup exclusion; `:4-25`). RK `JSONLDiagnosticStore` (`RightKitDiagnostics/JSONLDiagnosticStore.swift:89`) is the extraction of exactly this file according to `swift/README.md` (table row `RightKitDiagnostics`), with `maxBytes`, `retainBytes`, redaction and an uploader option (`StoreOptions` `:43`). Not used. **[read]** Gap to check: HeardRight records `[String: Any]` fields (`:37`), RK's `DiagnosticEvent` takes `[String: String]` (`:22`); and the DEBUG campaign file kept outside Caches (`DogfoodTraceStore.swift:57-60`).
- `HeardRightDiagnosticsReport` (`DiagnosticsReport.swift:22`) is product data and stays. `MobileDiagnosticSubscriber` (MetricKit) has no RK counterpart. **[grep]**

### RightKitIntents (exists; 1 file, 139 lines)

| File | LIVE | MAIN |
|---|---:|---:|
| Shared/HeardRightAppIntents.swift | 740 | 855 |
| Shared/IOSVoiceCommand.swift | 340 | 340 |
| Shared/HeardRightStartNeededIntent.swift | - | 29 |

- RK provides `RightKitRecordingDriver` (`:10`), `RightKitRecordingAction` (`:28`), `RightKitDarwinNotification` (`:60`), `RightKitMicrophonePermissionHandshake` (`:78`) and `RightKitRecordingBridge.dispatch` (`:112-137`). **[read]**
- MAIN uses the driver protocol, the Darwin post and the permission handshake (`Shared/HeardRightAppIntents.swift:14,27,99,340,799,824`). It does not call `RightKitRecordingBridge.dispatch`; it keeps its own `HeardRightRecordingBridge` (`:25`) and about 800 lines of intent bodies, a permission phase machine (`:71`) and an error type (`:686-736`). **[read for the RK calls; intent bodies grep only]**
- RK is a thin seam, not a home for the four `AppIntent` structs (`:368,452,547,573`). They carry HeardRight titles and phrases and must be declared by the app or its extension.
- `IOSVoiceCommand.swift` is a spoken-command parser (`IOSVoiceCommandParser` `:144`), not App Intents code. See section 4.

### RightKitLiveActivity (exists; 2 files, 322 lines)

| File | LIVE | MAIN |
|---|---:|---:|
| HeardRightSharedUI/RecordingActivityViews.swift | 576 | 765 |
| HeardRightSharedUI/RecordingDesignGallery.swift | 76 | 100 |
| HeardRightSharedUI/HeardRightRecordingActivityAttributes.swift | 47 | 69 |

- RK is lifecycle only: `ActivityLifecycle` (`ActivityLifecycle.swift:27`), `LifecycleFence` (`:84`), `LiveActivityHost` (`LiveActivityHost.swift:34`), `KeyedLiveActivityHost` (`:95`). It has no SwiftUI views. **[read]**
- The lifecycle is used: `RecordingLiveActivityController` (`HeardRight/RecordingLiveActivityController.swift:8,15,36,53`). LIVE's `Shared/RecordingActivityStateMachine.swift` (87 lines) is deleted in MAIN in favour of RK. **[read]**
- The three mapped files are the Dynamic Island and Lock Screen views (`ExpandedRecordingSurface` `:223`, `LockScreenRecordingView` `:555`) and the `ActivityAttributes` type (`:51`). RK does not cover them and its extraction contract says Live Activity presentation is product behaviour (`swift/README.md`, extraction rules). Mapping these as "EXTEND RightKitLiveActivity" means adding a view layer the module does not have today.

### RightKitWatch (exists, but is a different thing)

RK `RightKitWatch` is an FSEvents file watcher for macOS: `RightKitWatcher` (`RightKitWatch/RightKitWatch.swift:216`), `RightKitWatchEventID.current()` calling `FSEventsGetCurrentEventId()` (`:20`); `Package.swift` comment "FSEvents file watcher (macOS only; the target is empty on iOS)". **[read]** It has nothing to do with Apple Watch.

HeardRight files mapped to it (15 files / 2,153 lines LIVE; 19 files / 3,476 lines MAIN):

| File | LIVE | MAIN |
|---|---:|---:|
| HeardRightWatch/WatchRecordingController.swift | 533 | 754 |
| HeardRightWatch/WatchRecordingTransfer.swift | 387 | 704 |
| HeardRightWatch/HeardRightWatchApp.swift | 320 | 446 |
| HeardRight/WatchRecordingProcessor.swift | 284 | 417 |
| HeardRight/WatchRecordingInbox.swift | 215 | 231 |
| HeardRightWatch/WatchRecordingControllerSmokeTests.swift | 54 | 173 |
| HeardRightWatch/WatchRecordingRecovery.swift | - | 102 |
| HeardRight/WatchPendingStore.swift | - | 97 |
| HeardRightWatch/WatchTelemetry.swift | 70 | 90 |
| Shared/WatchPendingPolicy.swift | - | 90 |
| Shared/WatchRecordingState.swift | - | 68 |
| HeardRightWatch/WatchModeStore.swift | 59 | 59 |
| HeardRight/WatchBackgroundProcessing.swift | 40 | 51 |
| HeardRightWatch/WatchTelemetryDiagnostics.swift | 83 | 49 |
| HeardRightWatch/WatchRecordingIntents.swift | 34 | 44 |
| Shared/WatchRecordingDelivery.swift | 13 | 37 |
| HeardRightWatch/WatchRecordingMeter.swift | 27 | 27 |
| Shared/WatchRecordingControlIntent.swift | 25 | 27 |
| Shared/WatchTelemetryKeys.swift | 9 | 10 |

The real RK owners:
- `RightKitWatchBridge` (2 files, 263 lines): `TransferLedger` (`TransferLedger.swift:31`), `DualPathSender`, `WatchReceipts`, `RelocatableFile`, `WCSessionTransport` (`WatchBridgeSupport.swift:24,38,52,111`). **Used** in MAIN: `HeardRightWatch/WatchRecordingTransfer.swift:35,67-72,121,290,388,466`, `HeardRight/WatchRecordingInbox.swift:89,167-229`, `HeardRight/WatchPendingStore.swift:50`, `Shared/WatchRecordingDelivery.swift:16-35`. **[read]**
- `RightKitAudio.AudioFileRecorder` (`RightKitAudio/AudioFileRecorder.swift:103`): an `AVAudioRecorder` file recorder for watchOS with metering and an interruption policy whose default is described as "the watch default" (`:112-114`). **Not used**: `WatchRecordingController` owns its own `AVAudioRecorder` (`HeardRightWatch/WatchRecordingController.swift:89,208`). **[read]**

The `WCSessionDelegate` classes (`WatchRecordingTransfer` `:8`, `WatchRecordingInbox` `:12`), the recovery store and the watch SwiftUI screens have no RK counterpart.

### RightKitUI (exists; 2 files, 230 lines)

| File | LIVE | MAIN |
|---|---:|---:|
| HeardRight/OnboardingSetupSteps.swift | 436 | 575 |
| HeardRight/OnboardingIntroSteps.swift | 330 | 330 |
| HeardRight/OnboardingCompletionSteps.swift | 257 | 257 |
| HeardRight/OnboardingComponents.swift | 244 | 244 |
| HeardRight/OnboardingView.swift | 152 | 157 |

- RK `RightKitUI` is an adaptive glass surface modifier (`AdaptiveGlassSurface.swift:3-9`) and WCAG contrast maths (`RightKitContrast.swift:9`). It has no onboarding, no settings scaffold, no navigation. **[read]**
- HeardRight uses the glass modifier at `HeardRight/RecordingPillView.swift:205` and `HeardRight/ContentViewHomeControls.swift:324`, in both LIVE and MAIN. **[read]**
- The onboarding files (`OnboardingView` `:3`, `ShortcutSetupGuide` `OnboardingSetupSteps.swift:257`) are HeardRight screens with HeardRight copy. Nothing in RK covers them.
- The mapping document also sends 23 files / 3,903 lines of "iOS app scaffold" to `RightKitUI`, but the inventory JSON marks the same files `app-shell` / `SHELL` (for example `HeardRight/ContentView.swift`). The two disagree; see section 4.

### RightKitHubQA (exists, but is a different thing)

RK `RightKitHubQA` is three files of fakes for the desktop Hub: `HubQA` gallery (`HubQAGallery.swift:8`), `QAEngineClient: EngineClient` (`QAEngineClient.swift:8`), `RecordingPlatformServices: HubPlatformServices` (`QAServices.swift:9`). It depends on `RightKitHub`. **[read]**

The inventory JSON maps 34 files / 4,631 lines (LIVE) and 48 files / 6,749 lines (MAIN) of iOS tests to it: `HeardRightUITests/*` (XCUITest), `Tests/*_smoke.swift` (standalone swiftc smokes), `Tests/RightKit*E2E` helper packages, `HeardRightSyncTests/TranscriptCloudSyncTests.swift`, `Tests/IslandAuditCompanion/*`, `scripts/compute-plan-probe.swift`. HeardRight imports neither `RightKitHub` nor `RightKitHubQA`. The mapping document lists the same files as `app-shell` / `APPTEST` ("stays with the app"), which is the correct reading. **[read]**

### RightKitOverlay (exists; 1 file, 804 lines)

See section 3. HeardRight files mapped to the `rightkit-overlay` crate: 21 files / 7,380 lines LIVE, 17 files / 4,735 lines MAIN. Not used by the pill.

### Other targets that have HeardRight Swift mapped

| Target (as mapped) | Files | LIVE | MAIN | Exists as Swift? | Finding |
|---|---|---:|---:|---|---|
| `rightkit-mesh` (crate) | HeardRight/AccountSessionStore.swift, DeviceManagementView.swift, DevicePairingStore.swift | 866 | 873 | no Swift target | `AccountSessionStore.swift` (533) defines `AccountSessionClient` (`:151`), `AccountSessionStore` (`:359`), `AccountDeviceRow` (`:53`), `AccountSessionError` (`:66`), `StoredAccountSession` (`:93`). RK `RightKitAccount` has types with the same names (`AccountSession.swift:11,51,75`; `SessionStore.swift:5,79`). HeardRight does not link `RightKitAccount`. Coverage **[grep]**, bodies not compared. `DevicePairingStore` (`:29`) is a WatchConnectivity state object, not mesh. |
| `rightkit-text-polish` (crate) | HeardRight/AppleIntelligencePolishClient 105, BYOKPolishKeyStore 138, HeardRightCorePolisher 33, TextRulesController 100, TranscriptPolishPipeline 89/115, Shared/BYOKPolishClient 318, TextRulesStore 206, TranscriptPolish 259 | 1,248 | 1,274 | no Swift target | Needs a Swift home. `BYOKPolishKeyStore` (`:51`) is Keychain storage (RK `RightKitCredentials.SecretsStore`, `RightKitCredentials.swift:372`); `BYOKPolishClient` (`:85`) is a provider HTTP client (RK `RightKitLLM.LLMClient`, `LLMClient.swift:5`). Overlap **not verified**. |
| `@rightkit/theme` (npm) | HeardRight/AppMark 48, Color+HR 73/74, Font+HR 149 | 270 | 271 | n/a | The plan says Swift brand tokens are generated from `brand.json`. These three files are then generator output that the app keeps; no Swift module is implied. |
| `rightkit-models` (crate) | Packages/SharedModelRoot/* (4 files) | - | 264 | no | New in MAIN. Holds a second `CoreMLModelValidator` (146 lines). Duplicate of both the app copy and RK. |
| `rightkit-ml-lab` (crate) | tauri-app-next/scripts/coreml/*.swift | 348 | 127 | no | Bench scripts; not app code. |

## 3. The Mac pill: HeardRightPillMac versus RightKit's Swift RightKitOverlay

### What HeardRightPillMac does (MAIN; 4,863 lines in the package)

A static library linked into the Tauri app and driven from Rust over a C interface, plus three helper executables. **[read]**

| Part | File (lines in MAIN) | What it does |
|---|---|---|
| C interface | `HeardRightPillMacBridge/include/hr_pill.h` (50), `PillBridge.swift` (231) | 16 callbacks from Swift to Rust (`hr_pill.h:11-28`: copy text, open hub, stop recording, cancel processing, start dictation, recover terminal error, splash stall and action) and 16 `@_cdecl` entry points (`PillBridge.swift:102-224`), including `hr_pill_push_event(kind, level, cancellable, label)` |
| Panel and placement | `PillPanel.swift` (667), `FrameAnimator.swift` (65) | `HeardRightPillPanel` (`PillPanel.swift:6`): borderless non-activating `NSPanel` at window level 25, joins all Spaces and full-screen Spaces, key only when `allowsIntentionalEditingFocus` is set. `PillPanelController` (`:142`) sizes and centres the capsule, defers content until the splash frame, installs mouse monitors. `PillHostingView` (`:50`) does hover with a tracking area and a 50 ms watchdog. `GenerationGuardedFrameAnimator` (`FrameAnimator.swift:22`) is an 8 ms timer animator with a generation guard |
| State | `PillModel.swift` (592) | `PillModel` (`:177`) maps event kinds 0-7 to modes (`:215-236`: 0 hide, 1 idle, 2 recording, 3 level, 4 processing, 5 done, 6 status label, 7 error), owns hover, clipboard and hint state, plays `BloomSoundPlayer` (`:6`) and `ActionSoundPlayer` (`:86`) sounds read from the app's settings file |
| Views | `PillRootView.swift` (394), `AuxiliaryViews.swift` (323), `PillTokens.swift` (139), `PillMotion.swift` (85), `HintCatalog.generated.swift` (24) | SwiftUI capsule, recording bars, processing waveform, hover buttons, clipboard list, hints, colour and font tokens, level smoothing |
| Launch | `SplashPanel.swift` (583), `PillLaunchHandoff.swift` (109), `HubTransitionPanel.swift` (172) | Native splash window with a step loader and stall recovery, the "fly into pill" hand-off, and the animated frame transition between pill and hub window |
| CloudKit wake | `CloudSyncNotifications.swift` (610) | Keeps a CloudKit zone subscription alive, listens for pushes and foreground/wake/network changes, and wakes the Rust sync coordinator through three `@_cdecl` functions (`:590-605`) |
| Helpers | `HeardRightMenuBar/main.swift` (166), `HeardRightShortcuts/main.swift` (193), `HeardRightAppleCredentials/main.swift` (309) | A separate status-item app with a disposable bundle identifier (`main.swift:1-22`); an App Intents broker that hands commands to the desktop app; the Keychain helper |

### What RightKit's Swift RightKitOverlay does (804 lines, one file)

**[read]**

- Value types usable on any platform: `RightKitOverlayKind` (`.pill`, `.notch`; `:17`), `RightKitPillMode` (`:29`), `RightKitOverlayEvent` with cases hide, idle, starting, recording, processing(cancellable), status(label), error(label) and a Codable form (`:33-87`), rectangle and work-area clamping (`:89-124`), `RightKitOverlayModel` (`:126`), hit regions and the click-through decision (`:169-210`), display add/remove planning (`:223-249`), full-screen detection rules (`:251-348`).
- macOS only: `RightKitOverlayPanelConfiguration` (`:355`) with level, collection behaviour, key policy, shadow, movability, click-through start; `RightKitOverlayPanel` (`:436`), an `open` non-activating `NSPanel` with `allowsKey` settable at runtime (`:444`), `hitRegions`, `hitShape`, an event filter, `showWithoutActivating()`, `showAsKey()`, `hide()` (`:576-590`); `RightKitOverlayContentView` (`:595`); `RightKitOverlayPointerTracker` (`:626`), global and local mouse monitors plus a poll that toggles `ignoresMouseEvents`.
- It contains no SwiftUI, no hosting view, no frame animation, no audio level, no sounds, no splash, no clipboard or hint surface and no C interface (grep for `animat`, `SwiftUI`, `NSHostingView`, `sound`, `splash`, `setFrame`, `NSTrackingArea` finds only comments and the `animationBehavior` property).

### Overlap today

Only the panel shell. `HeardRightNonActivatingPanel` plus `HeardRightPillPanel` (`FrameAnimator.swift:8-11`, `PillPanel.swift:6-16`) is what `RightKitOverlayPanel` with a configuration provides: level 25 fits `configuration.level`; `[.canJoinAllSpaces, .stationary, .fullScreenAuxiliary]` is exactly `stationaryFullScreenAuxiliary` (RK `:421-422`); `allowsIntentionalEditingFocus` is `allowsKey`. The event vocabulary nearly matches: HeardRight kinds 0, 1, 2, 4, 6, 7 correspond to RK hide, idle, recording, processing, status, error. HeardRight kind 3 (audio level) and kind 5 (done) have no RK case, and RK's `.starting` has no HeardRight kind.

### What RightKitOverlay needs before the pill could use it

1. **An audio level path.** The pill receives level updates at high rate through the same call as mode changes (`PillModel.swift:217`). RK's event has no level. Add a level value (or a separate level channel) and a smoothing driver like `PillLevelDriver` (`PillMotion.swift:19`).
2. **A "done" outcome** or a rule that done is `status("Done")` (`PillModel.swift:222`).
3. **A C interface**, or an agreed place for one. The pill is driven from Rust through `@_cdecl` functions and a callback table. RK Swift has no such layer; the Rust crate `crates/rightkit-overlay` exists (`src/lib.rs`, `src/stack.rs`) but whether it defines a matching Swift-side contract is **not verified**.
4. **A SwiftUI host and hover tracking.** The pill's `PillHostingView` reports hover with a tracking area and a watchdog that stops when the panel is hidden (`PillPanel.swift:50-60,104-106`). RK offers pointer tracking for click-through only; it reports the pointer position (`onPointer`, `:629`) but has no hover-enter/leave state or hosting view.
5. **Anchored resize and animation.** `CenteredPanelFrameCoordinator` (`PillPanel.swift:18`) and `GenerationGuardedFrameAnimator` keep the capsule centred on the screen while it grows. RK only clamps a frame to a work area.
6. **The pill content itself**: capsule, bars, waveform, buttons, tokens, and the sounds. If "no duplicate code" includes the Windows pill and other apps showing the same pill, this needs a pill view layer in RK with injected tokens, strings and sounds. If only HeardRight shows it, it is product UI.
7. **Splash, launch hand-off and hub transition** (864 lines). These are app-launch behaviour, not overlay behaviour. They need an owner decision: a launch/splash module beside the overlay, or the app.
8. **A stable event log hook.** The pill logs through a Rust callback (`PillCallbacks.shared.log`, used throughout `PillPanel.swift`); RK has no logging seam in the overlay.

Items 1, 2, 4 and 5 are small additions to an 804-line module. Items 3, 6 and 7 are design decisions.

Separately from the overlay, two parts of the pill package already have RK owners that MAIN does not use:
- `CloudSyncNotifications.swift` (610 lines) versus RK `CloudKitWakeHost` (`RightKitCloudSync/CloudKitWakeHost.swift:62`, 489 lines), whose header says it was "moved out of HeardRight's macOS pill helper (`CloudSyncNotifications.swift`, PK16-2)" (`:9-10`) and that the app keeps only identifiers and the FFI entry points (`:16-19`). MAIN's copy even re-implements the entitlement check (`CloudSyncNotifications.swift:11-22`) that RK has as `CloudKitEntitlement` (`CloudKitWakePolicy.swift:27`). **[read]**
- The credentials helper, described in section 2.

## 4. Mapping errors

| Mapped | File(s) | Lines (MAIN) | Belongs with | Why |
|---|---|---:|---|---|
| `rightkit-overlay` crate | tauri-app-next/.../HeardRightAppleCredentials/main.swift | 309 | `RightKitAppleCredentials` (`AppleCredentialHelperHost`) | Keychain helper, no overlay code [read] |
| `rightkit-overlay` crate | .../HeardRightPillMac/CloudSyncNotifications.swift | 610 | `RightKitCloudSync` (`CloudKitWakeHost`) | CloudKit subscription and wake [read] |
| `rightkit-overlay` crate | .../HeardRightCloudSync/CloudSyncMain.swift, Tests/HeardRightCloudSyncTests/IncrementalPushTests.swift (LIVE only) | 1,124 + 222 | `RightKitCloudSync` / `rightkit-cloudkit-helper` | CloudKit CLI; already replaced in MAIN [read] |
| `rightkit-overlay` crate | .../HeardRightShortcuts/main.swift | 193 | `RightKitIntents` (macOS broker) or the app | App Intents, links `AppIntents` (`Package.swift` target) [read] |
| `rightkit-overlay` crate | .../HeardRightMenuBar/main.swift | 166 | a status-item helper owner (`RightKitSystem` has login item and banners but no status item; **[grep]**) or the app | Menu bar status item, not the pill [read] |
| `rightkit-overlay` crate | SplashPanel.swift, PillLaunchHandoff.swift, HubTransitionPanel.swift | 864 | launch/splash owner, to be decided | Launch sequence, not overlay [read] |
| `rightkit-overlay` crate (kind "crate") | all pill Swift | 4,735 | Swift module `RightKitOverlay` | The mapping names the Rust crate for Swift source; the Swift product of the same name is the only place Swift can go |
| `RightKitWatch` | 19 Apple Watch files | 3,476 | `RightKitWatchBridge`, `RightKitAudio`, and a new watch-recording module | `RightKitWatch` is the FSEvents file watcher [read] |
| `RightKitHubQA` | 48 iOS test files | 6,749 | stay with the app (as the mapping document's own APPTEST rows say) | `RightKitHubQA` is desktop Hub fakes [read] |
| `RightKitLibrary` | HeardRight/ContentViewSettings.swift, ContentViewSettingsAdvanced.swift | 1,400 | settings UI (`RightKitUI` or app) | Settings screens, not transcript library [grep] |
| `RightKitLicense` | HeardRight/StoreKitProController.swift, Shared/ProAccessStore.swift | 397 | `RightKitPurchases` | StoreKit and access cache; MAIN already imports `RightKitPurchases` there [read] |
| `rightkit-mesh` crate | HeardRight/AccountSessionStore.swift | 533 | `RightKitAccount` | Account session client and store; RK module with the same type names exists [grep] |
| `rightkit-mesh` crate | HeardRight/DevicePairingStore.swift | 77 | watch module | WatchConnectivity pairing state [grep] |
| `RightKitIntents` | Shared/IOSVoiceCommand.swift | 340 | Swift side of `rightkit-voice-commands`, or `RightKitDictation` | Spoken-command parser, no App Intents [grep] |
| `RightKitSpeech` ADOPT | HeardRight/ASRBenchRunner.swift, ComputePlanProbe.swift | 601 | QA/bench tooling in the app | SwiftUI bench screens (`ASRBenchView` `:33`, `ComputePlanProbeView` `:34`); RK does not ship them, so ADOPT ("delete app copy and use owner") is wrong [grep] |
| `RightKitSpeech` ADOPT | HeardRight/ANECacheMaintenance.swift | 112 | `RightKitSpeech` EXTEND | No RK equivalent found [grep] |
| `RightKitDictation` | HeardRight/WarmAudioCapture.swift | 772 | `RightKitAudio` (ADOPT with gaps) | RK already owns a type of the same name [read] |
| `RightKitDictation` | HeardRight/RecordingLiveActivityController.swift | 447 | adapter over `RightKitLiveActivity` | Already built on `LiveActivityHost` [read] |
| `RightKitDictation` | Shared/SharedImportQueue.swift, HeardRight/PendingImportProcessor.swift, AudioImportCoordinator.swift, DocumentImportCoordinator.swift | 398 | `RightKitImportQueue` (the generic part) | Share/import intake [grep] |
| `app-shell` (LIVE) | Parakeet*, Whisper*, VoiceActivityDetector, AudioConditioning, ASRVocabularyBias (14 files) | 2,322 LIVE | `RightKitSpeech` | ASR runtime marked as shell; moot in MAIN where they are deleted [read] |
| `app-shell` | HeardRight/AppUpdateController.swift | 115 | check against `RightKitUpdate` | **not verified** |
| `rightkit-text-polish` crate | BYOKPolishKeyStore.swift | 138 | `RightKitCredentials` | Keychain storage [grep] |
| `@rightkit/theme` (npm) | AppMark, Color+HR, Font+HR | 271 | generated output kept in the app | A Swift file cannot move into an npm package |

Two documents disagree. The mapping document (`per-app/heardright-rightkit-mapping.md`) sends 23 scaffold files / 3,903 lines to `RightKitUI` EXTEND and the iOS tests to `app-shell` APPTEST. The inventory JSON in the HeardRight tree marks the same scaffold files `app-shell` / `SHELL` and the tests `RightKitHubQA` / `EXTEND`. The mapping document's summary also says "Staying in the app as product code: 0 lines", which conflicts with RightKit's own extraction contract (`swift/README.md`, extraction rules: "Core ML, ASR, StoreKit, keyboard, Live Activity, App Intents, audio session, navigation, and visual design are product behavior, not shared infrastructure" and "product-facing `ObservableObject` controllers stay in app repos"). RightKit has already moved past that contract (`LicenseController` and `PurchaseController` are `ObservableObject`s in RK), so the contract file is stale; it should be rewritten or the plan should cite it as superseded.

## 5. The no-duplicate end state for Swift

### Modules to create in RightKitSwift

Only two of the six missing packages are needed for HeardRight. Line counts are MAIN.

**A. `RightKitDictation`** (iOS; a watchOS slice for item C). The dictation runtime, built on `RightKitAudio`, `RightKitSpeech`, `RightKitStore`, `RightKitLiveActivity`.

| Moves in | Lines |
|---|---:|
| HeardRight/RecordingAudioSession.swift | 1829 |
| HeardRight/RecordingSystemSurface.swift | 1192 |
| HeardRight/RecordingAudioPolicy.swift | 1074 |
| HeardRight/DictationShortcuts.swift | 595 |
| HeardRight/SharedRecordingTranscriber.swift | 427 |
| HeardRight/DictationMicrophone.swift | 398 |
| HeardRight/SharedRecordingCoordinator.swift | 333 |
| HeardRight/LongFormRecordingLifecycle.swift | 322 |
| HeardRight/DictationRecording.swift | 310 |
| Shared/SharedDictationSnapshot.swift | 277 |
| HeardRight/RecordingOwnershipLease.swift | 199 |
| HeardRight/DictationController.swift | 197 |
| Shared/SharedDictationResultWaiter.swift | 155 |
| Shared/SharedDictationCommand.swift | 142 |
| HeardRight/RecordingCommandBridge.swift | 137 |
| HeardRight/RecordingCheckpointStore.swift | 120 |
| HeardRight/DictationPresentation.swift | 111 |
| HeardRight/BackgroundTranscriptionGuard.swift | 102 |
| HeardRight/DictationSupport.swift | 97 |
| Shared/AudioCaptureRecoveryPolicy.swift | 75 |
| HeardRight/SavedAudioStore.swift | 71 |
| Shared/RecordingResultPreview.swift | 17 |
| HeardRight/OnDeviceTranscriber.swift (orchestration over RK engines) | 346 |
| HeardRight/LocalASRRuntimeStore.swift (engine cache and priming) | 489 |
| HeardRight/RealtimeTranscriptCoordinator.swift | 58 |
| HeardRight/KeywordCommandDetector.swift, KeywordCommandAudioCut.swift, RustKWSBridge.swift | 422 |
| Shared/IOSVoiceCommand.swift | 340 |
| Subtotal | about 9,835 |

App group name, defaults keys, deep-link scheme and strings must be injected; today they come from `HeardRightShared` (`Shared/SharedTranscriptContracts.swift:3`).

A view target beside it (`RightKitDictationUI`), if the owner wants views out of the app: `RecordingPillView.swift` 225, `HardwareRecordingControls.swift` 492, the Live Activity views 934 and `RecordingLiveActivityController.swift` 447.

**B. `RightKitLibrary`** (plus the `RightKitLibraryUI` target the plan folds in).

| Moves in | Lines |
|---|---:|
| Shared/SharedTranscriptStore.swift | 919 |
| HeardRight/TranscriptLibrary.swift | 617 |
| HeardRight/TranscriptCloudSync.swift (adapter over `RightKitCloudSync`) | 437 |
| HeardRight/TranscriptImportExport.swift | 225 |
| Shared/SharedTranscriptContracts.swift (record and tombstone types only) | part of 221 |
| HeardRight/WeeklyInsightsStore.swift | 198 |
| HeardRight/TranscriptCloudSyncCoordinator.swift | 116 |
| Shared/TranscriptCloudRecordIdentity.swift | 36 |
| Shared/TranscriptHistoryTools.swift | 22 |
| UI target: HeardRight/ContentViewHistory.swift | 793 |
| Subtotal | about 3,580 |

**C. A watch-recording home.** Not `RightKitWatch` (taken). Either extend `RightKitWatchBridge` with the two `WCSessionDelegate` halves, or add a watchOS slice to `RightKitDictation`. Moves in: `WatchRecordingController` 754 (on `AudioFileRecorder`), `WatchRecordingTransfer` 704, `WatchRecordingProcessor` 417, `WatchRecordingInbox` 231, `WatchRecordingRecovery` 102, `WatchPendingStore` 97, `WatchPendingPolicy` 90, `WatchTelemetry` 90, `WatchRecordingState` 68, `WatchModeStore` 59, `WatchBackgroundProcessing` 51, `WatchTelemetryDiagnostics` 49, `WatchRecordingDelivery` 37, `WatchRecordingMeter` 27, `WatchTelemetryKeys` 10, `DevicePairingStore` 77 (about 2,860).

**D. A Swift polish target** (the `rightkit-text-polish` crate has none): `TranscriptPolish` 259, `TextRulesStore` 206, `TextRulesController` 100, `TranscriptPolishPipeline` 115, `AppleIntelligencePolishClient` 105, `HeardRightCorePolisher` 33, and `BYOKPolishClient` 318 / `BYOKPolishKeyStore` 138 after checking them against `RightKitLLM` and `RightKitCredentials` (1,274).

**E. Additions to existing modules** (no new package):
- `RightKitOverlay`: section 3, items 1-5 and 8; then the pill panel, model and views (`PillPanel` 667, `PillModel` 592, `PillRootView` 394, `AuxiliaryViews` 323, `PillBridge` 231, `PillTokens` 139, `PillMotion` 85, `FrameAnimator` 65) and, once an owner is chosen, splash and transitions (864).
- `RightKitSpeech`: ANE cache maintenance 112, `SharedModelPublisher` 80, the Files-import install path, download telemetry hook.
- `RightKitAudio`: in-memory accumulation, speech-activity snapshot, WAV writer; a session policy layer if `RecordingAudioPolicy` is to move.
- `RightKitDiagnostics`: non-string field values; a MetricKit subscriber 80.
- `RightKitIntents`: only if the owner wants the recording intent bodies shared. The `AppIntent` declarations stay in the app either way.
- `RightKitUI`: onboarding scaffold (1,563) and settings scaffold (1,400) only if a second app will use them; otherwise they are app screens.

### What stays in the app

- Entry points: `HeardRightApp.swift` 186, `HeardRightAppDelegate.swift` 80, `MainAppDispatcher.swift` 152, the widget, watch and share-extension entry files, `HeardRightWatchApp.swift`.
- Composition: `ContentView*.swift`, `HeardRightRuntime.swift`, and a thin file per RK module that passes HeardRight's values in.
- Product data: model catalogue with routes and digests (`ASRModelOption`), StoreKit product identifiers, license URLs and app key, Keychain service and access-group names, app group, CloudKit container and zone names, deep-link scheme, strings.
- Declarations the system requires in the app: the `AppIntent` and `AppShortcutsProvider` types, `ActivityAttributes`, the Live Activity widget configuration.
- Generated brand tokens: `AppMark`, `Color+HR`, `Font+HR`.
- Product policy: `ProEntitlementResolver` (how StoreKit and license sources combine).
- Tests: UI tests, smokes and E2E helper packages.
- Mac: the `@_cdecl` functions and callback table (they are the app's contract with its Rust shell), the three helper `main.swift` files reduced to configuration plus a call into RK, identifiers.

### Order of work

1. **Correct the mapping** (section 4) so no file is planned into `RightKitWatch`, `RightKitHubQA` or a Rust crate by mistake, and make the mapping document and the inventory JSON agree.
2. **Finish adoption where RightKit already has the owner.** No new RK code, mostly deletion in HeardRight. Needs one Swift pin for the whole repo at a published version that contains 0.9.0's modules (0.9.1 is untagged; `ContinuityBridgeClient` availability at the 0.9.0 tag is **not verified**).

   | Delete in HeardRight (MAIN lines) | Use instead |
   |---|---|
   | ModelGrantClient 162 | `RightKitSpeech.ModelGrantClient` |
   | CoreMLModelValidator 259 + SharedModelRoot copy 146 | `RightKitSpeech.CoreMLModelValidator` |
   | ModelArtifactInstaller, ModelDownload*, ModelInstallation (2,200 less the catalogue) | `ModelDownloader`, `ModelInstaller`, `ModelDownloadCoordinator` |
   | WarmAudioCapture 772 | `RightKitAudio.WarmAudioCapture` (after the gaps in E) |
   | AVAudioRecorder code in WatchRecordingController | `RightKitAudio.AudioFileRecorder` |
   | DogfoodTraceStore 223 | `JSONLDiagnosticStore` |
   | LicenseController 346 (class, pending removals, link code) | RK `LicenseController`, `PendingLicenseRemovalQueue`, `requestWindowsLinkCode` |
   | ProBridge 126 + ProBridgeClient (about 80) | `ContinuityBridgeClient` |
   | ProAccessStore 57 | `AccessCache` |
   | AccountSessionStore 533 | `RightKitAccount` |
   | Mac CloudSyncNotifications 610 | `CloudKitWakeHost` |
   | Mac AppleCredentials main 309 (protocol part) | `AppleCredentialHelperHost` |

3. **Create `RightKitDictation`**, moving the command plane and snapshot first (they already sit on `RightKitStore`), then the coordinator and transcriber, then the audio session and policy.
4. **Create `RightKitLibrary`**, store and library first, then the cloud adapter, then the history view.
5. **Watch recording** (C), after `AudioFileRecorder` is in use.
6. **Overlay**: add items 1-5 to `RightKitOverlay`, switch `HeardRightPillPanel` to `RightKitOverlayPanel`, then move model and views; decide the splash owner.
7. **Polish Swift target** (D) and the remaining UI decisions (onboarding, settings, Live Activity views).
8. **Tests**: move each module's unit-level smokes into the RK test target as its code moves; the app keeps journeys.

## 6. LIVE to MAIN changes in Swift

### Deliberate

- Adoption of RightKitSwift: pin `rightkit-swift` 0.1.1 → `Orthic-Labs/rightkit` 0.8.1 with 13 products (section 1). **[read]**
- ASR runtime (14 files, 2,322 lines) deleted in favour of `RightKitSpeech` engines; `SherpaKeywordCommandDetector` replaced by `KeywordCommandDetector` + `RustKWSBridge`. **[read]**
- `Shared/LicenseEntitlement.swift` 591 → 194 and `HeardRight/LicenseController.swift` 723 → 346: verification and HTTP moved to `RightKitLicense`. **[read]**
- Cloud sync: four files / 1,420 lines → two / 553 lines on `RightKitCloudSync`; the Mac `HeardRightCloudSync` CLI (1,124 lines) replaced by `rightkit-cloudkit-helper`. **[read]**
- `Shared/RecordingActivityStateMachine.swift` (87) replaced by `RightKitLiveActivity`; `Shared/SharedDictationCommand.swift` 321 → 142 on `AppGroupCommandQueue`; the bounded-wait file lock moved to `RightKitStore.AppGroupFileLock` (the same non-blocking `flock` loop: LIVE `SharedDictationCommand.swift:93-133`, RK `AppGroupFileLock.swift:14,55-73`); `HeardRightShare/ShareViewController.swift` 157 → 53 on `RightKitImportQueue`. **[read]**
- Mac helper Keychain code replaced by `RightKitAppleCredentials` (`HeardRightAppleCredentials/main.swift` diff at `:161-196`). **[read]**
- Pill: joins full-screen Spaces (`PillPanel.swift:208-213`, comment cites issue 162; LIVE said "Intentionally no FullScreenAuxiliary"); hover watchdog stops when hidden (`:57-60,104-106`); splash stall recovery with four actions (`SplashPanel.swift:27-45,462-475`, issue 117); error expiry bound to an error generation and lengthened from 2.5 s to 5 s (`PillModel.swift:580-587`, `hr_pill.h:23-26`); error label taken from the event instead of the fixed "Failed" (`PillModel.swift:235`); CloudKit is not constructed when the build lacks the entitlement (`CloudSyncNotifications.swift:11-24,41-44,75-85`). **[read]**
- Pill deployment target macOS 13 → 14 (matches RK's `.macOS(.v14)`). **[read]**
- New features: `HardwareRecordingControls` (492), `PendingImportProcessor` (205), `WatchPendingStore`, `WatchRecordingRecovery`, `WatchPendingPolicy`, `WatchRecordingState`, iPad screenshot tests, `Packages/SharedModelRoot`. Growth in `RecordingSystemSurface` (697 → 1192), `RecordingAudioSession` (1633 → 1829), `WarmAudioCapture` (596 → 772), `SharedTranscriptStore` (645 → 919), `WatchRecordingTransfer` (387 → 704), `WatchRecordingController` (533 → 754), `RecordingActivityViews` (576 → 765). Contents of the growth **not verified**.
- Test-only environment switches removed from the pill (`HR_APP_DATA_DIR` in `PillModel.swift:147`, `HR_NATIVE_SNAPSHOT_DIR` in `AuxiliaryViews.swift:272`). The Rust side no longer references `HR_APP_DATA_DIR` outside `qa-e2e`, so the two sides agree. **[grep]**

### Look like regressions or losses (case-by-case check needed)

- **All pill Swift tests deleted**: `PillModelTests.swift` (1,195) and `SplashStartupTests.swift` (142), and the test target removed from `Package.swift`. This was a recorded decision (`docs/results/2026-11-02-test-reset/TEST-RESET.md`: "small tests removed, journeys are the coverage"; "Nothing was compiled or run"). Deliberate, but it removes the only automated check of pill state logic at the moment that logic is about to move into RightKit. **[read]**
- **iOS smoke tests deleted without a visible replacement in the app**: `parakeet_beam_bias_smoke`, `parakeet_runtime_smoke`, `whisper_*_smoke` (cover moved to RK, reasonable), `app_group_lock_timeout_smoke` (153) and `live_activity_state_machine_smoke` (33). RK has test targets for Store and LiveActivity; whether they cover the same cases is **not verified**.
- **`Shared/MobileTelemetryUploader.swift` (377) deleted with no reference left in MAIN.** LIVE's header says App Store builds never upload, so this may be intended, but telemetry upload from development builds is gone and `RightKitDiagnostics`' uploader option is not wired. **[grep] [suspicion]**
- **A second `CoreMLModelValidator`** was added in `Packages/SharedModelRoot` while the first remains and RK has a third. This is new duplication, the opposite of the goal. **[grep]**
- **MAIN still hand-writes code that RightKit took over from it** (`CloudSyncNotifications.swift`, the credentials helper's wire protocol, `DogfoodTraceStore`). Not a behaviour regression, but each is now two copies that can drift; MAIN's `CloudSyncNotifications` gained an entitlement check after RK's extraction that RK implements separately.
- **Two Swift pins in one repo** (0.8.1 and 0.9.0). **[read]**
- Behaviour parity of the adopted paths (license refresh ordering, share intake, cloud sync conflict handling) against LIVE was **not verified**; I compared structure, not behaviour.

## 7. Verdict per existing RightKit Swift module

| Module | Verdict | Notes |
|---|---|---|
| RightKitStore | right shape for HeardRight | Command queue, owner lease, revisioned state and file lock are all in use in MAIN |
| RightKitImportQueue | right shape for HeardRight | In use by the share extension and the app |
| RightKitCloudSync | right shape with listed gaps | iOS engine in use. Gap is on HeardRight's side: Mac `CloudKitWakeHost` not adopted |
| RightKitLicense | right shape with listed gaps | Client and store in use. Gaps on HeardRight's side: own controller, removal queue, link-code call, bridge client; iOS pin 0.8.1 |
| RightKitPurchases | right shape for HeardRight | `PurchaseController` in use; `ProAccessStore` still to fold into `AccessCache` |
| RightKitCredentials | right shape for HeardRight | Used by license and account code |
| RightKitAppleCredentials | right shape with listed gaps | Store in use on iOS and Mac. Gap on HeardRight's side: `AppleCredentialHelperHost` not adopted; iOS facade (300 lines) not checked line by line |
| RightKitSpeech | right shape with listed gaps | Engines in use. Model grant, validator and download owner exist but are unused. RK gaps: ANE cache maintenance, Files-import install path, download telemetry hook, keyword spotting |
| RightKitAudio | right shape with listed gaps | Not linked by HeardRight at all. RK gaps: in-memory accumulation, speech-activity snapshot, WAV writer; session policy richer than one category/mode/options (suspicion) |
| RightKitLiveActivity | right shape for HeardRight (lifecycle) | In use. It is not a home for the Live Activity views mapped to it |
| RightKitWatchBridge | right shape with listed gaps | Ledger and senders in use. No `WCSessionDelegate` halves, no recovery store |
| RightKitDiagnostics | right shape with listed gaps | Only `PrivacyRedactor` in use. Gaps: non-string field values, DEBUG campaign file, MetricKit subscriber |
| RightKitIntents | right shape with listed gaps | Driver, Darwin post and permission handshake in use. `dispatch` unused; the module is a seam, not a home for 855 lines of intents |
| RightKitUI | right shape for what it is; wrong shape as the target for onboarding and app scaffold | 230 lines of glass and contrast; nothing for onboarding, settings or navigation |
| RightKitAccount | not verified in depth; type names match HeardRight's `AccountSessionStore.swift` | Candidate ADOPT that the mapping sent to `rightkit-mesh` |
| RightKitOverlay | right shape with listed gaps (panel layer only) | Panel, key policy, collection behaviour and event names fit. Missing: level, done, hover state, hosting view, anchored animated resize, C interface, pill content, splash |
| RightKitWatch | wrong shape | FSEvents file watcher. No Apple Watch code belongs here |
| RightKitHubQA | wrong shape | Desktop Hub fakes. No iOS tests belong here |
| RightKitArtifacts | not verified | Reached only through `RightKitSpeech.ModelInstaller` |
| RightKitLLM, RightKitUpdate | not verified | Possible owners for `BYOKPolishClient` and `AppUpdateController` |

## What was not done

- Nothing was compiled or run, on either side.
- Function bodies were compared in full only for: the model grant client, the license facade, the pro bridge client, `RightKitIntents`, `RightKitOverlay`, the pill panel/bridge/model, and the LIVE-to-MAIN pill diffs. Elsewhere the evidence is declarations, call sites and header comments.
- ScrapeRight, Pulse and CodeRight were not read, so "shared with another app" claims in the plan are untested here.
- The contents of RightKit at the 0.8.1 and 0.9.0 tags were inferred from `CHANGELOG.md`, not from the tags.
