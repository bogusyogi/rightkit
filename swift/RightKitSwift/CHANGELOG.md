# RightKitSwift changelog

## 0.6.0

Gap ledger #75 (HeardRight fit review). The Diagnostics additions are source compatible. The Artifacts install API changes, see below.

- `RightKitDiagnostics`: `StoreOptions` (`fileProtection`, `redaction`, `maxBytes`, `retainBytes`, `uploader`) and `JSONLDiagnosticStore(fileURL:options:now:)`. The pre-0.6.0 initializers still work and keep their defaults.
- `RedactionPolicy`: `.standard` (default, unchanged), `.fields([String])`, `.custom((key, value) -> String)` and `.none`. The event name is screened unless the policy is `.none`.
- `fileProtection` applies a data-protection class to the log and its lock file after each write. The attribute write is best-effort; its errors are ignored. Not exercised on device in 0.6.0.
- Trimming keeps whole records only; `retainBytes` sets the trim target (default half of `maxBytes`), clamped to `maxBytes`.
- `append(_:) async -> Bool` runs the blocking record off the caller's executor.
- `drain() async throws -> Int` seals the active log under the record lock, uploads each sealed batch oldest first through `uploader`, and deletes a batch only after its upload succeeds. Delivery is at-least-once. A concurrent drain fails with `DiagnosticStoreError.drainInProgress`.
- `RightKitArtifacts`: `ArtifactInstaller.install(archive:descriptor:destinationRoot:options:)` is now `async` and takes `InstallOptions`.
- A nil `expectedSHA256` throws `ArtifactError.missingExpectedDigest` before the archive is read or the destination is created. Set `InstallOptions.allowUnverified` to install without a digest.
- The previous install is kept aside (`.aside-<name>`) until the new one passes `InstallOptions.postActivationValidation`, a hook that receives the activated directory. A throwing hook restores the previous install.
- `InstallOptions.requiredFiles` are checked before activation. Paths must be relative and free of `..`.
- Every install writes `.rightkit-manifest.json` into the installed directory (`version`, `archiveSHA256` when verified, and every file's path, size and SHA-256). Read it with `ArtifactInstaller.readManifest(destinationRoot:topLevelDirectory:)`.
- `ArtifactInstaller.recoverInterruptedInstall(destinationRoot:topLevelDirectory:postActivationValidation:)` resolves an interrupted activation. An unvalidated activation is rolled back, or completed when a validator passes. Installs call it first. Call it on launch.
- `ArtifactDescriptor` gains `version: String?` (default nil).
- New `ArtifactError` cases: `missingExpectedDigest`, `missingRequiredFile`, `invalidRequiredFilePath`, `postActivationValidationFailed`, `installInProgress`, `manifestUnreadable`, `recoveryMarkerUnreadable`.

Source-visible for apps: add `await` to `ArtifactInstaller.install` calls. Pass `InstallOptions(allowUnverified: true)` where a nil digest was intentional. A `try ArtifactInstaller.install` inside a synchronous function must move into an async context.

Verified: `swift test --no-parallel` passes (RightKitDiagnosticsTests 20 cases, RightKitArtifactsTests 24 cases, 0 warnings); `xcodebuild` for the iOS Simulator builds RightKitDiagnostics and RightKitArtifacts with BUILD SUCCEEDED.

## 0.5.0

Gap ledger #63 (Pulse port). All additions are source compatible.

- `RightKitAppleCredentials`: `ForeignGenericPassword.read(service:account:policy:)` reads another app's generic-password item from the legacy file Keychain, newest by modification date, by persistent reference, with a typed `ReadError` and a `.noPrompt` default policy; `matches(service:account:)` lists candidates. Read-only: no write or delete. macOS only.
- `RightKitOverlay`: `RightKitOverlayPanel.setClickThrough(_:)`, `hitRegions: [CGRect]` and `updateClickThrough(forMouseAt:)` toggle `ignoresMouseEvents` per rectangle; `RightKitOverlayHitRegions` is the pure rule. The default panel is unchanged (interactive).
- New product `RightKitWatch`: `RightKitWatcher` (FSEvents) delivers typed `item`, `rootChanged`, `historyDone` and `mustRescan` events as an `AsyncStream`, with `RightKitWatchEventID` (`Codable`) and `lastEventId` for replay. macOS only; the target is empty on iOS.

Source-visible for apps: `.noPrompt` uses `SecKeychainSetUserInteractionAllowed(false)` (process-wide, resolved with `dlsym`) because the Skip/`LAContext` settings do not stop legacy ACL dialogs; see the README.

## 0.4.0

Closure of the 2026-10-10 Apple audit (`docs/audits/fresh-2026-10-10/evidence/apple-closure.md`, "Closure 2026-10-10").

- Legacy `SFSpeechRecognizer` paths use only on-device-capable recognizers, force on-device recognition, and fail closed otherwise (A01).
- Import staging copies through a byte-counted hidden partial file with an atomic rename; partials are removed on failure (A02).
- Diagnostics log trims and appends under a sibling lock file; torn writes start a new line (A03).
- Live dictation rechecks its generation after each awaited permission before emitting a failure (A04).
- Legacy file transcription cancels its recognition task and resumes exactly once (A05).
- `BoundedStringLog` is atomic within a process; cross-process writers to one key are documented as unsupported (A06).
- `LicenseClient.platform()` is async with a `@MainActor` helper; the Keychain base query uses `LAContext.interactionNotAllowed` (A09).
- CloudSync subscription retry no longer retains the engine across awaits; `deinit` cancels it (A10).
- License removal queue read-modify-write is exclusive via `transact(forKey:)`; `UserDefaultsLicenseDeviceState(defaults:lockFileURL:)` adds a cross-process lock.
- `RightKitOverlayKind` cases are `pill` and `notch` (JSON raw values unchanged; deprecated aliases kept).
- 0.3.8 (unpublished): `probeStatus` and `deleteAnyScope` on RightKitAppleCredentials.

Source-visible for apps: pass `lockFileURL` when an app shares a suite with an extension; `enqueue` returns a discardable `Bool`; a public Live Activity attributes type must be `Sendable`; a `switch` over `RightKitOverlayKind` needs `default`.

Verified: macOS `swift test --no-parallel` 215 Swift Testing cases (213 pass, 2 skipped) + 21 XCTest (20 pass, 1 skipped); iOS Simulator build 0 warnings; StoreKit 8/8 on iPhone 17 / iOS 26.0; archive helper 5/5; CloudKit helper builds.
