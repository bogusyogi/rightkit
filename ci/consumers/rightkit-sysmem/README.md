# rightkit-sysmem 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-sysmem` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Calls the three public queries, which are process-local reads: allocator pressure relief returns
this process's free pages, the physical-memory probe and the monitor profile lookup read. Windows is
expected to report physical memory; every other host answers None for both reads.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
