# HeardRight

Fresh source audit at `b96da5f26b740698fbb58919f905607419fec3b0` covers `tauri-app-next` macOS/Windows desktop, `ios-native` iOS/iPadOS, Watch, widgets, App Intents, QA & release scripts. Generated/build/cache/vendor/history paths excluded; no builds or runtime qualification. All 97 catalog IDs appear once in [heardright.json](./heardright.json).

Result: **31 Y, 4 M, 17 N, 45 NA**.

Y routes:

- Cargo: `rightkit-audio`, `rightkit-crash`, `rightkit-logs`, `rightkit-process`, `rightkit-service`, `rightkit-speech-client`, `rightkit-tauri`.
- npm: `@rightkit/app-shell`, `@rightkit/legal`, `@rightkit/legal-ui`, `@rightkit/license`, `@rightkit/logs`, `@rightkit/onboarding`, `@rightkit/qa-chrome`, `@rightkit/release`, `@rightkit/settings-ui`, `@rightkit/tauri`, `@rightkit/theme`, `@rightkit/updates`.
- Swift: `RightKitLicense`, `RightKitDiagnostics`, `RightKitPurchases`, `RightKitCloudSync`, `RightKitStore`, `RightKitImportQueue`, `RightKitLiveActivity`, `RightKitWatchBridge`, `RightKitSpeech`, `RightKitIntents`, `RightKitAppleCredentials`, `RightKitUI`.

M overlap:

- `cargo:rightkit-asr`: HeardRight re-exports shared beam/scoring/policy APIs while local `CoreMlParakeet` owns decode/display & command alignment. Move full engine callers to `rightkit_asr::CoreMlParakeet`; retain app command/display policy.
- `cargo:rightkit-license`: shared `verify_token` is live while local `verify_lease_envelope_signature` duplicates shared `verify_lease_envelope`. Replace verifier; retain endpoint/device-lease policy.
- `cargo:rightkit-models`: shared `sha256_file` is live while local `ensure_model`/`download_and_verify` owns model download, manifest, checksum & activation. Replace local lifecycle with `rightkit_models` store/download APIs.
- `npm:@rightkit/platform-ui`: AppShell reaches shared caption/platform controls while local `usePlatform` & `Kbd.formatChord` remain active. Route local platform/chord helpers through package APIs.

N local gaps:

- Desktop: `rightkit-align`, `rightkit-control`, `rightkit-db`, `rightkit-fs`, `rightkit-hotkeys`, `rightkit-http`, `rightkit-ort`, `rightkit-overlay`, `rightkit-secrets`, `rightkit-settings`, `rightkit-sync`, `rightkit-transcript`, `rightkit-vad`, `rightkit-shell`, & `@rightkit/shell` remain behind live local callers.
- QA: `@rightkit/qa` remains behind local CDP driver/journey/cleanup code.
- Native: `RightKitArtifacts` remains behind `ModelArtifactInstaller` install/recovery calls.

Each N/M row names production caller, matching RightKit owner API, & concrete cutover task. Swift census checked diagnostics, model installs, share queue, OSLog traces, Keychain/SecItem paths, Watch target, extensions, widgets, & App Intents.
