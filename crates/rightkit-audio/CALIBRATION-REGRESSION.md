# Calibration deadline regression

Execution belongs to authorized installed-host qualification. Run on Windows & macOS with existing microphone grants & explicit user action; use real capture devices & host media callbacks.

1. Start calibration with `sample_rate = 16_000`, `min_seconds = 1`, `target_seconds = 2`, `max_seconds = 2`. Record monotonic start time, `onboarding_calibration_capture_limit`, media restore callbacks & OS microphone ownership.
2. Use an existing real input source capable of stopping callbacks while its stream remains open. Suspend its producer after calibration opens, keeping endpoint connected. Confirm source delivered fewer than 32,000 samples. If host cannot produce this condition, mark case unverified.
3. Leave recorder active without calling stop or cancel. Confirm deadline ends capture near two elapsed seconds even though sample ceiling was never reached. Read waits must never exceed remaining budget; final sub-millisecond remainder may end early. Account separately for native device-open latency & OS scheduling.
4. Confirm microphone ownership releases before media restoration; restoration occurs exactly once with `capture_limit_reached`, then limit event appears once. Call stop; confirm it returns retained PCM or `TooShort`, without second media restore.
5. Repeat with steady callbacks, then with actual device removal. Confirm sample ceiling still bounds PCM; removal releases stream before `capture_read_failed` restoration. Confirm stop reports capture error after removal.
6. Repeat stalled-source case with stop, cancel, superseding start & recorder drop before deadline. Confirm each ends old worker & releases its stream; cancel returns no PCM. Verify no automatic restart after old deadline.

Before repair, no-callback reads returned empty blocks indefinitely because limit compared sample count only. Source repair adds elapsed-time check before every read & caps read timeout to remaining budget. No runtime results are recorded here.
