# rightkit-speech-client 0.3.4: API reference

Every public item, with one line on its purpose and the errors it returns. Errors are
`ClientError` variants unless the row says otherwise. The host's input contract (what it
accepts, and the error each violation produces) is in [Input contract](#input-contract).

Host facts come from HeardRight's v1 host, read for this release in the `heardright`
repository under `tauri-app-next/`. Where this crate's fake host (`examples/hr_fake_host.rs`)
differs from the real host, the row or section says so.

Contents: [Discovery](#discovery-and-roles) · [Connecting and transcribing](#connecting-and-transcribing) ·
[Launch policy](#launch-policy) · [Cancel](#cancel) · [Results and cues](#results-and-cues) ·
[Pre-flight](#pre-flight) · [Errors](#errors) · [Wire constants and frames](#wire-constants-and-frames) ·
[Input contract](#input-contract) · [Known divergences](#known-divergences)

## Discovery and roles

| Item | Purpose | Errors |
|---|---|---|
| `HostRole` | Role of a host process: `HeardRightShell`, `HeardRightHeadless`, `ScrapeRight`, `CutRight`. | — |
| `HostRole::rank`, `outranks` | Rank 0 (shell) to 3 (CutRight); a lower rank outranks a higher one. | — |
| `HostRole::as_str`, `from_wire` | Wire name (`heard_right_shell`, `heard_right_headless`, `scrape_right`, `cut_right`) and its inverse; `from_wire` returns `None` for other names. | — |
| `HostRole::parse_env` | Parses `HR_SPEECH_SERVICE_ROLE` values, case-insensitively, including `heardright`. | — (returns `None`) |
| `HostRole::rank_table` | Role-name to rank map for `rightkit_service::ServiceConfig::known_roles`. | — |
| `ServiceRecord` | Fields of `service.json`: `protocol_major`, `protocol_minor`, `role`, `pid`, `endpoint`, `engine_version`. | — |
| `TakeoverRequest` | Fields of `takeover.json`: `role`, `pid`, `requested_at_ms`. | — |
| `InstalledEngine` | Fields of `installed/heardright.json`: `engine_path`, `engine_version`. | — |
| `service_dir`, `service_record_path`, `takeover_request_path`, `installed_heardright_path` | Paths under `<suite>/runtime/speech-service/`. | — |
| `endpoint_for(root, user_tag)` | Unix: `<service_dir>/speech.sock`. Windows: `\\.\pipe\orthic-labs-speech-<user_tag>-<fnv1a32 of root>`. | — |
| `default_suite_root()` | Suite root by the shared convention: `RIGHTKIT_SUITE_ROOT`, `RIGHT_SUITE_DIR`, `ORTHIC_SHARED_DIR`, then the app data location. | `Io` when no root resolves. It never falls back to the working directory. |
| `read_live_record(root)` | The record, only if its pid is alive. Reads files; changes nothing. | — (`None`) |
| `pid_alive(pid)` | Whether a process is alive. | — |
| `plan_host(root, my_role)` | Decides what a caller should do: use the running host, start the installed engine, host itself, or take over. | — |
| `HostPlan` | Result of `plan_host`: `UseRunning`, `StartInstalledHeardRight(PathBuf)`, `HostMyself`, `TakeOver { from }`. | — |
| `should_request_takeover(our, record)` | True only against a live host of lower rank. | — |
| `takeover_obliges_yield(ours, request, own_pid, alive)` | True when a live, outranking request names another process. | — |
| `read_takeover(root)` | The pending takeover request, if it parses and names a known role. | — (`None`) |
| `request_takeover(root, role)` | Publishes `takeover.json` for this process. | `io::Error`: `InvalidInput` for a non-absolute root, or any write failure. |
| `clear_takeover_if_ours(root)` | Removes `takeover.json` only if this process wrote it. | — |
| `EngineLaunch` (`args`, `env`) | How to launch an installed engine: its arguments and environment. | — |
| `EngineLaunch::new(args)`, `env(key, value)` | Builders for an `EngineLaunch`. | — |
| `EngineLaunch::heardright()` | `--speech-service` with `HR_ASR_BACKEND=parakeet-tdt`. | — |
| `start_installed_engine(root, engine, launch, wait)` | Spawns the engine detached, then waits up to `wait` for a live record. | `Io` (spawn failed), `Timeout` (no live record within `wait`). |
| `start_installed_heardright` (deprecated since 0.3.1) | `start_installed_engine` with `EngineLaunch::heardright()` and a caller-supplied wait. | as `start_installed_engine` |

## Connecting and transcribing

| Item | Purpose | Errors |
|---|---|---|
| `connect(root)` | Connects to the live host and completes the health handshake, bounded by 10 s. | `NoService`; `ProtocolMismatch { major }` when the record's major is not 1; `Io`; `Timeout` (handshake); `Engine { code: "unavailable" }` when the host reports itself unavailable. |
| `SpeechClient::role`, `engine_version`, `protocol_minor` | The host's role, engine version and protocol minor from `service.json`. | — |
| `SpeechClient::supports_engine_selection` | True for protocol minor 1 or later. | — |
| `SpeechClient::capabilities(timeout)` | The host's capabilities and engine inventory. Cached per connection. | `Engine`, `Io`, `Timeout`. |
| `SpeechClient::supports_queue` | True when the host advertises `file_transcription_queue`. | as `capabilities`. |
| `SpeechClient::job_status(job_id)` | One-shot state of a minor-3 job. | `Unsupported` (no queue contract); `Engine` (the job's stored failure); `Io`; `Timeout`. |
| `SpeechClient::transcribe_file(path, stall)` | Transcribes with the host's default engine and returns the transcript. | as `transcribe`. |
| `SpeechClient::transcribe(path, opts)` | Transcribes with engine, language, verbatim, request id, progress, cancel and stall settings. | `Unsupported` (verbatim not advertised; a named engine on a minor-0 host); `Engine { code, message }` (host error frame); `Cancelled`; `HandoffRetry`; `HostGone` (queue hosts); `Timeout` (non-queue hosts only); `Io` (closed connection, bad frame, write failure). |
| `SpeechClient::call_raw(frame, stall, cancel, report)` | Sends one frame and waits for its reply. Low level. | `Io`, `Timeout`, `Cancelled`. |
| `TranscribeOptions` | Public fields: `engine`, `language`, `stall_timeout`, `cancel`, `on_progress`, `request_id`, `verbatim`. `Default` uses a 120 s stall timeout. | — |
| `TranscribeOptions::new(stall)` | Options with the given stall timeout. | — |
| `TranscribeOptions::request_id`, `verbatim`, `engine`, `language`, `cancel_token`, `on_progress` | Builder methods. `request_id` is the idempotency key and the job id on a queue host. | — |
| `ProgressFn` | `Box<dyn FnMut(&Progress) + Send>`, the progress callback type. | — |
| `Progress` | Events: `StartingHost`, `Connecting { attempt }`, `Submitted`, `Waiting { elapsed, idle }`, `Host(HostProgress)` (minor 1 and 2 hosts), `Queued { job_id, position, reason }`, `Running { job_id, fraction }`, `Reconnecting { attempt }`, `HandoffRetry { attempt }`. | — |
| `JobStatus` | `Active(HostProgress)`, `Done(FileTranscriptResult)`, `Unknown`. | — |
| `CancelToken` | Shared cancel flag: `new()`, `cancel()`, `is_cancelled()`. Cloning shares the flag. | — |

## Launch policy

New in 0.3.4. `transcribe_via_service_with` is `transcribe_via_service_policy` with
`Launch::Auto` and `HeardRightLauncher`, and its behaviour is unchanged.

| Item | Purpose | Errors |
|---|---|---|
| `Launch::Auto` | Default. When no service is live, starts an installed HeardRight host through the launcher, as before. | — |
| `Launch::Never` | Uses a live host as found, whatever its role. Never launches and never requests a takeover. With no live host it returns `NoLiveHost` before doing anything else. | `NoLiveHost` (no live record). |
| `HostLauncher` (trait) | `launch(suite_root, engine_path) -> Result<(), ClientError>`: starts an installed host and returns once a live record appears. Any error becomes `NoService` in the caller. | Implementation-defined. |
| `HeardRightLauncher` | The default `HostLauncher`: `start_installed_engine` with `EngineLaunch::heardright()` and a 60 s wait. | `Io`, `Timeout` (surfaced to the caller as `NoService`). |
| `transcribe_via_service_with(root, role, path, opts)` | Transcribes with `Launch::Auto` and `HeardRightLauncher`. Connects, transcribes, and re-discovers on handoff (up to 3 attempts). | `NoService`; `NoLiveHost` is never returned here. `HandoffRetry` and `HostGone` after 3 attempts; `Cancelled`; any transcribe error. |
| `transcribe_via_service_policy(root, role, path, opts, launch, launcher)` | The general form: `launch` decides whether a host may be started, and `launcher` starts it. | `NoLiveHost` (`Never`, no live host); `NoService` (`Auto` with a plan that says host yourself or take over, a failed launch, or no host after a launch); `HandoffRetry` and `HostGone` after 3 attempts; `Cancelled`; any transcribe error. |
| `transcribe_via_service(root, role, path, timeout)` | `orthic-speech-client`-compatible wrapper: the default engine and a stall timeout. Returns `FileTranscript`. | as `transcribe_via_service_with`. |

## Cancel

New in 0.3.4. The protocol has no cancel message and no cancel acknowledgement.

| Item | Purpose | Errors |
|---|---|---|
| `cancel_request(root, &token, request_id, bound) -> CancelOutcome` | Fires `token` (the in-flight request returns `Cancelled` within about 100 ms and its connection closes), then asks the live service for the status of `request_id` on a new connection. Each wire step gets the time left of `bound`, so the call returns in about `bound`. | Never fails. The failure cases are `CancelOutcome::Unknown`. |
| `CancelOutcome::Finished(Result<FileTranscriptResult, ClientError>)` | The job had finished before the status query. `Ok` is the stored transcript. `Err` is the failure the service stored; `HandoffRetry` means the job ended in a handoff and should be resubmitted with the same request id. | — |
| `CancelOutcome::StillActive(HostProgress)` | The service still holds the job queued or running, with its last reported state. | — |
| `CancelOutcome::NoLongerActive` | The service holds no queued, running or retained job under the id. This is absence, not an acknowledgement. | — |
| `CancelOutcome::Unknown(ClientError)` | The outcome cannot be learned. The error says why: `NoService` (no live record), `Io` (connection failed or closed), `Timeout` (no reply within `bound`), `Unsupported` (empty request id, or a host below protocol minor 3). | — |

What each outcome means on the wire:

- **`Finished`** is definite: the service reports a finished job, retained for 10 minutes.
- **`StillActive`** is definite and is the usual outcome on HeardRight's queue host. The
  cancel closed this client's connection, but the job runs on: the host's dispatcher does not
  link the job to the socket, and nothing sets the cancel flag it passes to the decoder
  (`speech_service/conn.rs`, `dispatch`). A `StillActive` job finishes, and its result is kept
  for 10 minutes.
- **`NoLongerActive`** means nothing is running for the id on that service. It does not
  show that a cancel worked. The result may have expired, the host may have restarted, or the
  request may never have reached it.
- **`Unknown`** means the outcome is not known. Treat the job as possibly running.

A true acknowledgement needs a cancel message on the host. None is defined in protocol
minor 3, and this release does not invent one.

## Results and cues

| Item | Purpose | Errors |
|---|---|---|
| `FileTranscript` | Fields: `text`, `srt`, `vtt`, `words`. See [Output](#output). | — |
| `FileTranscript::cues()` | The cues parsed from `srt` (see `parse_srt_cues`). Empty when `srt` is empty. Derived from the host's own cues, not regrouped on the client. | — |
| `Cue` | One subtitle cue: `start_ms: u64`, `end_ms: u64`, `text: String` (lines joined with `\n`, literal). | — |
| `parse_srt_cues(srt)` | Parses SubRip text into cues. Blocks without a `-->` line, with an unreadable stamp, or with no text are skipped. Accepts LF or CRLF, a BOM, and `,` or `.` before milliseconds. | — (never fails; unreadable input yields fewer cues) |
| `FileTranscriptResult` | Fields: `transcript: FileTranscript`, `engine: Option<String>`, `language: Option<String>`. | — |
| `TimedWord` | Re-export of `rightkit_transcript::TimedWord`: `text`, `start_ms`, `end_ms` (integer milliseconds, `u64`). | — |

## Pre-flight

New in 0.3.4.

| Item | Purpose | Errors |
|---|---|---|
| `preflight_file(path)` | Checks a path before it is sent. Returns `Ok(())` unless: the path is not valid UTF-8 (`Unsupported`, because the JSON wire cannot carry it and sending would name a different file); or the path is absolute and does not exist (`Engine { code: "E_ENGINE", message: "file not found: <path>" }`, the host's own code and message). A relative path is not checked, because the host resolves it in its own working directory. | `Unsupported`, `Engine`. |

`preflight_file` is opt-in. `transcribe` and its wrappers do not call it, so existing callers
keep their behaviour. The client has no check for file size, extension, sample rate, duration
or readability: the host has no size, extension or sample-rate limit, its duration limit
depends on an entitlement the client cannot see, and its read check runs in its own process
(see [Input contract](#input-contract)). A directory is not checked either: the host rejects it
with `E_ENGINE` on Unix, and Windows may report `E_PERMISSION`.

## Errors

`ClientError` is not `#[non_exhaustive]`. Matches on it need a wildcard arm to survive
`NoLiveHost`, which 0.3.4 added.

| Variant | Meaning | Retry? |
|---|---|---|
| `NoService` | No live host; or the caller must host (or take over) itself. | Only after a host appears. |
| `NoLiveHost` | `Launch::Never` and no live host. Nothing was launched or taken over. | Only after a host appears. |
| `HandoffRetry` | The host is handing off to a higher-ranked host. | Yes; resubmit with the same request id. |
| `HostGone` | Queue host lost: connection closed, pid dead, or `service.json` withdrawn or replaced. | Yes; resubmit with the same request id. |
| `ProtocolMismatch { major }` | The record's protocol major is not 1. | No. |
| `Engine { code, message }` | The host's error frame. Codes are listed under [Input contract](#input-contract). | Per code: `E_PERMISSION` and `E_ENGINE` are not retryable; `speech_service_host_handoff`, `E_QUEUE_FULL` and `model_lease_unavailable` are. |
| `Timeout` | No frame within the stall timeout (non-queue hosts), or no reply within the handshake or cancel bound. | Yes. |
| `Io(String)` | Socket or protocol failure: closed connection on a non-queue host, bad frame, write failure. | Depends on cause. |
| `Cancelled` | The caller's `CancelToken` fired. The service may still finish the job; see [Cancel](#cancel). | — |
| `Unsupported(String)` | The host cannot serve the request as asked: no verbatim, no queue contract, a named engine on a minor-0 host, or a path the wire cannot carry. | No; change the request. |

## Wire constants and frames

| Item | Value or purpose |
|---|---|
| `SPEECH_FAMILY` | `"parakeet-tdt-v3"`, the model family. |
| `SERVICE_NAME` | `"speech-service"`, the directory under `<suite>/runtime/`. |
| `SERVICE_PROTOCOL_MAJOR` | `1`. |
| `SERVICE_PROTOCOL_MINOR` | `0`, the v1.0 baseline. |
| `SERVICE_PROTOCOL_MINOR_ENGINES` | `1`: engine selection and progress frames. |
| `SERVICE_PROTOCOL_MINOR_VERBATIM` | `2`: the `verbatim` request field. |
| `SERVICE_PROTOCOL_MINOR_QUEUE` | `3`: the job queue contract. |
| `CAPABILITY_FILE_TRANSCRIPTION` | `"file_transcription"`. |
| `CAPABILITY_VERBATIM` | `"file_transcription_verbatim"`. |
| `CAPABILITY_QUEUE` | `"file_transcription_queue"`. |
| `SCHEMA_PROGRESS` | `"file_transcription_progress"`. |
| `SCHEMA_STATUS` | `"file_transcription_status"`. |
| `ERROR_HOST_HANDOFF` | `"speech_service_host_handoff"`, retryable. |
| `ERROR_NOT_ALLOWED` | `"speech_service_request_not_allowed"`. |
| `ERROR_ENGINE_UNAVAILABLE` | `"speech_service_engine_unavailable"`. |
| `ERROR_JOB_UNKNOWN` | `"speech_service_job_unknown"`: no queued, running or retained job. |
| `QUEUE_KEEPALIVE_MS` | `2000`: the host's maximum interval between job-state frames. |
| `QUEUE_RESULT_RETENTION_MS` | `600000`: how long a finished job's result is kept for re-attach. |
| `SERVICE_ALLOWED_SCHEMAS` | The five request schemas the host accepts: `engine_health`, `engine_capabilities`, `engine_info`, `file_transcription_request`, `file_transcription_status`. |
| `ENGINE_PARAKEET`, `ENGINE_WHISPER` | `"parakeet"`, `"whisper"`. |
| `is_allowed_schema(schema)` | Whether the host accepts a schema. |
| `ServiceEnvelope` (public fields) | One v1 frame: `protocol_major`, `protocol_minor`, `schema_name`, `schema_version`, `engine_version`, `request_id`, `session_id`, `trace_id`, `payload`, `error`. |

`ServiceEnvelope` constructors and readers. "Host" means a server builds the frame; "client"
means a client builds it.

| Method | Purpose |
|---|---|
| `health(request_id, trace_id)` | Health request (client). |
| `capabilities(request_id, trace_id)` | v1 capabilities request (client). |
| `capabilities_with_engines(request_id, trace_id, engines)` | Capabilities response with the engine inventory (host). |
| `capabilities_full(request_id, trace_id, engines, verbatim)` | Capabilities response, optionally advertising verbatim (host). |
| `capabilities_with(request_id, trace_id, engines, extra)` | Capabilities response listing `file_transcription` plus `extra` (host). |
| `info(request_id, trace_id, cold_load_s, engine_version)` | Engine info response (host). |
| `file_request(request_id, trace_id, path)` | v1 file request (client). |
| `file_request_with(request_id, trace_id, path, &FileOptions)` | File request; `engine`, `language` and `verbatim` appear only when set (client). |
| `file_result(request_id, trace_id, transcript)` | Result frame (host). |
| `file_result_with(request_id, trace_id, transcript, engine, language)` | Result frame echoing `engine` and `language` (host). |
| `job_progress(job_id, trace_id, state, position, reason, fraction)` | Minor-3 job-state frame (host). |
| `status_request(request_id, trace_id, job_id)` | Minor-3 status request (client). |
| `progress(request_id, trace_id, fraction, state, message)` | Minor-1 progress frame (host). |
| `error(request_id, trace_id, code, message, retryable)` | Error frame (host). |
| `validate_envelope()` | Checks protocol major 1 and schema version 1. |
| `request()` | Classifies a request frame as a `ServiceRequest`, or returns the reason it is invalid. |
| `file_options()` | Reads `engine`, `language` and `verbatim` from a request. |
| `health_status()`, `health_unavailable()` | Reads the health status. |
| `transcript()`, `transcript_result()` | Reads a result frame. |
| `capabilities_info()` | Reads a capabilities frame. |
| `host_progress()` | Reads a progress frame. |

Other wire types:

| Item | Purpose |
|---|---|
| `ServiceRequest` | Classified request: `Health`, `Capabilities`, `Info`, `FileTranscription { path }`, `Status { job_id }`. |
| `Capabilities` | `capabilities: Vec<String>`, `engines: Vec<EngineInfo>`. Methods: `supports_file_transcription`, `supports_verbatim`, `supports_queue`, `engines_or_default`, `engine(id)`, `ready_for(language)`. |
| `EngineInfo` | One engine: `id`, `languages`, `ready`, `loaded`, `model`. |
| `EngineSelector` | `Auto` or `Named(String)`. Serialises as its wire string. `parakeet()`, `whisper()`, `as_str()`, `parse(s)`. |
| `FileOptions` | Request options: `engine: Option<EngineSelector>`, `language: Option<String>`, `verbatim: bool`. |
| `HostProgress` | Progress frame: `fraction: Option<f32>`, `state: ProgressState`, `message`, `job_id`, `position: Option<u32>`, `reason: Option<QueueReason>`. |
| `ProgressState` | `Queued`, `Running`, `PausedForDictation`, `Other(String)`. |
| `QueueReason` | `DictationActive`, `BehindJobs`, `LoadingModel`, `Other(String)`. `as_str()`, `from_wire(s)`. |
| `ServiceErrorPayload` | Error object: `code`, `message`, `retryable`, `diagnostics`. |

## Input contract

What the HeardRight host accepts, and what each violation produces. The host's checks run in
the order shown. The client's only pre-flight, `preflight_file`, covers row 4 (existence)
and the UTF-8 requirement, which is not a host row. Everything else the host decides.

| # | Check | Where (host) | Result |
|---|---|---|---|
| 0 | Request line: at most 1 MiB (1,048,576 bytes) including its newline. Paths of any OS-valid length fit well under this. | `speech_service/conn.rs`, `MAX_LINE` | A longer line closes the connection with no error frame. The client sees `Io`, or `HostGone` on a queue host. |
| 1 | Frame shape: `file_transcription_request` with a string `path` and a non-empty `request_id`. | `conn.rs`, `parse_line`, `handle_frame`; `ServiceEnvelope::request` | `E_BAD_FRAME`: "request_id must not be empty" or "missing file path". Not retryable. |
| 2 | Host is accepting work (not handing off). | `conn.rs`, `accepting` | `speech_service_host_handoff`. Retryable. |
| 3 | Fewer than 512 retained jobs. | `conn.rs`, `enqueue` | `E_QUEUE_FULL`. Retryable. |
| 4 | The path exists. `Path::exists` follows symlinks. The host resolves a relative path in its own working directory. | `heardright-inference/src/file_transcribe.rs`, `decode_to_16k_mono` and `probe_duration_secs` | `E_ENGINE`, "file not found: <path>". Not retryable. |
| 5 | The host process can open and read the file. The host does this itself before FFmpeg runs, so the client's own access does not matter. | `file_transcribe.rs`, `preflight_read`, `permission_error` | `E_PERMISSION`, "E_PERMISSION: HeardRight is not allowed to read <path>. Grant …". Not retryable. Other open failures: `E_ENGINE`, "could not read <path>: <os error>". |
| 6 | Duration against the entitlement, probed before decoding. Free: at most 600 s. Pro: no limit. The host knows the entitlement; the client does not. | `heardright_core/src/engine/protocol.rs`, `check_duration_limit`; `heardright-runtime/src/worker/worker_main.rs`, `transcribe_file` | `E_ENGINE`, "Free transcription is limited to 10 minutes per file. Upgrade to Pro for unlimited file length." Not retryable. |
| 7 | Decode with FFmpeg: `-vn`, the first audio stream, `-ac 1 -ar 16000 -c:a pcm_s16le`. | `rightkit-media/src/decode.rs`, `decode_command`; `file_transcribe.rs`, `decode_with_ffmpeg` | If FFmpeg's stderr reports a denied read: `E_PERMISSION`. Otherwise `E_ENGINE`, "ffmpeg could not read this file: <stderr>". If FFmpeg cannot run: `E_ENGINE`, "could not run ffmpeg at <path> (…)" or "Bundled media decoder is missing; repair the HeardRight installation." |
| 8 | Duration again, exactly, from the decoded sample count. | `worker_main.rs`, after `decode_to_16k_mono` | As row 6. |
| 9 | Progress: a job that makes no progress for 10 minutes fails. | `worker_api.rs`, `FILE_STALL_TIMEOUT`, `wait_for_file_reply` | Job error, `E_ENGINE` class, not retryable. |
| 10 | Cancel: none. A client disconnect does not stop a job. | `conn.rs`, `dispatch` | No error; see [Cancel](#cancel). |

Errors reach the client as `ClientError::Engine { code, message }`. A job's stored error
reaches the client the same way. Its retryable flag is set only for a handoff or a model lease
that is unavailable (`model_lease_unavailable`).

### Input rules

- **Containers and codecs.** Whatever the approved bundled FFmpeg decodes. The host probes
  the content, not the extension, and has no container or codec allowlist. WebM and MKV go
  through the same FFmpeg path. The build's configure flags are recorded in
  `rightkit-media/catalog/artifacts.json`. This release did not run the build to list its
  decodable formats, so check your own formats before you rely on them.
- **Video and streams.** Video is dropped (`-vn`). The first audio stream is decoded. Choose
  the track before sending if a file has several.
- **Sample rate and channels.** Any. FFmpeg resamples to 16 kHz and mixes down to mono.
- **Size.** No file-size limit in the host. Duration limits apply (rows 6 and 8). The whole
  decoded audio is held in memory as `f32`, about 230 MB per hour.
- **Directories the host must read.** Any path the host process can open, with no
  directory allowlist in the host. On macOS, paths under `/Volumes` need Files & Folders →
  Removable Volumes (or Full Disk Access); other paths need Full Disk Access. Other
  platforms need ordinary read access for the host's user. The client does not need access
  to the file.
- **Paths.** Send absolute paths. A relative path resolves in the host's working directory,
  not yours. A path that is not valid UTF-8 cannot be sent; `transcribe` sends its lossy form
  silently, so run `preflight_file` first.
- **Request id.** Non-empty. The host uses it as the job id and keeps the result for 10 minutes
  (`QUEUE_RESULT_RETENTION_MS`).
- **Verbatim.** Skips filler removal and text polish. The engine echo gains a `+verbatim`
  suffix.

### Output

- `text`: the host's transcript. Post-processed by text polish unless `verbatim`.
- `words`: integer milliseconds, `start_ms` and `end_ms`. Polish does not change them.
- `srt`: SubRip cues from the host's grouping rules (12 words, 7 s, a gap over 1 s, or
  sentence-final punctuation after 3 words, with an 80 ms floor). Literal text: no escaping.
  Parse it with `FileTranscript::cues()`.
- `vtt`: the same cues in WebVTT, with `&`, `<` and `>` escaped.
- `engine`: the engine version and model, for example `heardright-engine/<version>/parakeet-tdt-v3`,
  with a `+verbatim` suffix when verbatim.
- `language`: not echoed by the current host, so it is absent. The client sends the request's
  language, but the result will not confirm it.

## Known divergences

- **Disconnect and cancel.** This crate's fake host (`examples/hr_fake_host.rs`) writes a
  `cancelled` marker when the client disconnects during a job. The real host does not stop
  the job on disconnect. Tests built on the fake therefore check the client side only.
- **`language` echo.** The real host does not echo `language`. The fake does for minor 1 and
  later.
- **Cancel acknowledgement.** Absent on the wire in protocol minor 3. `CancelOutcome` is the
  strongest outcome the protocol supports.
