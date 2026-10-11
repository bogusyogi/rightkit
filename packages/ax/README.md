# @rightkit/ax

Agent-experience (AX) conformance gates and agent-plugins.org packaging checks for Right Suite products. One CLI, `right-ax`.

| Item | Value |
|---|---|
| Where it runs | Dev and test tooling (Node, CLI and library) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## What it does

- `right-ax plugin init|validate`: scaffold and validate an agent-plugins.org layout (`plugin.json`, `skills/<name>/SKILL.md`, optional `mcp.json`, optional `hooks/hooks.json`) and the Claude (`.claude-plugin/`) and Codex (`.codex-plugin/`) host manifests. `plugin validate` includes the skill description gate.
- `right-ax marketplace build|validate`: generate and check the one suite marketplace from `marketplace/plugins.json`. See "Suite plugins".
- `right-ax ax static`: static contract checks over an MCP surface. MCP gates need `--artifact-root`, an external clean-room directory.
- `right-ax ax conformance`: spawn the product's MCP server over stdio and probe it.
- `right-ax ax behavioral`: scenario suites. `--matrix` needs a configured real-agent matrix.
- `right-ax ax adversarial`: declared adversarial cases bound to the artifact digest.
- `right-ax ax report`: aggregate results as JSON, SARIF 2.1.0 or Markdown.
- `right-ax ownership-review`, `right-ax adopt`: ownership review packets and repository adoption.

`right-ax` gates and reports. It never signs off. The top-level field is `gateResult`.

## Entry points

| Import | Module |
|---|---|
| `@rightkit/ax/manifest` | `manifest.mjs` (`loadManifest` and related) |
| `@rightkit/ax/ax/static` | Static checks |
| `@rightkit/ax/ax/conformance` | Conformance probes |
| `@rightkit/ax/ax/behavioral` | Behavioral scenarios |
| `@rightkit/ax/ax/adversarial` | Adversarial cases |
| `@rightkit/ax/ax/ownership-review` | Ownership review packets |
| `@rightkit/ax/ax/report` | Report aggregation |
| `@rightkit/ax/plugin/scaffold` | Plugin scaffolding |
| `@rightkit/ax/plugin/validate` | Plugin layout validation |
| `@rightkit/ax/plugin/portable-core` | Portable plugin core |
| `@rightkit/ax/plugin/hooks` | Hook validation and scaffolding |
| `@rightkit/ax/plugin/skill-gate` | Skill description gate |
| `@rightkit/ax/plugin/marketplace` | Suite marketplace build and validate |

Bin: `right-ax`. No runtime dependencies (Node builtins only). No peer dependencies.

## Example

```sh
pnpm add -D @rightkit/ax
pnpm exec right-ax --version
```

```js
import { loadManifest } from "@rightkit/ax/manifest";
```

## Suite plugins

Agent plugins for the Right suite are RightKit's responsibility, through `right-ax`. One plugin per tool an agent should drive; one marketplace for the whole suite.

### Add your app's plugin

A plugin's core is ONE skill that teaches an agent the tool's command line. Run this in the app repository, in the directory that will ship the plugin:

```sh
pnpm exec right-ax plugin init . \
  --cli scraperight \
  --skill-description "Scrape web pages into clean markdown with the scraperight CLI; use when the user asks to scrape a URL, crawl a site, or extract page content." \
  --commands scrape,crawl \
  --version 0.1.0
pnpm exec right-ax plugin validate .
```

This writes `plugin.json` (agent-plugins.org), `.claude-plugin/plugin.json`, `.codex-plugin/plugin.json` and `skills/<name>/SKILL.md`. It writes no `mcp.json`. The generated skill body is a seed: it tells the agent to check the binary is installed and to read `--help`. Replace it with the real commands, flags, output shapes and exit codes of your CLI, and keep every command in it true for the installed app, not a dev checkout. Scaffolding throws if the description fails the gate, so a vague description cannot be scaffolded.

### The description rule

A skill is used only if its one-line description matches the request, so `plugin validate` gates it. It fails when any of these hold:

1. No frontmatter `name` and `description`, or `name` differs from the skill directory.
2. The description is not one physical line (block scalars and continuation lines fail), or an unquoted value contains `: ` or ` #`, or a quote is unclosed.
3. It is shorter than 40 or longer than 1024 characters. Over 300 characters is a warning: hosts truncate the tail, where trigger phrases sit.
4. It does not start with what the tool does: the first word is an article, `this`, `use`, `for`, or a generic noun (`tool`, `skill`, `plugin`, `helper`, `utility`, `wrapper`).
5. It names fewer than two concrete trigger phrases: a marker (`use when`, `use for`, `when the user`, `triggers on`, `invoke when`, `asks to`) followed by at least two distinct phrases, or two quoted phrases.
6. It contains placeholder or vague text: TODO/TBD/lorem, `<...>`, `[...]`, `{{...}}`, `describe this`, `and more`, `etc`, `various tasks`, `general purpose`, `helps with`, the scaffold fallback, or fewer than six distinct words.

Pass example (the Pulse skill): `Message AI chats on paired machines through Pulse chat; use when the user mentions another machine, the Dell/Mac, a chat elsewhere, or asks to tell/ask a session on another computer.`

The gate checks form, not truth. Test a new description by asking whether a real request would match its trigger phrases.

### When hooks or MCP are justified

- Hooks only for a tool that pushes into a chat (Pulse, Legion). Add `--hook-args "<subcommand>"` (and optionally `--hook-events Stop,PostToolUse`) to scaffold `hooks/hooks.json` plus a shim that exits 0 silently when the product is not installed. The scaffold writes the Claude form (`${CLAUDE_PLUGIN_ROOT}`); a Codex hooks file (`${PLUGIN_ROOT}`, `commandWindows`) is validated but not scaffolded. `plugin validate` requires every script a hook names under a plugin-root placeholder to exist inside the plugin, rejects absolute machine paths in commands and user-home paths in scripts, and rejects secret-shaped text. A bare PATH program (Legion's `legion-hook`) is reported as info: the install prerequisite is yours to document.
- MCP only when typed calls are really needed. Pass `--mcp-command "<serve command>"` in addition to `--cli`. Otherwise the CLI skill is the whole plugin.

### How it lands in the suite marketplace

`marketplace/plugins.json` lists plugin sources: git URL, `ref`, optional `sha`, `version`, and a `path` per host. An entry appears only when its plugin exists and is pushed. Add yours, then from the repository root:

```sh
pnpm exec right-ax marketplace build
pnpm exec right-ax marketplace validate --workspace <dir holding one checkout per repo>   # or --fetch
```

`build` writes `.claude-plugin/marketplace.json` (the path Claude Code reads in a marketplace repository; `build --check` reports drift). A Codex `.agents/plugins/marketplace.json` is written only for plugins vendored in this repository (`hosts.codex.local`), because only the `local` source is evidenced in Pulse. `validate` checks the list, that the generated file is current, and that every listed plugin resolves and passes `plugin validate` on every host path; its manifest name must equal the entry name and its version the entry version. `--fetch` clones the listed `ref` (or `sha`) from the network; `--workspace` validates working trees and says so. `knownIssues` on an entry waives plugin-validate errors visibly (never identity or resolution errors) until the plugin's owner fixes them.

Users add the whole suite once: `/plugin marketplace add Orthic-Labs/rightkit`, then install any listed plugin.

Remote sources in the Claude manifest use `{ "source": "url" | "git-subdir", ... }`. These two shapes are from Claude Code's marketplace documentation, not from a file in this workspace (Pulse and Legion use relative-path sources). Confirm with a real `/plugin marketplace add` before relying on it.

## Known issues

- Only the subpaths in `exports` are importable. `adopt.mjs`, `ax/artifact.mjs` and `lib/version.mjs` are used by the CLI but are not exported, so deep imports of them are blocked.
