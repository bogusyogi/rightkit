# RightKitSwift Mac extraction checklist

This is the Phase 7 execution gate. Do not publish or adopt the package until every evidence row passes on a Mac.

## 0. Preserve and baseline

- Record `git status --short` in both `claude` and `heardright`; stage only named Phase 7 paths.
- Confirm `swift --version`, `xcodebuild -version`, and `xcodegen --version` are available.
- From `heardright/ios-native`, run `scripts/local-smoke.sh`. Preserve its final `All local iOS smoke checks passed.` line.
- From `rightkit`, run `pnpm test:swift-contracts`. Preserve two passing Node tests.
- Do not modify or delete app-local source until the package replacement is compiled and HeardRight is green.

## 1. Create the package skeleton

- Create `rightkit/swift/RightKitSwift/Package.swift` with iOS 17 and macOS test support, library products matching the four targets in `README.md`, test targets, bundled `contracts/license-v2.json`, and `MIT OR Apache-2.0` licensing.
- Use Foundation, CryptoKit, Security, and native URLSession/FileManager APIs. Add no third-party dependency. If gzip/tar support needs a system-library target, keep it private to `RightKitArtifacts` and verify registry archive portability.
- Add a package test that loads the bundled vector, verifies its Ed25519 signature, and asserts app/device/tier/features/iat/exp.
- Gate: `swift package dump-package`, `swift test`, and `swift package archive-source` succeed from the package directory.

## 2. Extract licensing first

- Characterize `Shared/LicenseEntitlement.swift` and the licensing section of `Tests/main.swift` before movement: formatting, malformed/base64/key/signature/app/device/expiry/JSON failures, tier ranking, and feature checks.
- Extract generic value types and verifier. Inject app key, public key, endpoints, accepted key prefixes, app version, keychain namespace, and clock.
- Extract the HTTP client and Keychain store from `HeardRight/LicenseController.swift`; keep `LicenseController` as the app adapter.
- Correct the current hidden coupling during extraction: refresh errors must be returned/observable instead of `LicenseActivationClient.refresh` swallowing every error.
- Rewire HeardRight to import the package without changing messages, activation behavior, offline verified-token behavior, or Free/Pro product policy.
- Gate: package tests pass; `scripts/local-smoke.sh` passes; wrong app/device, expired, malformed, and tampered vectors fail closed.

## 3. Extract diagnostics and QA hooks

- Characterize identifier redaction and sorted JSON from `Shared/DiagnosticsReport.swift`, plus bounded JSONL behavior from `Shared/DogfoodTraceStore.swift`.
- Extract generic redaction, envelope/event encoding, bounded append, rotation, backup exclusion, and injected clock/storage root.
- Keep `HeardRightDiagnosticsReport`, its model/recording/rules/control snapshots, event names, app group, and UI copy in HeardRight.
- Provide fault-injection seams for time, URLSession, filesystem, keychain, download progress, install, and validation. Do not add a global QA mode to production behavior.
- Gate: redaction tests prove secrets, bearer/auth headers, URLs, file/audio paths, and user text never appear; interrupted/corrupt writes preserve the last valid log.

## 4. Extract artifact download/cache/install

- Characterize `ModelArtifactInstaller.swift`, `model_install_smoke.swift`, and generic download/retry/cancellation tests in `model_store_smoke.swift` before movement.
- Extract descriptor/manifest parsing, request construction, progress, retry classification, checksum streaming, backup exclusion, safe archive extraction, staging, atomic activation, cache pruning, and rollback.
- Keep `ASRModelOption`, HeardRight routes/digests/names, Core ML stage rules, `CoreMLModelValidator`, runtime warm/invalidation, `ModelDownloadStore` UI state/messages, and its `app.heardright.ios.model-download` identifier local.
- Add failure tests for checksum mismatch, traversal, absolute paths, symlink/hardlink members, gzip expansion cap, truncation, cancellation after download, validation failure, atomic-replace failure, and rollback to the prior valid artifact.
- Gate: no failed install can delete or expose a partially installed current artifact; retry never duplicates a completed install.

## 5. Integrate and prove HeardRight

- Add the package through a local path only while extracting inside the canonical RightKit workspace; do not commit an app dependency on that path.
- Run `scripts/local-smoke.sh`, then `scripts/release-preflight.sh` from `heardright/ios-native`.
- Generate the Xcode project from `project.yml`, build the simulator app, and run the existing HeardRight UI/device proof contracts. Native foreground/device interaction remains operator-approved only.
- Replace the temporary path dependency with an exact Swift package-registry version only after registry publication succeeds.
- Gate: the app contains no copied RightKit sources, behavior remains characterized, and all package/app tests pass.

## 6. Registry and clean-clone proof

- Publish only through the future `right-release publish swift` lane. It must run tests, archive-source, deterministic archive/hash comparison, secret scan, license/provenance checks, and registry dry-run before the real publish. Do not hand-run a registry publish.
- In a clean directory outside the Claude workspace, resolve the exact registry version, run `swift test`, and build the HeardRight integration without sibling paths or Git dependencies.
- Record package version, registry URL, archive SHA-256, resolved dependency metadata, test logs, and clean-clone path in the Phase 7 evidence bundle.
- Gate: `right-release doctor` rejects Git/path/branch/revision dependencies and non-exact registry versions before app adoption.

## 7. Template and ScrapeRight proof

- Create the XcodeGen iOS template separately from the package. It may contain signing placeholders, privacy manifest, dependency injection, accessibility defaults, diagnostics screen, test targets, and release verification scripts—never HeardRight branding or product behavior.
- Generate the ScrapeRight spike from the template; import a fixture, download a test artifact, verify checksum/install/rollback, emit redacted diagnostics, and pass simulator plus approved installed-device QA.
- Gate: the spike resolves RightKitSwift from the registry and contains no copied shared source, HeardRight identifiers, transcription behavior, keyboard, Live Activity, or StoreKit policy.

## Required evidence

| Evidence | Pass condition |
|---|---|
| Package | `dump-package`, tests, archive-source, deterministic hash, secret/license scans green |
| Licensing | shared vector passes; all negative vectors fail closed; refresh errors observable |
| Diagnostics | privacy redaction and bounded/recoverable JSONL tests green |
| Artifacts | checksum, safe extraction, atomic activation, rollback, retry/cancel tests green |
| HeardRight | local smoke, release preflight, simulator build, existing QA green |
| Portability | clean external clone resolves exact registry version; no path/Git dependency |
| Template/spike | XcodeGen output and ScrapeRight simulator/device proof green; no copied shared source |

Publication remains blocked until this Mac evidence exists. Windows contract parity alone is preparation, not a Swift release gate.
