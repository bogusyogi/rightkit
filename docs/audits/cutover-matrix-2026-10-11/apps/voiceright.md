
# VoiceRight cutover summary

Fresh source/config audit at commit c42cf611f04355e666523ae7d3e6c2a9210a4cea; targets: Tauri 2 + React 19 desktop on Windows/macOS. No Swift, iOS, watchOS, or Xcode target exists.

Counts: Y=1, N=13, M=0, NA=83 (97 catalog rows).

Active shared adoption: npm:@rightkit/release is Y because right-release.config.mjs:4 imports @rightkit/release/cargo-target.mjs, while package.json:26-37 invokes release build/publish/dependency commands.

Largest gaps are local QTS/ORT/model/WAV stack, Tauri shell, TTS adapter, artifact filesystem, React shell/media/platform controls. Each remains N until shared package adoption has a real production caller.

Applicable rows and migration tasks:
- cargo:rightkit-audio — N: Local WAV validation/write overlaps shared WAV I/O; no RightKit route. Task: Route WAV parsing/writing through rightkit-audio or rightkit-tts; preserve sample-rate/export checks.
- cargo:rightkit-fs — N: Local app-data/staged/generated/voice writes overlap shared state/file safety; no RightKit route. Task: Adopt rightkit-fs for app-data/staged/generated/voice writes; remove direct std::fs after parity.
- cargo:rightkit-models — N: Local QTS model/ORT bundle provisioning overlaps shared model installation; no RightKit route. Task: Adopt rightkit-models manifest/verification/activation for QTS model and ORT provisioning.
- cargo:rightkit-ort — N: Direct ort and local ORT setup implement shared runtime concern; no RightKit route. Task: Route ORT setup through rightkit-ort; remove direct ort and local env/path setup.
- cargo:rightkit-qts — N: Embedded engine calls local-path qts despite matching rightkit-qts APIs. Task: Replace eval/qts-upstream with rightkit-qts; preserve embedded and voice-prompt contracts.
- cargo:rightkit-qts-ggml — N: Active local qts dependency brings qts_ggml transitively; no RightKit GGML route. Task: After qts adoption, remove local qts_ggml path and verify Vulkan/CPU behavior.
- cargo:rightkit-qts-ggml-sys — N: Active local qts dependency brings qts_ggml_sys transitively; no RightKit GGML FFI route. Task: After qts adoption, remove local qts_ggml_sys path and verify native linkage.
- cargo:rightkit-shell — N: Local Tauri builder/config own shared shell concerns; no rightkit-shell route. Task: Adopt rightkit-shell for Tauri shell wiring; keep product commands local.
- cargo:rightkit-tts — N: Local Rust QTS adapter owns shared TTS concerns; no rightkit-tts route. Task: Adopt rightkit-tts or document retained product-specific adapter; remove duplicate path when clone is covered.
- npm:@rightkit/app-shell — N: Local React frame implements app-shell layout; no shared app-shell import. Task: Adopt app-shell primitives; preserve VoiceRight compose/voice-library/result policy.
- npm:@rightkit/media-ui — N: Local HTML audio preview implements media playback; no media-ui import. Task: Adopt media-ui player for WAV preview; preserve output/export controls.
- npm:@rightkit/platform-ui — N: Local platform detection/window controls implement shared platform UI. Task: Adopt platform-ui detection/window controls; preserve titlebar styling.
- npm:@rightkit/release — Y: Release package is actively used by production release config/scripts. Task: none
- npm:@rightkit/shell — N: Live titlebar controls directly minimize, toggle maximization, & close like createShell().window. Task: Adopt shared shell window controls; preserve product engine invokes.

Delivery facts: QTS local path dependency is src-tauri/Cargo.toml:16; ORT direct is :15; @rightkit/release pinned 0.2.109. Tauri updater config has no runtime JS check/download caller; Vitest/query mocks stay product-local.

Evidence detail, all 97 IDs exactly once, and searches are in voiceright.json.

