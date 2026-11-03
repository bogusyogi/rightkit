# rightkit-dictation-ffi 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-dictation-ffi` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

FFI ownership contract (null owned-bytes and null string frees are no-ops) and keyword-spotting file names. No model is loaded.

Proof row: FFI ownership. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
