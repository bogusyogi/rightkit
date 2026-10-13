# Cargo publication decision

Windows follow-through from `3107b2401ea4852a3316fa039b523424bcacc9f0`.
Publication runs through `right-release publish cargo` after exact-commit native host receipts, package inspection, clippy & registry dry-run pass.

## Previously disabled crates

| Crate | Decision | Release requirement |
| --- | --- | --- |
| agent-room-core | Keep private | UNLICENSED coordination implementation |
| rightkit-computer | Keep private | Retired; replaced by rightkit-automation |
| rightkit-browser | Public candidate | Native receipts; rightkit-process available |
| rightkit-cache | Public candidate | Native receipts |
| rightkit-control | Public candidate | Native receipts; rightkit-daemon available |
| rightkit-crash | Public candidate | Native receipts; new rightkit-logs API available |
| rightkit-pdf | Public candidate | Native receipts |
| rightkit-qa | Public candidate | Native receipts; control, service, framed-sidecar & browser available |
| rightkit-sandbox | Public candidate | Native receipts; versioned process dependency |
| rightkit-search | Public candidate | Native receipts |
| rightkit-daemon | Public candidate | Native receipts; rightkit-framed-sidecar available |
| rightkit-shell | Public candidate | Native receipts; service & jobs available; native shell evidence |
| rightkit-speech-client | Public candidate | Native receipts; service & framed-sidecar available |
| rightkit-automation | Public; hold | Dedicated Windows input session |
| rightkit-agent-tools | Public; hold | Automation & other optional registry dependencies available |
| rightkit-media | Public candidate | Approved FFmpeg catalog; current native receipts; process available |
| rightkit-playback | Public candidate | Media available; native remux & cancellation receipts |
| rightkit-audio | Public candidate | Media available; native audio receipts |
| rightkit-ort | Public; hold | Native runtime/artifact catalog gate |
| rightkit-embed | Public; hold | ORT gate |
| rightkit-ocr | Public; hold | Signed model/runtime catalogs & migrated adapter parity |
| rightkit-vad | Public; hold | ASR stack versioning & model/runtime gates |
| rightkit-asr | Public; hold | ASR/model parity gate; versioned VAD dependency |
| rightkit-transcript | Public; hold | Stable version & versioned ASR test dependency |
| rightkit-tts | Public; hold | Stable transcript/runtime versions & TTS native evidence |

`rightkit-hotkeys` remains outside ordinary-session test execution. Compile its tests without running them; collect input-suite receipts only in a dedicated session.

## Dependency order

For enabled candidates & their shared prerequisites:

1. `rightkit-fs`, `rightkit-framed-sidecar`, `rightkit-process`, `rightkit-logs`, `rightkit-cache`, `rightkit-pdf`, `rightkit-search`.
2. `rightkit-db`, `rightkit-daemon`, `rightkit-browser`, `rightkit-sandbox`, `rightkit-crash`, `rightkit-media`.
3. `rightkit-jobs`, `rightkit-control`, `rightkit-speech-client`, `rightkit-playback`, `rightkit-audio`.
4. `rightkit-shell`, `rightkit-qa`.

Every group waits for its own dependencies to exist in crates.io. Optional runtime dependencies also need published registry versions. Package dry-runs verify registry dependencies rather than sibling source.

Registry inspection found `rightkit-process 0.1.0` lacks APIs consumed by jobs, & `rightkit-logs 0.1.0` lacks APIs consumed by crash. Stage both expanded APIs at `0.2.0`, with matching internal path dependency pins. Keep adopted consumer pins until publication is verified.

Source inventory: `crates/*/Cargo.toml`; publication policy: `packages/release/publication-gate.mjs`; staged versions: `packages/release/rightkit-versions.json`.
