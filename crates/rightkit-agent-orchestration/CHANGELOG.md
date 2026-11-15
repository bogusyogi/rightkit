# Changelog

## 0.1.3

- Breaking for dependents: a dependency requirement was raised to a semver-breaking version.
- `coding` takes CodeRight's newer orchestration code. New `coding::sibling_hold` (`claim`, `SiblingStart`, `LeaderGuard`, `FollowerWait`): the first sub-agent start for a `(parent session, prefix)` pair goes at once, siblings wait for its first byte or a cap, so they read the provider's prompt cache instead of each writing it. New `GovernedArbiter::begin_goal(run_id, goal_key)` resets a run's unbacked-work counters when the goal changes (`reset` also forgets the goal binding); a run that never calls it behaves as before.
- New `coding::host`, each value fixed once at start-up with the value this crate always used until set: `set_task_id_domain` (`rightkit-agent/r1b/task-id/v1`, mixed into every `StableTaskId`), `set_roster_router_name` (`rightkit-agent-router (L2 roster heuristic)`) and `set_strict_checkpoint_resume` (off). With strict resume on, `GraphCheckpointStore::restore_graph` adopts a saved node only when it is `Done` and has the planned node's title, role, done-when and dependencies (the planned node always wins), and `run_graph_resumable` drops saved outputs of nodes that did not resume; off, every planned node is replaced by its saved one as before.
- The workspace state folder under which handoff results (`<state-dir>/handoff-results`, `HandoffArchiveConfig::for_workspace`) and sub-agent worktrees (`<state-dir>/worktrees/<branch>`) live is `rightkit_agent_policy::state_dir_name()` (`.rightkit` until the host calls `rightkit_agent_policy::set_state_dir_name`). New optional dependency `rightkit-agent-policy` 0.2.1 under `coding`. CodeRight calls `set_state_dir_name(".coderight")`, `set_task_id_domain("coderight/r1b/task-id/v1")`, `set_roster_router_name("coderight-router (L2 roster heuristic)")` and `set_strict_checkpoint_resume(true)`.
- Not carried from CodeRight: its `examples/router_proof_report.rs` (an ad-hoc runner), `#[cfg(test)]` modules, and `skill_agent` (this crate maps a host-parsed `SkillManifestSummary`). Kept here: the `Waker::noop` form of `block_on_sync`, the `data/coding` fixture paths, and the newer `completion` module documentation.

## 0.1.2 (unpublished)

- Requires `rightkit-agent-engine-protocol` 0.2.0, whose approval records carry the approvals core's `Binding` (mandatory digest) instead of an optional `payload_digest`. Decision 4 of the 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/README.md, "Agent engine") taken on the recommended option, pending the owner's confirmation; design note: docs/audits/2026-11-11-shape-matrix/reports/agent-engine-contract-design.md.

## Unreleased

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Synced `coding` with CodeRight: a failed verification is `Rejected` (not `PartiallyComplete`), a refused spawn is a terminal task failure for `RecoveringWorker`, and `resolve_role_pack_on_single_model` resolves a pack on the one model the user selected.
- Feature `coding` (+ `coding-host`, `coding-os-keyring`): module `coding`, multi-agent orchestration for coding agents (registry, router, conductor, jury, governance, teams, recovery, proof harnesses) moved from CodeRight `coderight-orchestration`. Off by default; the existing API is unchanged.
- Skills come from the host: `coding::skill_agent` maps a host-parsed `SkillManifestSummary` (from `rightkit-agent-host-traits`) onto an agent descriptor and does not parse skills itself.

## 0.1.0 (unreleased)

- First RightKit release, moved from `legion-runtime` (engine).
