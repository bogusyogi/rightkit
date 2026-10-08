# rightkit-record

The platform-neutral core of the RightKit recorder (RK3). It owns the recording
lifecycle, the session bundle, redaction, bundle verification, and the frame
reader. It never captures pixels: a per-OS helper implements the `Backend` trait
and reports frames, geometry, losses and failures as `HelperEvent`s.

## Modules

- `backend`: the `Backend` seam, `BackendPlan`, `StartInfo`, `HelperEvent`, `Failure`, `Permission`, `Codec`.
- `recorder`: the public take lifecycle over a `Backend`. A take ends `complete`, `quarantined` or `invalid`; `complete` needs a clean stop and an accepted readiness marker.
- `plan`, `redact`, `verify`, `reader`: the request, per-field redaction, bundle verification, and the step-to-frame reader.
- `protocol`: the helper wire protocol. Length-prefixed JSON frames (`rightkit-framed-sidecar`) with `request`, `response`, `event` and `bye` kinds.
- `process`: `ProcessBackend`, the core's `Backend` over a helper's stdio, and `serve_backend`, which lets a helper serve any `Backend`.
- `windows` (Windows only): Windows.Graphics.Capture and Media Foundation capture for one HWND.

## Helpers

- `rightkit-record-helper capture ...`: a one-shot Windows take that prints a JSON summary.
- `rightkit-record-helper serve`: the protocol over stdin and stdout, for rightkit-process.

Process ownership (spawn, supervise, restart) belongs to rightkit-process. A
helper that dies mid-take is reported as `helper_lost` and the take is
quarantined, never completed.

## Tests

`rightkit cargo test -p rightkit-record` runs the lifecycle, bundle, redaction, reader and
protocol tests on any host. The Windows capture code compiles only on Windows.
