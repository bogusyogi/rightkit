# @rightkit/ax

Agent-experience (AX) conformance gates and agent-plugins.org packaging validation for the Right/Orthic product suite. One CLI, `right-ax`.

## What it does

- **`right-ax plugin init|validate`** — scaffold and validate an [agent-plugins.org](https://agent-plugins.org) `1.0.0` layout (`plugin.json`, `skills/<name>/SKILL.md`, `mcp.json`) against vendored, sha256-pinned spec schemas. Validation enforces the spec's path-containment rules.
- **`right-ax ax static`** — a 14-rule contract checklist over an MCP surface (strict input schemas, wire-level when-to-use guidance, typed errors, effect declarations, claim-boundary separation, plugin-manifest validity, and more). MCP surfaces require `--artifact-root <repo-relative clean-room artifact>`.
- **`right-ax ax conformance`** — bind a product's MCP server to that verified artifact, spawn it over stdio, and probe unknown-field rejection, missing-required handling, `structuredContent` versus advertised `outputSchema`, and stack-trace leakage. Adapter JSON records resolved artifact root and SHA-256 digest with every probe result.
- **`right-ax ax behavioral`** — run scenario suites against the surface; `--matrix` requires `--artifact-root` and an explicitly configured real-agent matrix.
- **`right-ax ax adversarial`** — bind adversarial cases and deterministic canaries to artifact digest, then run them against advertised MCP tools.
- **`right-ax ax report`** — aggregate to JSON, SARIF 2.1.0, or markdown.

## Core discipline

`right-ax` **gates and reports; it never signs off.** The top-level field is `gateResult` (`pass`/`fail`), never a "clean"/"verdict"/"signoff" claim. If any finding fails at any depth, in any output format including SARIF, the report cannot read as pass and the exit code is non-zero. Verdict authority stays with a human or a designated audit authority — this tool produces evidence, not authorization.

## Evidence boundaries in 0.2.0

- **Stub is not matrix truth.** Stub runs prove harness plumbing only. Only manifest-configured adapters with distinct execution identities and five trials each can earn matrix-backed behavioral evidence.
- **Artifact-root is mandatory for MCP gates.** `static`, `conformance`, `adversarial`, matrix behavioral runs, and reports that run those gates require an external clean-room directory: an absolute path or repo-relative path resolving outside source repo. AX records its SHA-256 digest and verifies it stayed unchanged.
- **Adversarial evidence is separate.** `ax adversarial` executes declared cases with per-case deterministic canaries tied to artifact digest. It reports pass or fail evidence; it never converts coverage into signoff.

Both limitations fail closed (the tool never falsely claims what it did not verify); they are disclosed here so consumers do not infer coverage the tool did not perform.

## Install

```sh
npm install -g @rightkit/ax   # or: npx @rightkit/ax --help
```

Node builtins only; no runtime dependencies. Public repos only for CI use — see `@rightkit/git`.
