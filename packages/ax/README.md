# @rightkit/ax

Agent-experience (AX) conformance gates and agent-plugins.org packaging validation for the Right/Orthic product suite. One CLI, `right-ax`.

## What it does

- **`right-ax plugin init|validate`** — scaffold and validate an [agent-plugins.org](https://agent-plugins.org) `1.0.0` layout (`plugin.json`, `skills/<name>/SKILL.md`, `mcp.json`) against vendored, sha256-pinned spec schemas. Validation enforces the spec's path-containment rules.
- **`right-ax ax static`** — a 14-rule contract checklist over an MCP surface (strict input schemas, wire-level when-to-use guidance, typed errors, effect declarations, claim-boundary separation, plugin-manifest validity, and more).
- **`right-ax ax conformance`** — spawn a product's MCP server over stdio and probe it: unknown-field rejection, missing-required handling, `structuredContent` vs advertised `outputSchema`, no stack-trace leakage.
- **`right-ax ax behavioral`** — run scenario suites against the surface.
- **`right-ax ax report`** — aggregate to JSON, SARIF 2.1.0, or markdown.

## Core discipline

`right-ax` **gates and reports; it never signs off.** The top-level field is `gateResult` (`pass`/`fail`), never a "clean"/"verdict"/"signoff" claim. If any finding fails at any depth, in any output format including SARIF, the report cannot read as pass and the exit code is non-zero. Verdict authority stays with a human or a designated audit authority — this tool produces evidence, not authorization.

## Known limitations at 0.1.0 (read before relying on results)

- **Behavioral results are UNPROVEN unless a real multi-agent matrix ran.** The bundled `--driver stub` proves harness plumbing only, not real-agent AX reliability. A real matrix requires ≥2 distinct declared agents with ≥5 trials each; **no real-agent driver ships in 0.1.0**, so behavioral output is labelled `UNPROVEN` by construction and must not be read as proven coverage. Wiring a real driver depends on an unresolved design decision (agent-matrix composition).
- **Conformance probes the source tree, not a verified packaged artifact.** Containment is enforced (symlink-aware realpath), but the tool does not assert that the probed `mcp.serve` command is the built tarball / installed bin rather than a source checkout. "Conformance passed" therefore means "passed for the code at this path," not "for the artifact a user installs."

Both limitations fail closed (the tool never falsely claims what it did not verify); they are disclosed here so consumers do not infer coverage the tool did not perform.

## Install

```sh
npm install -g @rightkit/ax   # or: npx @rightkit/ax --help
```

Node builtins only; no runtime dependencies. Public repos only for CI use — see `@rightkit/git`.
