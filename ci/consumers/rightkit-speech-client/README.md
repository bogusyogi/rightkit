# rightkit-speech-client 0.4.1 registry consumer (PK15)

Standalone consumer, outside the workspace, pinning `rightkit-speech-client =0.4.1` exactly.
It exercises the direct admission and wire surface: role rank/wire names, endpoint and path
derivation, empty-root discovery (`read_live_record` none, `plan_host` HostMyself), takeover
publish/read/clear, `TranscribeOptions` builders with staging fields, `preflight_file`, SRT
cues via `FileTranscript::cues`, the 0.4.0 `TimedWord::source_ms` wire (absent parses as None,
absent serialises as before, present round-trips), schema allowlists, capability predicates,
engine selection, & capture arbitration (`decide_capture`). Takeover policy
assertions use public `TakeoverRequest` values & deterministic synthetic
liveness; separate own-PID assertion exercises real `pid_alive`. Endpoint
assertions follow Windows user-tag naming & Unix root-relative socket naming.
Staging builders are exercised for compatibility; private staging fields are
not inspected.

State: one per-run temp dir, removed on drop. No connection, launch, pipe, network, or real
user data. Queue/cancel/capture frames are not sent (no host); `Launch`, `connect`, and
`SpeechClient` Bearer paths are not exercised.

Run only by the CI consumer lane.
