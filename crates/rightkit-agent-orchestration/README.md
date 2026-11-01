# rightkit-agent-orchestration

Agent orchestration runtime: plans, routes, tasks, budgets, grants, escalation, scheduling, delivery, inventories and platform layers

Moved into RightKit from `legion-runtime` (engine) on 2026-11-01.

## Feature `coding`

Off by default. Adds module `coding`: multi-agent orchestration for coding agents (agent registry, router, conductor, jury, governance, teams, recovery, router proof harnesses). Its names live under `coding::` because they overlap the runtime modules above. `coding::skill_agent` takes a `SkillManifestSummary` from `rightkit-agent-host-traits`; the host owns skill parsing and installs a `SkillCatalog`.
