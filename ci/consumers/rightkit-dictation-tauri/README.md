# rightkit-dictation-tauri 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-dictation-tauri` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Error wording for microphone failures and the capture-live window. The start sequence, runtime lock and Tauri host are not run.

Proof row: error wording and capture window. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
