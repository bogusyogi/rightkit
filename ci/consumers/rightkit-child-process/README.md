# rightkit-child-process 0.5.7 registry consumer (PK02)

Standalone consumer, outside the workspace, pinning `rightkit-child-process =0.5.7` exactly with
default features. The child is this same executable, started through `std::env::current_exe` with a
`--child <mode>` argument. It needs no shell, no network fetch and no second binary, so it runs the
same way on Windows and macOS runners.

It exercises `run` (zero exit with bounded stdout, non-zero exit as an `Ok` status with stderr),
`run_with_stdin` and `run_command` (stdin delivered to the child and returned on stdout),
`run_command_capture` (first-four-bytes capture with truncation and total byte count), and an
execution deadline that terminates a sleeping child and reports `timed_out`. It also checks the 0.5.5
descriptor policy surface: `inherit_all_descriptors(true)` still runs a child, and on Unix combining it
with `inherit_fd` is refused before spawn.

State: no files, no network, no real user data. Run only by the CI consumer lane.

Not covered: the strict incomplete-capture `Err` path. It is not deterministic on a trivial child, so
the PK02-1 stdio regression tests on each runner remain the proof for that path. `run_streaming` and
the Unix containment builders are also not exercised.
