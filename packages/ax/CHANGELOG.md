# Changelog

## 0.3.0

Additive. Existing commands, exports and the `ax static` gates behave as before.

- `plugin init --cli <program> --skill-description <text>`: scaffolds a CLI-skill plugin (one skill teaching the command line, no `mcp.json`) with `plugin.json`, `.claude-plugin/plugin.json` and `.codex-plugin/plugin.json`. `--commands a,b` seeds the command list. `--mcp-command` stays optional on top. Scaffolding throws when the description fails the gate and writes nothing. Without `--cli`, `plugin init` is unchanged.
- Hooks: `plugin init --hook-args "<subcommand>" [--hook-events E1,E2]` scaffolds `hooks/hooks.json` and a shim that stays silent when the product is absent. `plugin validate` validates `hooks/hooks.json` (structure, script existence and containment, no absolute machine paths, no secrets; bare PATH programs are info). New module `@rightkit/ax/plugin/hooks`.
- Skill description gate in `plugin validate` (module `@rightkit/ax/plugin/skill-gate`): one line, 40..1024 characters, opens with what the tool does, at least two concrete trigger phrases, no placeholder or vague text, valid YAML. `ax static` rule 13 keeps its original layout-only scope (`descriptionGate: false`). Library callers pass `{ descriptionGate: false }` to skip it.
- `plugin validate` understands host-native layouts (only `.claude-plugin/plugin.json`, as in `pulse/plugins/claude/pulse`, or only `.codex-plugin/plugin.json`), checks host manifest names against `plugin.json`, ignores `skills/_*` and `skills/.*` directories, and reports `layout` in its output.
- `marketplace build|validate`: generate the suite marketplace (`.claude-plugin/marketplace.json`, plus `.agents/plugins/marketplace.json` for locally vendored plugins) from `marketplace/plugins.json`, check drift, and verify every listed plugin resolves (`--workspace` or `--fetch`) and passes `plugin validate`. New module `@rightkit/ax/plugin/marketplace`.
- New tests: `skill-gate.test.mjs`, `plugin-hooks.test.mjs`, `plugin-cli-skill.test.mjs`, `marketplace.test.mjs`. `plugin.test.mjs` round trip now uses a description that passes the gate.
