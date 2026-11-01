# Changelog

## 0.1.1 (unreleased)

- Synced `coding` with CodeRight: a failed verification is `Rejected` (not `PartiallyComplete`), a refused spawn is a terminal task failure for `RecoveringWorker`, and `resolve_role_pack_on_single_model` resolves a pack on the one model the user selected.
- Feature `coding` (+ `coding-host`, `coding-os-keyring`): module `coding`, multi-agent orchestration for coding agents (registry, router, conductor, jury, governance, teams, recovery, proof harnesses) moved from CodeRight `coderight-orchestration`. Off by default; the existing API is unchanged.
- Skills come from the host: `coding::skill_agent` maps a host-parsed `SkillManifestSummary` (from `rightkit-agent-host-traits`) onto an agent descriptor and does not parse skills itself.

## 0.1.0 (unreleased)

- First RightKit release, moved from `legion-runtime` (engine).
