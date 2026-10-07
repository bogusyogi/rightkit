# rightkit-speech-client

Client and protocol contract for the Orthic Labs shared speech service. It discovers the current-user service endpoint and exchanges bounded v1 JSON frames for health, capabilities, and transcription operations.

Hosting remains in the speech engine application; this crate provides the client contract.

Publish through the RightKit release workflow.

## Contract extensions (capability-gated)

A client uses an extension only when the host lists its capability in `engine_capabilities`; requests to older hosts stay byte-identical to v1.

| Capability | Minor | Client | Host must |
|---|---|---|---|
| `file_transcription_verbatim` | 2 | `TranscribeOptions::verbatim()` sends `verbatim: true`; refused (`Unsupported`) if the host does not list it | Decode with `DetokPolicy { strip_fillers: false, .. }` through the one shared formatter; keep the flag on the queued job through every window and dictation pause; skip all later cleanup/polish; text and word timings from the same policy |
| `file_transcription_queue` | 3 | No stall timeout; typed `Progress::Queued {position, reason}` / `Running {fraction}`; `job_status(id)`; resubmits with the same `request_id` when the host is gone (closed connection, dead pid, withdrawn/replaced `service.json`) | Answer each file request at once with `queued` (`job_id` = `request_id`, `position`, `reason`: `dictation_active` / `behind_jobs` / `loading_model`); re-send state on change and at least every 2 s; `running` + `fraction`; treat `request_id` as idempotent (attach, or replay a result finished within 10 min); answer `file_transcription_status {job_id}` with one frame or `speech_service_job_unknown`; allowlist `file_transcription_status` |

`examples/hr_fake_host.rs` implements both host sides from raw JSON; `tests/e2e.rs` is the wire-level acceptance suite for a real host.
