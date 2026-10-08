# @rightkit/ax

Agent-experience (AX) conformance gates and agent-plugins.org packaging checks for Right Suite products. One CLI, `right-ax`.

| Item | Value |
|---|---|
| Where it runs | Dev and test tooling (Node, CLI and library) |
| Rust pair | None |
| Version | see INDEX.md |
| License | MIT OR Apache-2.0 |

## What it does

- `right-ax plugin init|validate`: scaffold and validate an agent-plugins.org layout (`plugin.json`, `skills/<name>/SKILL.md`, `mcp.json`).
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

Bin: `right-ax`. No runtime dependencies (Node builtins only). No peer dependencies.

## Example

```sh
pnpm add -D @rightkit/ax
pnpm exec right-ax --version
```

```js
import { loadManifest } from "@rightkit/ax/manifest";
```

## Known issues

- Only the subpaths in `exports` are importable. `adopt.mjs`, `ax/artifact.mjs` and `lib/version.mjs` are used by the CLI but are not exported, so deep imports of them are blocked.
