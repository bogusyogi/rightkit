# GenRight RightKit cutover matrix

Reviewed source HEAD `1d76671b560d35df3441bba51cf39f25f35105ee` at `/Volumes/D/claude/genright`; targets: macOS & Windows Tauri desktop.

Counts: **Y 25 · N 5 · M 1 · NA 66** across all 97 catalog components.

## Active shared routes

Y rows cover Cargo browser, control, db, fs, http, jobs, logs, media, ort, process, qa, qts, qts-ggml, qts-ggml-sys, secrets & settings, tts; npm app-shell, media-ui, onboarding, legal-ui, platform-ui, release, settings-ui & theme. Cargo rows still use local `[patch.crates-io]` paths, recorded as delivery flags.

## Cutover work

- **ASR:** route whisper workflow through `rightkit-asr`; preserve caption policy, remove local runner/parser after parity.
- **Audio:** replace compose narration/music filters with `rightkit-audio` primitives; retain video, subtitle & loudness policy.
- **Models (mixed):** migrate Whisper artifacts into `rightkit-models`; remove `asr_provision`'s separate manifest/download/checksum lifecycle.
- **Engine framing:** adopt `rightkit-framed-sidecar` while preserving `EngineFrame`; remove local NDJSON framing.
- **Shell:** adopt `rightkit-shell` for Tauri plumbing; preserve GenRight commands & Motion protocol.
- **Transcript:** use `rightkit-transcript` for SRT/VTT parsing/export & timing; preserve caption policy.
- **Legal UI:** shared `LegalGate` is wired through onboarding; populate GenRight legal documents before release qualification.

All Swift components are NA because GenRight has no SwiftPM/Xcode/iOS/watchOS target. Full 97-row evidence & searches are in [genright.json](./genright.json).
