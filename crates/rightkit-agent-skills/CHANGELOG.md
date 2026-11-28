# Changelog

## 0.2.0

A superset of the CodeRight private crate `coderight-skills` (its HEAD), with product names taken from the host at use. Breaking: the crate had no consumer, so the command table, the fail-closed parser rules and the metadata key changed in place. The unit tests of `coderight-skills` are not carried over (end-to-end tests only).

**Host names.** The crate depends on `rightkit-agent-config` (0.1.6, no features). The command table, its handler texts and the generated goals carry no product name: `{cli}` stands for `rightkit_agent_config::host_slug()` (the command-line name: `rightkit` for the default host, `coderight` for a host named `CODERIGHT`), `{app}` for `rightkit_agent_config::app_name()` (`Agent` by default, `CodeRight`) and `{state_dir}` for `rightkit_agent_config::state_dir_name()` (`.rightkit`, `.coderight`). They are read at every use, never cached, so a host that names itself at start-up is honoured.

**Metadata key.** A `SKILL.md` carries the host's fields under `metadata: <key>:`. The key is `host_slug()`, resolved at parse time: `coderight` for a host named `CODERIGHT`, exactly as `coderight-skills` read it. The default host (slug `rightkit`) reads `rightkit` and, as 0.1.0 did, `rightkit-agent`. A native `+++` TOML manifest reads `[metadata.<key>]` the same way; an explicit `[metadata.host]` wins.

Public changes:
- `CommandSpec`: `summary: &'static str` and `usage: &'static str` become `summary_template` and `usage_template` (the text with `{cli}` / `{app}` placeholders); new `CommandSpec::summary()` and `CommandSpec::usage()` return `Cow<'static, str>` with the names filled in.
- New `commands::render_host_names(template: &str) -> String`.
- `CommandInfo`: `description` and `usage` are `String` (were `&'static str`), filled in when `CommandRegistry::list` runs. `CommandDefinition` is unchanged; its `description` and `usage` may carry the placeholders.
- `CommandAction::CompactContextWith(String)` added; `/compact [instructions]` returns it for non-blank text.
- The command table is CodeRight's: new rows `/preset`, `/side` (`/btw`), `/tree`, `/browser-consent`, `/observability` (`/obs`), `/raw`, `/ci`, `/changes`, `/reload`, `/recap`, `/skill-doctor`, `/pwd`, `/ps`, `/statusline`, `/experimental`, `/debug-config`, `/warnings`, `/daemon`, `/rollout`, `/feedback`; aliases `/title` (`/rename`) and `/subagents` (`/agents`); `/fork back [N]`; `/permissions [mode]`; `/clone` at Daemon scope without arguments (was Session scope with arguments); `/commit` is one approval-gated turn (`CommandAction::SubmitMessage`, no push) instead of a conductor audit-and-push goal; `/diff` summary says it opens the diff viewer; `/plugin` and `/plugins` no longer mention a marketplace (`/plugins [ui [slot]]`).
- The `/install` guidance no longer names a development command of one product.
- New `manifest::read_bounded_skill_source(path) -> Option<String>`.
- New module `shared_roots` with `SHARED_SKILL_DIR`, `PROJECT_SKILL_DIRS`, `ActivationStore`, `ProjectSkillCandidate`, `user_shared_skill_dir`, `user_claude_skill_dir`, `project_skill_roots`, `project_shared_skill_dir`, `skill_digest`, `discover_project_skill_candidates` and `materialize_activated_project_skills` (all re-exported at the crate root). New dependency `rightkit-fs` 0.5.0 (feature `digest` only).
- New `trigger::parser::MAX_TRIGGER_TOKENS` (1024) and `MAX_TRIGGER_NESTING` (64).

Unconditional hardening (stricter for every host):
- Skill discovery reads a file only when it is a regular file no larger than `MAX_SKILL_SOURCE_BYTES`; FIFOs, devices and oversize files are skipped. Discovery visits directory entries in name order, so the 256-candidate cap keeps the same files on every run.
- `enabled` is true only for `true`, `yes`, `on` or `1` (any case); anything else, including a typo, disables the skill. It was enabled for anything but `false`.
- `lifecycle` with an unrecognised value is `Quarantined` (was `Active`). The `status` key, which other ecosystems use for their own meaning, changes nothing when its value is not a lifecycle name, and never overrides a `lifecycle` line in the same manifest (a later `status: active` re-activated a skill that governance had quarantined by rewriting `lifecycle`).
- A project `SKILL.md` is opened without following a link at the final component (`rightkit_fs::open_read_nofollow`, feature `fs` of `rightkit-fs`), so a symlink swapped in after the file-type check is not read.
- YAML block scalars (`>`, `|`) and quoted values that span lines are folded before the line reader sees them.
- `allowed-tools` is accepted as a spelling of `allowed_tools`.
- A trigger with more than 1024 tokens, or nested deeper than 64 levels of `(` and `not`, is a parse error instead of a stack overflow.
- `materialize_activated_project_skills` returns the error when it cannot clear its output folder (it ignored it), so a de-activated skill is never left staged.

Fixes: the module attribute `#![allow(clippy::empty_line_after_doc_comments)]` is gone; the `SkillManifest` doc comment sits on `SkillManifest` (it was on `SkillEffects`); the `PathAffinityRouter` doc example names the crate.

## 0.1.0

- First RightKit release, moved from `coderight-skills` (CodeRight).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.
