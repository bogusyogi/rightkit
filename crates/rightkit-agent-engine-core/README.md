# rightkit-agent-engine-core

Agent engine core: the agent turn loop, tool dispatch, permission and approval flow, compaction, goals, guardian and worker agents

Moved into RightKit from `coderight-engine` (CodeRight) on 2026-11-01.

Host behaviour that is a choice rather than a fix (what compaction keeps and when it triggers, how a todo result is trusted, tool-result clearing before the first compaction, whether an ABAC allow authorizes a call, which verifiers run after an edit, how long a read result stays inline, how loop counters reset) is set once at start-up through `host_options::set_*`; every option is off until the host sets it (the CHANGELOG lists what each one selects). Host names come from `rightkit-agent-config` (`set_app_name`, `set_state_dir_name`, `set_env_prefix`).
