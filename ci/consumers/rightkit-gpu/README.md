# rightkit-gpu 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-gpu` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

No adapter is enumerated and no device is created (`GpuContext::create` and `adapter_names` load GPU
drivers). Exercises backend list parsing, the app-variable and WGPU_BACKEND precedence, per-OS
defaults, resolution against compiled backends, limits-cap parsing, adapter pick, context option
defaults, memory-budget sizing and admission (including halving on out-of-memory and growing back),
a private exit gate, and the init-marker read path with no marker set. The process-wide shutdown is
never begun and nothing is written.

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
