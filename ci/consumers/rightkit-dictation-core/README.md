# rightkit-dictation-core 0.3.2 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-dictation-core` to the exact version
in its `Cargo.toml`. Through the public API only, it covers: a file transcription request frame
validates and one with a mismatched payload is refused, a missing saved microphone falls back to
the default device, the RMS of a silent buffer is zero, and `PillEvent::Status { label: "Saved" }`
serializes to the source-derived `{"kind":"status","label":"Saved"}` wire form. The expected
wire form is grounded in `PillEvent`'s serde attributes and checked-in fixture; no runtime or
qualification result is claimed.

State: in-memory values only. No audio device, no engine process, no network, no files. Run only by
the CI consumer lane.
