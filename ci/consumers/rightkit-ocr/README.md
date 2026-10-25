# rightkit-ocr 0.2.3 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-ocr =0.2.3` exactly with its default
features (`native` and `onnx`), which is what CutRight uses; ScrapeRight enables the same pair. It
also depends on `serde_json = "1"` to read and write the worker protocol and result JSON. It runs on
Windows and macOS in hosted qualification, where the Apple Vision and `Windows.Media.Ocr` bindings
compile in; nothing here calls either of them.

What it proves, in plain sentences, through the public API only:

- The error contract: the three legacy prefixes, `is_backend_unavailable`, `OcrErrorKind`
  (`from_message`, `as_str` for every variant, `is_backend_unavailable`, camelCase serde),
  `OcrError` (the `Display` text and the `kind()` of every variant, prefix refinement of
  `BackendUnavailable`, `From<String>`), and `OcrResult`, `OcrLine`, `OcrRect` and `OcrBackend`
  serialising in camelCase; `OcrOutcome` is a plain record compared by value (it has no serde
  form). `native_backend()` is Vision on macOS, `WindowsMedia` on Windows and `None` elsewhere.
- The decode guard: `decode_encoded` decodes the embedded canary (`canary_png`), refuses junk, and
  refuses a PNG header claiming 10,000 by 10,000 pixels with the exact `MAX_PIXELS` message before
  any pixel buffer exists.
- The artifact manifest: `characterized_windows_contract` is the unresolved PP-OCRv5 versus v6
  conflict and fails closed; `pp_ocr_v6` is resolved with the recorded digests; `with_resolved_provenance`
  refuses a blank label, source or licence. `validate_model_dir` is exercised on a directory of three
  tiny synthetic files whose SHA-256 values the program carries: absolute path first, then
  existence, then each digest in order, returning `VerifiedOcrArtifacts` when they match, with exact
  `expected` and `actual` values on a mismatch, `MissingArtifact` for a removed file and
  `UnresolvedProvenance` before the directory is looked at. `OcrEngine::load`, `load_with_manifest`,
  `load_with_lease`, `validate_sessions` and `ocr_image_with_manifest` verify before any ONNX
  session, and the `ModelLeaseHook` is taken first, receives the three expected paths, releases its
  guard when the load fails, and a refused lease stops the load.
- The router without a recognizer: `OcrOptions` and `OnnxConfig` defaults and builders, the ONNX
  preference with no model config (`NoFallbackConfigured`) and with a config that fails
  verification, bad bytes and a missing file giving `Runtime`, and `validate_fallback`.
- The isolated worker protocol, in both directions. This executable is the child. The crate's own
  `worker::run_if_requested` serves real requests that fail at the decoder (junk bytes, and the
  oversized header), returning `Runtime` with exit status zero. A fake child speaking the same
  protocol drives `WorkerCommand` (`current_exe`, `new`, `with_deadline`, `with_canary_deadline`,
  `for_canary`, `run`, `run_observed`): a success with its event order (`Spawned`, `Waiting`,
  `Exited`, `Responded`) and `RunRecord`; a 2 MB request delivered unchanged; every failure kind
  (`WindowsOcrUnavailable`, `MacosVisionUnavailable`, `UnsupportedPlatform`, `Runtime`,
  `WorkerProtocol` twice, `WorkerFailed` on a non-zero exit, `WorkerTimeout` from the child's own
  exit code 124 and from the parent's deadline, `WorkerOutputExceeded` from a flood, `RequestTooLarge`
  with no child started, and `WorkerFailed` for a missing executable).
- The router with that worker: where the host has a native backend (the hosted macOS and Windows
  runs) the isolated native path returns the fake child's lines under the host's backend, `Auto`
  with an "unavailable" verdict and no ONNX config returns `NoFallbackConfigured`, a per-image fault
  stays `Runtime`, and `probe_native` honours a host `NativeHealthHook` (cached verdict, and a
  recorded verdict after a worker probe). Where it has none, the router refuses the same requests
  before spawning.

What it deliberately does not prove: recognition by Apple Vision, `Windows.Media.Ocr` or the PP-OCR
models, ONNX Runtime loading or session building, DirectML or CoreML, recognition accuracy, and the
`rightkit-ocr` binary. No model, runtime or OS text-recognition service is used.

State: a per-process directory under the OS temp directory, removed at the end, and short-lived
child processes of this executable (one sleeps until the parent's 500 ms deadline kills it). No
network, no user data. Runs in about a second. Run only by the CI consumer lane.
