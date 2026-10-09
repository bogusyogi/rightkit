# Changelog

## 0.3.3

- Requires `rightkit-process` 0.3.3. No source change.

## 0.3.0 — staged, unpublished

- **Breaking:** `SandboxOutput` gains `truncated: bool`. Struct literals of `SandboxOutput`
  must add the field.
- **Bounded capture.** `wait_with_output` keeps at most `DEFAULT_OUTPUT_CAP` (16 MiB) per stream;
  bytes past the cap are read and discarded, so the child never blocks on a full pipe and
  `truncated` is set. `wait_with_output_capped(timeout, cap)` sets the cap per call.
- **Deadline covers the tree and its pipes.** The timeout bounds the whole run, including
  descendants that inherit stdout/stderr. If a descendant still holds a pipe at the deadline,
  the tree is killed (process group on Unix, Job Object on Windows), readers get a 500 ms
  grace to reach end-of-pipe, and the result is `timed_out` with whatever was captured.
  Previously a background grandchild could hold `wait_with_output` for as long as it lived.
- Normal runs (child and pipes finish before the deadline) return the same output as before.

## 0.2.4

- Prior release; see git history.
