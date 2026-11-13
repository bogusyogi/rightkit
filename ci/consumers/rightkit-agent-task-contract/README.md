# rightkit-agent-task-contract 0.1.1 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-agent-task-contract` by exact registry
pin (`=0.2.0`) with default features (no `cli`), plus `serde_json`. It has no path, Git, workspace or patch
dependency and no committed `Cargo.lock`.

Proof row: task contract. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.

## What it proves

- The task-state table: legal and illegal transitions, terminal statuses (`uncertain` and `cancel-requested` are
  not terminal), and that only reversible effects skip the owner's approval check.
- The approval binding: its wire form keeps `args_hash` (`sha256:<hex>`), `destination`, `expires_at_ms` and
  `revision` flat, reads into the approvals core's `Binding`, writes back byte-equal, and refuses an `args_hash`
  that is not `sha256:` plus 64 lowercase hex; the core rule refuses a changed argument digest.
- The voice hand-off maps to a task envelope labelled with the caller's chat label and policy reference, and
  refuses empty labels and a dictation-style hand-off R1 resolved locally.

## What it deliberately does not prove

The task command line (feature `cli`, needs an engine), any runtime, journal or IPC. No network, no real user
data, no process spawn.
