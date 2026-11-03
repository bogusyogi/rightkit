# rightkit-agent-engine 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-agent-engine` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

Task-state table and effect classification: transition, is_legal_transition, TaskStatus::is_terminal, EffectClass::is_irreversible. No I/O.

Proof row: task-state table. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
No network, no real user data, no process spawn.
