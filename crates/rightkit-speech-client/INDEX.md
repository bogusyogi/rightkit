# rightkit-speech-client: file index

Version 0.3.4. Start with [README.md](README.md) for scope, then [API.md](API.md) for every
public item and the host's input contract. The repository-wide index is the generated
`INDEX.md` at the repository root.

## Package

| Path | What it is |
|---|---|
| `Cargo.toml` | Manifest. Version 0.3.4. `include` lists the files that ship. |
| `README.md` | Scope, platform support, and the contract-extension table. |
| `API.md` | Public API reference: purpose and errors for each item, the input contract, and known divergences. |
| `CHANGELOG.md` | Release notes, newest first. |
| `LICENSE-MIT`, `LICENSE-APACHE` | Licences. |
| `rightkit-ownership.json` | Ownership fingerprints for the shared speech-service capability. |

## Source (`src/`)

| File | What it holds |
|---|---|
| `lib.rs` | The client: discovery, `connect`, `SpeechClient`, transcription, the launch policy (`Launch`, `HostLauncher`, `HeardRightLauncher`, `transcribe_via_service_policy`), and cancel (`cancel_request`, `CancelOutcome`). |
| `protocol.rs` | Wire types and frame constructors: `ServiceEnvelope`, `ServiceRequest`, capabilities, progress, file options, and the wire constants. Re-exported from `lib.rs`. |
| `cues.rs` | SubRip cue parsing: `Cue`, `parse_srt_cues`, `FileTranscript::cues`. |
| `preflight.rs` | `preflight_file`, the path check the client can make before sending. |

## Examples (`examples/`)

| File | What it is |
|---|---|
| `hr_fake_host.rs` | An out-of-process fake HeardRight host, written from raw JSON on both sides of the wire. Used by `tests/e2e.rs`. It writes a `cancelled` marker on disconnect; the real host does not (see API.md). |

## Tests (`tests/`)

| File | Platform | What it covers |
|---|---|---|
| `contract.rs` | all | Role names and ranks, engine selector serialisation, record round trips, host planning on an empty root. |
| `e2e.rs` | Unix | Wire compatibility against `hr_fake_host`: frames against captured fixtures, discovery, handoff, takeover, cancel, stall, dictation pause, engine selection, verbatim, queue. |
| `windows_pipe.rs` | Windows | Named-pipe round trip. |
| `input_and_cues.rs` | all | SRT cue parsing on the host's output shape, `FileTranscript::cues`, and `preflight_file`. |
| `launch_cancel.rs` | Unix | `Launch::Never` and `Auto` with an injected launcher, and each cancel outcome, against an in-process fake service. |

## Fixtures (`fixtures/`)

Captured v1 frames and discovery files used by `tests/e2e.rs`:
`v1_file_request.json`, `v1_file_result.json`, `v1_handoff_error.json`,
`v1_health_request.json`, `v1_installed.json`, `v1_not_allowed_error.json`,
`v1_service_record.json`, `v1_takeover.json`.
