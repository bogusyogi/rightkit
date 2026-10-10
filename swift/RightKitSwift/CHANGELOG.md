# RightKitSwift changelog

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
