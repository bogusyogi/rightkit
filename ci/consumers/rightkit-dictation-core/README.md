# rightkit-dictation-core registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-dictation-core` to the exact version
in its `Cargo.toml`. Through the public API only, it proves: a file transcription request frame
validates and one with a mismatched payload is refused, a missing saved microphone falls back to
the default device, and the RMS of a silent buffer is zero.

State: in-memory values only. No audio device, no engine process, no network, no files. Run only by
the CI consumer lane.
