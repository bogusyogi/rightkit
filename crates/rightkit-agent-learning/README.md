# rightkit-agent-learning

A coding agent's post-session learning pipeline (CodeRight's): extracts proposal candidates from a finished session, evaluates and promotes them into a sink, keeps instincts, detects regressions, and holds evaluation-gated workflow proposals, skill optimisation runs, skill synthesis and the SkillOpt meta-skill.

Moved into RightKit from `coderight-learning` (engine) on 2026-11-01 as `rightkit-adapt` 0.1.x; renamed on 2026-11-11, because `rightkit-adapt` is now Pulse's governed behavioural learning (taste and insights), the reference for that name. The two never shared a type. The harness-radar watch-list feed, CodeRight's backlog intake, moved to `rightkit-agent-server`, its only user.
