# Changelog

## 0.3.2

Additive. Existing source lists, commands and exports behave as before, except that `hosts.<host>.local` is now accepted for Claude too.

- `marketplace build`: `hosts.<host>.local: "./plugins/<name>"` lists a plugin vendored inside the marketplace repository, for either host. Claude gets the relative-path source string (`"source": "./plugins/<name>"`, as in Pulse's and Legion's own marketplace files), Codex keeps `{ "source": "local", "path": ... }`. A repository that vendors its plugins (the new `Orthic-Labs/right-plugins`) can now generate both manifests instead of hand-editing the Claude one, so `marketplace validate` passes with drift checking.
- `local` is refused unless it is a `./` relative path inside the marketplace repository: no absolute path, no drive letter, no `..`, no backslashes, no empty segment. `url` and `ref` are required only for an entry that has a host without `local`; `path` is optional on a `local` host. `sha` never applies to a local source.
- `marketplace validate` resolves a `local` host inside the marketplace repository (`--workspace` when that is the repository, else `--out`), refuses a symlink that leaves it, and runs `plugin validate` and the name/version identity checks on it like any other listed plugin. Because it is a vendored copy, validation no longer needs a checkout of another repository for it.
- `--workspace` accepts the marketplace repository root itself (the directory the source list lives under). Remote entries are then looked up in the root when it is that repository, then beside it (`<root>/../<repo>`), as well as under the root. A directory of checkouts works as before.
- New exports: `workspaceIsMarketplaceRoot`. Tests in `marketplace.test.mjs`.

## 0.3.1

Fixes to argument parsing across the CLI. No new commands; exports unchanged.

- `--help` or `-h` anywhere after a command prints that command's usage and exits 0 without touching the filesystem. Previously `plugin init --help` treated `--help` as the target directory and wrote `plugin.json` and `skills/rightkit-shared/SKILL.md` into the current directory.
- Every subcommand now declares its options. An unknown flag, a stray positional, or a target starting with `-` exits 2 before any write.
- `plugin init` requires an explicit target directory; it no longer defaults to the current directory. It creates a missing target (recursively), removing the directory again if scaffolding then fails. It refuses to write into a directory that already has `plugin.json` unless `--force` is given.
- `adopt <repo-path>` now uses the positional target. Previously the target was ignored and the current directory was adopted. `adopt --dry-run` as the last argument no longer writes `.rightax.json`.
- `plugin init` on a missing directory now succeeds instead of exiting 1 with "repo path does not exist".
- New tests in `plugin-init-args.test.mjs`; `cli.test.mjs` replaces the missing-path test.

## 0.3.0

Additive. Existing commands, exports and the `ax static` gates behave as before.

- `plugin init --cli <program> --skill-description <text>`: scaffolds a CLI-skill plugin (one skill teaching the command line, no `mcp.json`) with `plugin.json`, `.claude-plugin/plugin.json` and `.codex-plugin/plugin.json`. `--commands a,b` seeds the command list. `--mcp-command` stays optional on top. Scaffolding throws when the description fails the gate and writes nothing. Without `--cli`, `plugin init` is unchanged.
- Hooks: `plugin init --hook-args "<subcommand>" [--hook-events E1,E2]` scaffolds `hooks/hooks.json` and a shim that stays silent when the product is absent. `plugin validate` validates `hooks/hooks.json` (structure, script existence and containment, no absolute machine paths, no secrets; bare PATH programs are info). New module `@rightkit/ax/plugin/hooks`.
- Skill description gate in `plugin validate` (module `@rightkit/ax/plugin/skill-gate`): one line, 40..1024 characters, opens with what the tool does, at least two concrete trigger phrases, no placeholder or vague text, valid YAML. `ax static` rule 13 keeps its original layout-only scope (`descriptionGate: false`). Library callers pass `{ descriptionGate: false }` to skip it.
- `plugin validate` understands host-native layouts (only `.claude-plugin/plugin.json`, as in `pulse/plugins/claude/pulse`, or only `.codex-plugin/plugin.json`), checks host manifest names against `plugin.json`, ignores `skills/_*` and `skills/.*` directories, and reports `layout` in its output.
- `marketplace build|validate`: generate the suite marketplace (`.claude-plugin/marketplace.json`, plus `.agents/plugins/marketplace.json` for locally vendored plugins) from `marketplace/plugins.json`, check drift, and verify every listed plugin resolves (`--workspace` or `--fetch`) and passes `plugin validate`. New module `@rightkit/ax/plugin/marketplace`.
- New tests: `skill-gate.test.mjs`, `plugin-hooks.test.mjs`, `plugin-cli-skill.test.mjs`, `marketplace.test.mjs`. `plugin.test.mjs` round trip now uses a description that passes the gate.
