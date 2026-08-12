# RightKit Build Control Phase 0

`pnpm --dir tools/rightkit/packages/build-control-phase0 run run -- --output <path>` collects machine inventory, disk capacity & executable frontend parity evidence. It creates fixtures only in an OS temporary directory & emits typed `PASS`, `FAIL`, `SKIPPED_TOOL_UNAVAILABLE` or `BLOCKED_EXTERNAL` results.
