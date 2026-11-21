# rightkit-dictation-tauri 0.3.2 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-dictation-tauri` by exact registry pin
(`=0.3.2`) with the optional Tauri integration enabled. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Error wording for microphone failures and the capture-live window. The start sequence, runtime lock and Tauri host are not run.

The consumer also checks the exported paths for capture diagnostics, command outcomes, hotkey eligibility, settings policies and renderer routing. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
