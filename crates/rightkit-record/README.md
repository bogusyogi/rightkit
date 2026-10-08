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

## Recoverable media (R14)

The helpers write a fragmented MP4, so a helper killed mid-take leaves `clean.mp4` playable up to its
last whole fragment (about half a second plus encoder latency is lost). macOS runs `AVAssetWriter` in segment mode
(`outputFileTypeProfile = .mpeg4AppleHLS`, 0.5 s segments) and appends and fsyncs each segment itself,
because `movieFragmentInterval` has no effect on a file-URL MP4 writer (measured). Windows sets
`MF_TRANSCODE_CONTAINERTYPE = MFTranscodeContainerType_FMPEG4` on the sink writer (`src/windows/mf.rs`,
unverified on Windows).

When a take ends `quarantined` or is recovered from a stale state, the core probes the media
structurally (`media::probe_mp4`: ftyp, moov, and every complete `moof`/`mdat` pair; a fragment whose
`mdat` is cut short does not count). Playable media is kept and its figures are appended to the manifest
reason (`helper_lost; recovered_media frames=118/149 duration_ms=4006`) and returned as
`Outcome::recovered_media` / `Recovered::media`. Media that does not play ends the take `invalid` with
`media_unplayable`. The `accepted` count is the bundle reader's frame table.

## Redaction hashes

A `hash` field stores HMAC-SHA256 keyed by a per-installation secret that never enters the bundle. The
secret lives in the OS credential store (`rightkit-secrets`, service `dev.rightkit.record`, account
`record.redaction-key.v1`, minted on first use). `StartRequest::key_source` (a `KeySource`) supplies it;
tests inject `FixedKeySource`. A take that hashes a field and cannot read the key is refused before
anything is written. The bundle records only the key id (`scenario.json` `redaction.keyId`, plus
`hashAlgorithm`), and each hashed event carries `saltId` `hmac-sha256:<keyId>`. Equal values hash equal
within an installation. Digests are written `hmac-sha256:<64 hex>` (record-event contract 1.2), and
`bundle_verify` requires the digest prefix to match the `saltId` algorithm (`hmac-sha256:` or, for legacy
`label:` salts, `blake3:`).

Migration: bundles written before this change carry label-salted digests (`saltId` `label:<field>`, no
key id). They still read and verify; `bundle_verify` reports them in `VerifyReport::warnings` as weak
redaction (anyone can confirm a guessed value). New bundles never use label salts. Existing digests
cannot be re-keyed: the plaintext is gone.

## Helpers

- `rightkit-record-helper capture ...`: a one-shot Windows take that prints a JSON summary.
- `rightkit-record-helper serve`: the protocol over stdin and stdout, for rightkit-process.

Process ownership (spawn, supervise, restart) belongs to rightkit-process. A
helper that dies mid-take is reported as `helper_lost` and the take is
quarantined, never completed.

## Windows borderless capture

Windows grants borderless capture per executable path. On the XPS (build 26300), a
byte-identical copy of a helper that captures borderless fails at a new path:
`SetIsBorderRequired(false)` returns `0x80070490` (Element not found), even though
`GraphicsCaptureAccess::RequestAccessAsync(Borderless)` reports `Allowed`. The access
status therefore does not predict the setter.

- Install the helper at a stable path: the app's install directory or its `current` root.
  Never run it from a broker build path or a temp copy, which always shows the border.
- Grant consent once, at install or first run, for that installed path.
- A refused setter is not fatal: the take keeps the border, the helper warns on stderr, and
  the manifest records `capture.border {required, access, error}`. `bundle_verify` warns
  "capture border visible".

Unverified: the consent store key is believed to be
`HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\ConsentStore\graphicsCaptureWithoutBorder\NonPackaged`.
It has not been read on a host yet.

## Tests

`rightkit cargo test -p rightkit-record` runs the lifecycle, bundle, redaction, reader and
protocol tests on any host. The Windows capture code compiles only on Windows.
