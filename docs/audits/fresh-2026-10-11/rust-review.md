# Rust dependency review — 2026-10-11

Scope: 57 workspace crates + 10 secondary Cargo packages; 445 declared dependency edges. No Cargo, rustc, builds, tests, network, or installs ran.

## Findings

One direct-edge candidate met manifest + Rust/build evidence. Full inventory includes targets, dev/build roles, optionality, renames, features, workspace inheritance, intent & first source evidence in `rust-dependency-edges.json`. Token evidence needs target-aware compilation before deletion.

### RK-RUST-KEEP-001: windows-core

- Manifest: `crates/rightkit-audio/Cargo.toml:44`; scope: target cfg(windows), optional capture feature; status: keep.
- Proof: `capture` enables it at `crates/rightkit-audio/Cargo.toml:18`. `#[implement(IMMNotificationClient)]` at `crates/rightkit-audio/src/lifecycle.rs:198` expands through locked `windows-implement 0.60.2`; current generator emits `::windows_core::IUnknownImpl` at `/Users/adrdsouza/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/windows-implement-0.60.2/src/gen.rs:182-184`. Lock anchors: `Cargo.lock:8338-8347`, `Cargo.lock:8373-8381`.
- Decision: retain direct edge; source-token absence does not cover proc-macro output.

### RK-RUST-001: rightkit-service

- Manifest: `desktop-app-template/src-tauri/Cargo.toml:41`; scope: template production dependency; confidence: high.
- Proof: Shell imports `rightapp_protocol::app_data_root` at `desktop-app-template/src-tauri/src/lib.rs:12`; diagnostics does same at `desktop-app-template/src-tauri/src/diagnostics.rs:8`. Protocol owns service at `desktop-app-template/crates/protocol/Cargo.toml:11` & resolves `Suite` at `desktop-app-template/crates/protocol/src/lib.rs:7,27-32`.
- Validation: Delete direct template edge only if protocol remains API boundary; compile template on supported targets before accepting.

## Package coverage

| Package | Manifest | scope | intent | edges | evidence |
|---|---|---:|---|---:|---|
| agent-room-core | `crates/agent-room-core/Cargo.toml` | workspace | private | 10 | source, test |
| rightkit-agent-tools | `crates/rightkit-agent-tools/Cargo.toml` | workspace | private | 7 | source, test |
| rightkit-align | `crates/rightkit-align/Cargo.toml` | workspace | private | 3 | source, test |
| rightkit-asr | `crates/rightkit-asr/Cargo.toml` | workspace | public-explicit | 17 | source, test |
| rightkit-audio | `crates/rightkit-audio/Cargo.toml` | workspace | public-implicit | 14 | source, test |
| rightkit-automation | `crates/rightkit-automation/Cargo.toml` | workspace | private | 4 | source, test, example |
| rightkit-browser | `crates/rightkit-browser/Cargo.toml` | workspace | public-implicit | 10 | source |
| rightkit-cache | `crates/rightkit-cache/Cargo.toml` | workspace | public-implicit | 3 | source, test |
| rightkit-channel-memory | `crates/rightkit-channel-memory/Cargo.toml` | workspace | private | 3 | source |
| rightkit-compose | `crates/rightkit-compose/Cargo.toml` | workspace | private | 3 | source |
| rightkit-contracts | `crates/rightkit-contracts/Cargo.toml` | workspace | private | 3 | source |
| rightkit-control | `crates/rightkit-control/Cargo.toml` | workspace | public-implicit | 7 | example, source |
| control-test-app | `crates/rightkit-control/examples/control-test-app/Cargo.toml` | secondary | private | 3 | build, example |
| rightkit-crash | `crates/rightkit-crash/Cargo.toml` | workspace | public-implicit | 6 | source |
| rightkit-daemon | `crates/rightkit-daemon/Cargo.toml` | workspace | private | 21 | source, bench, test |
| rightkit-db | `crates/rightkit-db/Cargo.toml` | workspace | public-implicit | 6 | source, test |
| rightkit-embed | `crates/rightkit-embed/Cargo.toml` | workspace | public-implicit | 9 | source |
| rightkit-framed-sidecar | `crates/rightkit-framed-sidecar/Cargo.toml` | workspace | public-implicit | 5 | source |
| rightkit-fs | `crates/rightkit-fs/Cargo.toml` | workspace | public-implicit | 6 | source |
| rightkit-fsindex | `crates/rightkit-fsindex/Cargo.toml` | workspace | public-explicit | 4 | source, bench |
| rightkit-hotkeys | `crates/rightkit-hotkeys/Cargo.toml` | workspace | public-implicit | 1 | source |
| rightkit-http | `crates/rightkit-http/Cargo.toml` | workspace | public-implicit | 13 | source |
| rightkit-jobs | `crates/rightkit-jobs/Cargo.toml` | workspace | public-implicit | 11 | source, example, test |
| rightkit-license | `crates/rightkit-license/Cargo.toml` | workspace | public-implicit | 4 | source |
| rightkit-llm | `crates/rightkit-llm/Cargo.toml` | workspace | public-implicit | 5 | source |
| rightkit-logs | `crates/rightkit-logs/Cargo.toml` | workspace | public-implicit | 10 | source |
| rightkit-mcp | `crates/rightkit-mcp/Cargo.toml` | workspace | public-implicit | 12 | source, example, test |
| rightkit-media | `crates/rightkit-media/Cargo.toml` | workspace | public-implicit | 5 | source |
| rightkit-models | `crates/rightkit-models/Cargo.toml` | workspace | public-implicit | 17 | source, test |
| rightkit-oauth | `crates/rightkit-oauth/Cargo.toml` | workspace | public-implicit | 16 | example, source, test |
| rightkit-ocr | `crates/rightkit-ocr/Cargo.toml` | workspace | public-explicit | 12 | source, test |
| rightkit-ort | `crates/rightkit-ort/Cargo.toml` | workspace | public-implicit | 8 | source, test |
| rightkit-overlay | `crates/rightkit-overlay/Cargo.toml` | workspace | public-implicit | 1 | source |
| rightkit-ownership | `crates/rightkit-ownership/Cargo.toml` | workspace | public-explicit | 12 | source |
| rightkit-pdf | `crates/rightkit-pdf/Cargo.toml` | workspace | public-implicit | 10 | source |
| rightkit-playback | `crates/rightkit-playback/Cargo.toml` | workspace | public-implicit | 4 | source |
| rightkit-process | `crates/rightkit-process/Cargo.toml` | workspace | public-implicit | 3 | source, test |
| rightkit-qa | `crates/rightkit-qa/Cargo.toml` | workspace | public-implicit | 13 | source |
| rightkit-qts-ggml-sys | `crates/rightkit-qts-ggml-sys/Cargo.toml` | workspace | public-implicit | 2 | generated, source |
| rightkit-qts-ggml | `crates/rightkit-qts-ggml/Cargo.toml` | workspace | public-implicit | 1 | source |
| rightkit-qts | `crates/rightkit-qts/Cargo.toml` | workspace | public-implicit | 7 | source |
| rightkit-record | `crates/rightkit-record/Cargo.toml` | workspace | private | 9 | source |
| rightkit-review | `crates/rightkit-review/Cargo.toml` | workspace | public-implicit | 2 | source |
| rightkit-sandbox | `crates/rightkit-sandbox/Cargo.toml` | workspace | public-implicit | 4 | source |
| rightkit-scenario | `crates/rightkit-scenario/Cargo.toml` | workspace | public-implicit | 4 | source |
| rightkit-search | `crates/rightkit-search/Cargo.toml` | workspace | public-implicit | 3 | source, test |
| rightkit-secrets | `crates/rightkit-secrets/Cargo.toml` | workspace | public-implicit | 10 | source, test |
| rightkit-send | `crates/rightkit-send/Cargo.toml` | workspace | private | 7 | source |
| rightkit-service | `crates/rightkit-service/Cargo.toml` | workspace | public-implicit | 6 | source, example |
| rightkit-settings | `crates/rightkit-settings/Cargo.toml` | workspace | public-implicit | 5 | source, test |
| rightkit-shell | `crates/rightkit-shell/Cargo.toml` | workspace | public-implicit | 17 | source, test |
| shell-example-app | `crates/rightkit-shell/example-app/Cargo.toml` | secondary | private | 9 | build, source, test |
| rightkit-speech-client | `crates/rightkit-speech-client/Cargo.toml` | workspace | public-implicit | 7 | source, example, test |
| rightkit-sync | `crates/rightkit-sync/Cargo.toml` | workspace | public-implicit | 3 | source, bench |
| rightkit-tauri | `crates/rightkit-tauri/Cargo.toml` | workspace | public-implicit | 2 | source |
| rightkit-transcript | `crates/rightkit-transcript/Cargo.toml` | workspace | public-explicit | 3 | source |
| rightkit-tts | `crates/rightkit-tts/Cargo.toml` | workspace | public-explicit | 9 | source |
| rightkit-vad | `crates/rightkit-vad/Cargo.toml` | workspace | public-explicit | 3 | source, test |
| rightkit-watch | `crates/rightkit-watch/Cargo.toml` | workspace | public-implicit | 5 | source, bench |
| rightapp-engine | `desktop-app-template/crates/engine/Cargo.toml` | secondary | private | 3 | source |
| rightapp-protocol | `desktop-app-template/crates/protocol/Cargo.toml` | secondary | private | 2 | source |
| rightapp-tools | `desktop-app-template/crates/tools/Cargo.toml` | secondary | private | 1 | source |
| rightapp | `desktop-app-template/src-tauri/Cargo.toml` | secondary | private | 14 | build, source |
| rightkit-managed-agent-probe | `docs/plans/evidence/rightkit-build-control-plane-v5/windows/managed-agent-fixture/Cargo.toml` | secondary | public-implicit | 0 | none |
| rk2-fixture | `spikes/rk2-macos/fixture-app/Cargo.toml` | secondary | private | 3 | build, source |
| rk2-svc-call | `spikes/rk2-macos/svc-call/Cargo.toml` | secondary | private | 2 | example |
| rk2-rs-probe | `spikes/rk2-windows/rs-probe/Cargo.toml` | secondary | private | 1 | source |

## Feature review

Feature-heavy resolver/consumer review candidates: `rightkit-agent-tools` (5; crates/rightkit-agent-tools/Cargo.toml), `rightkit-asr` (18; crates/rightkit-asr/Cargo.toml), `rightkit-control` (5; crates/rightkit-control/Cargo.toml), `rightkit-db` (7; crates/rightkit-db/Cargo.toml), `rightkit-fs` (5; crates/rightkit-fs/Cargo.toml), `rightkit-http` (5; crates/rightkit-http/Cargo.toml), `rightkit-oauth` (5; crates/rightkit-oauth/Cargo.toml), `rightkit-ort` (28; crates/rightkit-ort/Cargo.toml), `rightkit-pdf` (5; crates/rightkit-pdf/Cargo.toml), `rightkit-qts-ggml-sys` (6; crates/rightkit-qts-ggml-sys/Cargo.toml), `rightkit-qts-ggml` (5; crates/rightkit-qts-ggml/Cargo.toml), `rightkit-qts` (24; crates/rightkit-qts/Cargo.toml), `rightkit-shell` (5; crates/rightkit-shell/Cargo.toml), `rightkit-tts` (6; crates/rightkit-tts/Cargo.toml), `rightkit-vad` (5; crates/rightkit-vad/Cargo.toml). No binary-size claim follows from feature count.

## Method

- Parsed every current Cargo manifest, including target, dev, build, optional, renamed, path, workspace-inherited & feature declarations.
- Scanned Rust source, macro tokens, checked-in generated Rust, tests, examples, benches & build.rs; JSON retains first evidence location per edge.
- `publish = false` is private; `publish = true` is explicit public; omission is recorded as implicit public intent.


