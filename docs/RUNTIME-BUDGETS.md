# Runtime budgets for long-running shared crates

Added 2026-10-11 after rightkit-fsindex 0.1.0 reached 5.4 GB resident and 65% CPU inside the Pulse hub without any audit noticing (gap ledger #76). Every audit until then was static: declarations, ownership, duplication, adoption. None measured a crate under its real load.

## Rule

A crate that runs continuously in an app (watchers, indexes, daemons, services, supervisors, schedulers, caches with eviction) must declare and prove a runtime budget before it is published or adopted:

1. **Declare** in the crate README under "Runtime budget": resident memory (per entry or per unit of work and an absolute ceiling), steady-state CPU, disk operations per second under change, and the measured figures from one named run (machine, data set size, command).
2. **Measure** with a repeatable in-repo bench or test (synthetic data allowed when real data is unavailable) that prints the figures; the published figures come from that run, not from reasoning.
3. **Expose** the figures at runtime through `status()` or an equivalent, so a host can show them and an audit can read them from a running app.
4. **Gate**: `right-release publish cargo` refuses a crate in this class whose README has no "Runtime budget" section, and the per-app adoption audit records the measured figures from the installed app, not the declaration.
5. **First consumer at scale** (whole disk, whole mailbox, whole repo) runs the crate under that load with the figures visible before the app ships it on; a crate is not "adopted" until that run is recorded.

## Crates in this class today

rightkit-fsindex, rightkit-watch, rightkit-jobs (supervisor), rightkit-service, rightkit-daemon, rightkit-cache, rightkit-sync, rightkit-logs (tracing worker), rightkit-shell (watchdog, idle), rightkit-qa (harness), rightkit-send.

## Why the audits missed it

The 2026-10-10 audits checked what each crate declares and who owns what. They did not run anything. A crate can pass every static check and still be unusable at scale. This rule adds the measurement the audits lacked; the release gate makes it impossible to publish without it.
