# rightkit-dictation-ffi 0.2.1 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-dictation-ffi` by exact registry pin
(`=0.2.1`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

FFI ownership contract (null owned-bytes and null string frees are no-ops), keyword-spotting file names, and brand-table configuration result codes. The process-wide table is installed last, after other checks. No model is loaded.

Proof row: FFI ownership. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
