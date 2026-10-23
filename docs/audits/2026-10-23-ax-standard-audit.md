# Agent Experience standard (AXA-1) in RightKit: where it applies, how well it is implemented, and whether it needs a refresh

Date: 2026-10-23. Author: Claude Code cloud session (read-only audit; nothing in RightKit was changed except this file). Standard audited against: "Agent Experience (AX) — research foundation, engineering principles, audit standard" v1.0, 2026-08-06 (AXA-1), SHA-256 `9e4181c70b64eba86332cac5e234d993e1f7bf67c63a6a4c8039804c6d68878c`, identical to the copy in `claude/docs/plans/legion/legion-complete-master-archive-v1.2/.../source-material/`. RightKit at `77ade28` (branch `claude/great-hawking-dcwyqn`, equal to `main` at the time of the audit).

Method: I read `packages/ax` in full and reproduced its defects with small scripts against the package source. Four reviewers ran in parallel: `rightkit-mcp`, `rightkit-agent-tools` with `rightkit-agents`, and the agent-facing Node CLIs were read-only code reviews (the CLI reviewer also ran CLIs in scratch repositories); the fourth checked AXA-1's references against primary web sources. I re-read every high finding they reported in source before including it, and checked the headline MCP spec facts at modelcontextprotocol.io myself. No Rust was built. Severity follows AXA-1 §6.9. "Verified" means the exact code path was read or the behaviour was reproduced; "plausible" means the path exists but the full scenario was not executed.

## 1 Summary

**Where AXA-1 applies.** It governs RightKit's agent-facing surfaces (section 2): the auditor itself (`packages/ax`, `right-ax`), the MCP host every app uses (`rightkit-mcp`), the agent tool registry and CLI (`rightkit-agent-tools`, `right-ctl`), supervised agent sessions (`rightkit-agents`), and the Node CLIs that AGENTS.md tells agents to run. It does not apply directly to runtime crates with no agent surface. Only `packages/ax` implements the standard, and nothing runs it: no `.rightax.json` exists in RightKit or the workspace repository, its one recorded consumer is Legion (devDependency), and RightKit does not audit its own agent surfaces with it.

**Is it properly implemented?** Partly. `right-ax` is a careful MCP stdio contract linter with honest "stub is UNPROVEN" labelling, a clean-room artifact digest and fail-closed aggregation, and its 268 tests pass. Measured against AXA-1 it has six high findings: the behavioral matrix hands the agent the answer key (AX-01), trusts the agent's self-reported final state (AX-02) and matches claims as exact substrings (AX-03), so it cannot support a behavioral (AX4) claim; CLI surfaces, the suite's default plugin shape, are not audited and a CLI-only product gets `Gate result: PASS` with zero operations inspected (AX-04); conformance calls destructive tools with the auditor's full environment (AX-05); and `tools/list` pagination is ignored (AX-06). It has no Phase A scope freeze, no Phase F reconciliation, none of the §7 metrics and none of the §14 profiles.

**Other RightKit components.** 5 more high findings and 26 medium across the agent-facing crates and CLIs, notably: `ReadOnlyPolicy` lists tools it will always refuse (MCP-H1); `right-ctl` lets the agent confirm its own risky desktop actions (AT-H1) and reports a failed `batch` as success (AT-H2); `right-git uninstall --dry-run` deletes files (CLI-H1). `rightkit-mcp` does not validate tool input, which the current MCP spec makes a MUST, and supports neither of the two latest MCP revisions.

**Does the standard need a refresh?** Yes (section 7). The MCP revision it cites as R6/R7 (2025-11-25) was superseded by 2026-07-28 nine days before AXA-1 was dated; the new revision removes the `initialize` handshake, makes tasks an extension and allows any JSON Schema 2020-12 keyword in tool schemas, so §12.2 and Principle 12 describe an obsolete protocol. The rest is new evidence to add (tool search and deferred loading, overclaim and false-success research, adaptive prompt-injection evaluation, MiniScope v2, τ-bench 2.0). The implementation also needs a refresh against new data inside the repository: the suite moved its plugins to CLI skills, and `right-ax` still audits only MCP.

Counts by component (high / medium / low-or-info):

| Component | High | Medium | Low/info |
|---|---|---|---|
| `packages/ax` | 6 | 7 | 5 |
| `rightkit-mcp` | 1 | 7 | 8 groups |
| `rightkit-agent-tools` / `right-ctl` | 2 | 7 | 2 groups |
| `rightkit-agents` | 0 | 3 | 1 group |
| Node CLIs | 2 | 9 | 1 group |

## 2 Where AXA-1 applies in RightKit

| Surface | Why it is agent-facing | AXA-1 parts that govern it | Audited by `right-ax` today | Section |
|---|---|---|---|---|
| `packages/ax` (`right-ax`) | The AX auditor (AXA-1 "AX audit as a capability"), and itself a CLI agents run | §6, §7, §9.7-9.8, §12.1, §14 | No: it has no CLI checks and no `.rightax.json` of its own | 3 |
| `crates/rightkit-mcp` | MCP server host and client for every app that exposes or calls MCP tools; its defaults are every app's MCP agent experience | §12.2; Principles 6, 7, 11, 12, 13, 15, 17 | Could be (stdio servers), but none is configured | 4 |
| `crates/rightkit-agent-tools` (`right-ctl`) | One typed tool registry with schemas and effects; the CLI agents use to drive apps | §5.1, §5.8, Principles 5, 6, 14-17, §12.1 | No (not MCP) | 5.1 |
| `crates/rightkit-agents` | Supervised Claude Code / Codex sessions with tool approvals; the natural Phase D agent adapter | Principle 17, §6.4 D, §6.6, §9.8 | n/a | 5.2 |
| Node CLIs: `right-release`, `rightkit-gate`, `right-git`, `right-issues`, `rightkit-hooks`, `right-update` | AGENTS.md directs agents to run them for releases, gates, CI and issues | §12.1, Principles 8, 11, 13, §13.4-13.5 | No | 6 |
| Plugin and skill packaging (`right-ax plugin`, `marketplace`) | Skills are agent routing policy | §12.4 | Form only (description gate) | AX-17 |
| `AGENTS.md` and agent rules | Agent instructions | §12.4, §13.18 | No | CLI-M9 |
| `contracts/` | Strict, versioned JSON Schemas for the content pipeline and daemon | §10 Phase 1 asks for a shared operation descriptor, result envelope, error envelope and effect profile; none exists here or elsewhere in RightKit, although `right-ax` rule 5 checks other products for a `claimBoundary` shape | n/a | this row |
| Runtime crates with no agent surface (audio, ffmpeg, crypto, db, ...) | Not agent-facing | Only through an app's agent surface | n/a | out of scope |

Not reviewed in this audit: the in-app control server in `rightkit-qa` and `rightkit-automation` beyond the paths `right-ctl` reaches, `rightkit-browser`, `rightkit-daemon`, `rightkit-llm`, and every crate without an agent surface. Earlier audits in `docs/audits/` cover fs, http, pdf, process and secrets in depth.

## 3 `packages/ax` (`@rightkit/ax` 0.3.2, `right-ax`) against AXA-1

### 3.1 What it implements

| AXA-1 element | Status in 0.3.2 | Evidence |
|---|---|---|
| Phase A scope freeze, `ax-plan.json` (§6.4 A, §9.5) | Missing. Digests are computed at run time and recorded, never sealed in advance. No repository revision, dirty digest, `tools/list` catalog digest, documentation digest or policy digest. | `ax/behavioral.mjs` (`scenarioDigest`, `matrixDigest` only), `ax/report.mjs:61-73` |
| Phase B static contract (§6.5) | Partial: 14 lexical/structural rules over MCP `tools/list` page 1 plus plugin layout. | `ax/static.mjs` |
| Phase C executable conformance (§6.4 C, §12.2) | Partial: initialize, `tools/list`, 3 probes per tool (valid / missing-required / unknown-field), `structuredContent` vs `outputSchema`, stack-trace leak. No unknown-tool, wrong-type, malformed-frame, cancellation, concurrency, oversized-result, pagination or task probes. | `ax/conformance.mjs:245-286` |
| Phase D behavioral (§6.4 D, §6.6) | Harness exists (stub driver + adapter matrix with >=2 adapters, >=5 trials, limits, process-tree kill). Not independent: see AX-01 to AX-03. | `ax/behavioral.mjs`, `ax/adapter.mjs` |
| Phase E adversarial (§6.4 E) | Runner only. No built-in injection, path, oversize or secret probes; every case is product-authored. | `ax/adversarial.mjs` |
| Phase F claim reconciliation (§6.4 F) | Missing. | none |
| CLI profile (§12.1), API profile (§12.3), skill profile evals (§12.4), GUI profile (§12.5) | Missing. `surfaces.cli` is accepted by the manifest schema and never read. | `manifest.schema.json:26-30`; grep finds no reader |
| Metrics (§7.1-7.4): pass^k, first-operation accuracy, overclaim rate, routing confusion matrix | Missing. Reports carry pass/fail totals only. A matrix scenario passes only if every trial passes (strict pass^k), but no pass^1/3/5 figures are emitted. | `ax/report.mjs:219-272` |
| Result vocabulary (§9.7: pass / fail / incomplete / unproven / integrity_fail) | `gateResult` is pass/fail only; `fatal` carries the rest. CLI exits 1 for both a failing gate and a crash. | `ax/report.mjs:266`, `cli/right-ax.mjs:512-513` |
| Profiles (§14) | Missing. Which gates run is whatever `.rightax.json` lists. | `cli/right-ax.mjs` `ax report` |
| Invariant I1 (never signs off), I2 (stub is UNPROVEN) | Implemented and tested. | `ax/report.mjs:254-256`, `ax/behavioral.mjs:281-282` |
| Clean-room artifact with before/after tree digest | Implemented. | `ax/artifact.mjs` |
| Fail-closed aggregation, SARIF parity with `gateResult` | Implemented (recursive leaf scan, fallback SARIF results). | `ax/report.mjs:48-56, 149-163` |

Tests: `node --test --test-concurrency=1 *.test.mjs` in `packages/ax`: 268 pass, 0 fail (Node 22.22.0, this container, 2026-10-23).

Adoption: no `.rightax.json` exists in `rightkit` or in the workspace repository. `docs/ADOPTION.md:98,625` lists one consumer, Legion, as a devDependency. `docs/CAPABILITY-MAP.md:15` lists `agent-experience-gates` as declaration-only with 0 fingerprints, so `rightkit owns` cannot detect a product that rebuilt its own AX harness. Rollout steps R1-R5 in `claude/docs/plans/rightkit-ax/DESIGN.md:§8` have no evidence in either checkout (Cortex and Membrane are not in this session, so their state is unverified). RightKit does not run `right-ax` on its own agent surfaces (`rightkit-mcp`'s example server, `right-ctl`, the Node CLIs), so the §9.8 self-audit property is absent.

### 3.2 Findings

Each finding was reproduced in this container unless marked "read". Repro scripts: the `ax-repro/` folder in the session scratchpad (not committed); each is under 60 lines and imports the package by absolute path.

**AX-01 (high) The behavioral matrix gives the agent adapter the answer key.** `ax/behavioral.mjs:303` sends the whole `scenario` object in the adapter request, including `expected.finalEnvironment`, `requiredClaims`, `forbiddenClaims`, `requiredReasonCodes`, `requiredOperations` and `fixture.stub.trial`. The package's own test adapter, `fixtures/fake-ax-adapter.mjs`, passes by echoing `request.scenario.expected` back, and that run is reported as `provenBy: "real-matrix"`, `claimBoundary: "PROVEN"` (`ax-behavioral.test.mjs:309`). Any real adapter that puts the scenario into the model's prompt leaks the expected result into context. Violates §6.4 Phase D ("without exposing implementation internals"), §9.8 ("scenario expected states live outside the production handler path", held-out scenarios). Fix: send only `goal`, `userMessages`, `initialState`, `allowedOperations` and limits; keep `expected` in the evaluator.

**AX-02 (high) Final state is self-reported.** `evaluateTrial` (`ax/behavioral.mjs:86-90`) compares `expected.finalEnvironment` to `trial.finalEnvironment`, which is a field of the adapter's own JSON. The engine never reads filesystem, database or application state itself and has no verifier hook. Violates Principle 18 and §9.8 ("final-state checks read durable external state rather than trusting prose"). With AX-01, an AX4 claim (§7.6) cannot be supported by this harness.

**AX-03 (high) Claim fidelity is an exact-substring check.** `ax/behavioral.mjs:92-93`. Reproduced: with `requiredClaims: ["The audit is incomplete."]` and `forbiddenClaims: ["The repository is clean."]`, the answer "The audit is incomplete. Good news: the repo is clean!" passes, and the correct paraphrase "The audit did not finish, so I can't say it's clean." fails. Real agents paraphrase, so the overclaim check under-detects and the required-claim check over-fails. §7.3 says a clean-claim false positive in an auditor should be a hard release failure; this evaluator cannot see one that is reworded. Needs a semantic claim evaluator (rule plus model, recorded uncertainty, §13.15) or structured final answers.

**AX-04 (high) CLI surfaces are not audited, and a CLI-only product passes.** The suite's own plugin guidance makes the CLI skill the default plugin shape and MCP the exception (`packages/ax/README.md`, "Suite plugins"). `right-ax` has no CLI checks (§12.1) and never reads `surfaces.cli`. Reproduced: a plugin scaffolded exactly as the README says (`right-ax plugin init <dir> --cli mytool --skill-description ...`) with `.rightax.json` `surfaces.cli` and `gates.static: "hard"` produces `ax report` **Gate result: PASS** with `toolCount: 0`; rules "no semantic-duplicate tools" and "no model-selectable arbitrary filesystem root" report `pass` over an empty tool set. This is §13.5 "empty means clean", and the manifest schema advertising a surface the auditor cannot inspect is §13.2. Fix: a surface with no executable checks must yield `unproven`, never `pass`; add a CLI profile (help, unknown flag, exit taxonomy, JSON mode, stdout/stderr split, no-TTY run).

**AX-05 (high) Conformance executes destructive tools with the auditor's environment.** `ax/conformance.mjs:254-268` calls every advertised tool three times with synthetic arguments, without reading `destructiveHint` or any effect declaration. `ax/lib/mcp-stdio-client.mjs:110` spawns the server with no `env` option, so it inherits every variable of the process running `right-ax` (in CI: registry and GitHub tokens). Reproduced: a tool annotated `destructiveHint: true` was called by the conformance run, and the same server process read `FAKE_SECRET` from the inherited environment. The artifact tree digest only proves the artifact directory did not change. Violates hard gate "no unauthorized or out-of-scope effects" (§3.3) and §5.9. Fix: scrub the child environment to an allowlist, skip or sandbox tools whose declared effects are not read-only unless the manifest provides a safe example, and record the effective authority in the report.

**AX-06 (high) `tools/list` pagination is ignored.** `ax/lib/mcp-stdio-client.mjs:141-146` reads one page and drops `nextCursor`. Reproduced: a server returning `good_tool` with `nextCursor`, and `delete_everything` (destructive, unconstrained `path`, non-strict schema) on page 2, gets a static result over 1 tool; the second tool is never inspected and nothing in the report says coverage is partial. Hard gate "no clean claim when required evidence is missing" (§3.3).

**AX-07 (medium) The client speaks MCP 2025-03-26 only.** `ax/lib/mcp-stdio-client.mjs:13` sends `initialize` with 2025-03-26. `outputSchema`/`structuredContent` arrived in 2025-06-18; the current revision is 2026-07-28, which removes the `initialize` handshake in favour of per-request `_meta` and a mandatory `server/discover`, and moves tasks to the `io.modelcontextprotocol/tasks` extension (spec changelog, checked 2026-10-23). A server that honours the requested 2025-03-26 may omit `outputSchema`, which then fails static rules 4, 5 and 7 because of the auditor, not the product; whether a 2026-07-28 server answers `initialize` at all depends on its backward-compatibility support. The negotiated version is recorded but never checked. Principle 22.

**AX-08 (medium) The schema validator ignores keywords and reports "matches".** `lib/json-schema-lite.mjs` has no `$ref`/`$defs`, `oneOf`, `not`, `if/then/else`, `exclusiveMinimum`/`exclusiveMaximum`, `patternProperties`, `prefixItems` or `format` (line 6 documents part of this). Reproduced: `{x: 5}` validates against a `$ref` to a schema requiring string `x` and a missing `y`; `42` validates against `oneOf [string, boolean]`; `0` validates against `exclusiveMinimum: 0`. Zod-generated MCP schemas commonly use `$ref` and `exclusiveMinimum`, and MCP 2026-07-28 explicitly allows any JSON Schema 2020-12 keyword in `inputSchema`/`outputSchema`, with `$ref` resolution requirements. Conformance then reports "structuredContent matches advertised outputSchema" (`ax/conformance.mjs:277-278`). An unsupported keyword must make the check `unverifiable`, not `pass`.

**AX-09 (medium) Typed-error detection does not check the typed-error contract.** `ax/lib/mcp-stdio-client.mjs:218-224` recognises a typed error only from JSON inside a text block and only needs `error.code`. Reproduced: an MCP-native error carried in `structuredContent` (`isError: true`, `structuredContent.error = {code, retryable, remediation}`) is classified `untyped-error` (false fail); `{error:{code:"E"}}` with no `retryable` or `remediation` is `typed-error` (false pass). Static rule 6 defers proof of Principle 11 to these probes (`ax/static.mjs:135-138`), so retryability and remediation are never verified anywhere. The missing-required and unknown-field probes also count a JSON-RPC error as a pass (`ax/conformance.mjs:153`, `:168`), while the MCP tools page (2026-07-28, unchanged from 2025-11-25) says input-validation errors are tool execution errors with `isError: true` so the model can self-correct.

**AX-10 (medium) Effect and handle rules accept the anti-patterns AXA-1 names.** Rule 9 passes on any one boolean annotation hint (`ax/static.mjs:192`), which is §13.9 "the boolean readOnly", and MCP annotations are self-declared hints. Rule 8 recognises only the exact names `path|filePath|file|repoRoot|root|dir|directory` (`ax/static.mjs:154`), so `repoPath`, `workspaceDir`, `outputPath`, `cwd` and `uri` are not checked, and any `pattern` (including `.*`) counts as constrained. Rule 1 detects duplicates only by identical description text; §Principle 2 asks for semantic-overlap analysis.

**AX-11 (medium) Scenario budgets are not enforced in matrix mode.** The adapter protocol reports tokens as `metrics.tokens` (`ax/adapter.mjs`), but `evaluateTrial` reads top-level `trial.contextTokens` and `trial.invalidCalls`, defaulting to 0 (`ax/behavioral.mjs:102-103`). Reproduced: a trial using 50,000 tokens passes a scenario with `maxContextTokens: 10`.

**AX-12 (medium) Report integrity skips the static gate's artifact.** `reportIntegrity` compares artifact digests from conformance, behavioral and adversarial only (`ax/report.mjs:62-66`); the static result's artifact digest is not included, so static evidence from a different artifact is not detected. (read)

**AX-13 (medium) `right-ax`'s own agent experience.** Its output has no machine-readable claim boundary, the field it requires of others in rule 5; `gateResult` cannot express incomplete, unproven or integrity failure; exit code 1 means both "a gate failed" and "right-ax crashed" (`cli/right-ax.mjs:512-513`); an unknown `--format` value silently prints JSON (`cli/right-ax.mjs:435-438`, an accepted-but-ignored argument, §12.1); the Markdown scorecard prints `undefined` in the severity column for gates the manifest does not configure, and says "the stub driver proves harness plumbing only" when the behavioral gate did not run at all; the real-matrix claim text points at "retirement-condition fields" that do not exist (`ax/report.mjs:255`). Findings have no stable IDs, `operationId` or `surface` fields (§6.8), and every SARIF location is `.`.

**AX-14 (low) Adversarial runner.** No built-in probes (the design promised injection, root-escape, oversize and secret probes, `DESIGN.md §4`); case results carry no reason, so SARIF evidence is empty (`ax/adversarial.mjs`, `ax/report.mjs:162`). `ax/adversarial.mjs` and `ax/adapter.mjs` are written as minified one-line functions, unlike the rest of the package, which makes them hard to review.

**AX-15 (low) Redaction misses common credential shapes.** Reproduced as not redacted: Stripe `sk_live_...`, npm `npm_...`, GitLab `glpat-...`, Hugging Face `hf_...` (`lib/redact.mjs:26-38`).

**AX-16 (low) Dead and stale code.** `findArtifactContainmentViolation` (`ax/conformance.mjs:197-205`) is never called and references `resolvesOutsideRoot` and `describePathHazard`, which the module does not import; the F12 comments above it describe a containment model that `ax/artifact.mjs` replaced.

**AX-17 (low) The skill gate checks positive triggers only.** `plugin/skill-gate.mjs` enforces form and trigger phrases; §12.4 also asks for non-trigger conditions, adjacent-skill boundaries and should-trigger / should-not-trigger evals. The README says so ("The gate checks form, not truth").

**AX-18 (info) Version drift and standard binding.** The package never states which AXA-1 version its rules encode. Version citations disagree: `docs/PLATFORM-MATRIX.md:111` and `docs/INVENTORY-STATUS.md:79` say 0.2.4, `claude/docs/RIGHTKIT-SHARED-PACKAGES.md:42` says staged 0.2.1, `DESIGN.md` says 0.2.0; the package is 0.3.2. The standard itself is not in `rightkit`; `DESIGN.md` points at `D:\Downloads\...`, and the only versioned copy is inside the Legion master archive (same SHA-256 as the uploaded file).

### 3.3 Verdict for `packages/ax`

It is a careful implementation of a narrower thing than AXA-1: a static and conformance linter for MCP stdio servers with an honest stub/real-matrix label. The invariants it chose (no sign-off, stub is UNPROVEN, artifact clean room, fail-closed aggregation) hold and are tested. It does not implement Phase A or F, the CLI/API/skill/GUI profiles, the metrics, or the result vocabulary, and its behavioral harness cannot support an AX4 claim because the agent sees the answer key, final state is self-reported and claims are matched as substrings. Per §7.6 it can support scoped AX2 and partial AX3 statements for MCP stdio surfaces, and nothing for CLI surfaces.

## 4 `crates/rightkit-mcp` against the AXA-1 MCP profile (§12.2)

Read-only review by a delegated reviewer; nothing built or run. I re-read H1, MCP-M1, the version table and the trust fingerprint in source before including them. `docs/audits/` had no prior report of any item below.

This crate is the MCP server host and client every Right Suite app uses, so its defaults set the agent experience of every app that exposes MCP tools.

**MCP-H1 (high) `ReadOnlyPolicy` advertises tools it will always refuse.** `src/server.rs:462-464` `expose` filters only `blocked`; `admit` (`:445-460`) refuses every tool not annotated `readOnlyHint: true` when `allow_mutate` is false. So `tools/list` lists mutating tools and every call to them returns "tool 'x' is mutating and mutation is not enabled"; `tests/server_options.rs:755-766` asserts exactly this. A misspelled tool name gets the same "mutating" refusal instead of "unknown tool", which steers an agent toward asking the user to enable mutation. Admission reads descriptors only from the typed `provider.list()`, so a provider that publishes tools only through raw `list_tools` has every call refused. §12.2 "advertised tools are callable", §13.2 aspirational catalog, Principle 11.

**MCP-M1 (medium) The client replays `tools/call` after a server crash.** `src/client.rs:1324-1334` retries any request once on `McpError::Closed`, and the reader marks every pending call `Closed` when the server's stdout ends. If the effect committed before the crash, it runs twice. No idempotency key, `idempotentHint` ignored; `tests/e2e.rs:430` asserts the retry as intended. Principle 13.

**MCP-M2 (medium) No server-side input validation; failures are protocol errors.** Arguments, including non-objects, reach the provider unchecked (`src/server.rs:1766-1770`, `1817-1820`). Provider errors become JSON-RPC `-32603`/`-32602` (`:1061-1077`, `:1601-1619`), not `isError` tool results, so the model cannot self-correct. The MCP tools page says servers **MUST** validate all tool inputs and that input-validation errors are tool execution errors with `isError: true` (2026-07-28, checked 2026-10-23). The reference `examples/example_server.rs:46-49` silently returns 0 for `add {"a":"x"}` and has no `required` or `additionalProperties: false`. Principles 6, 11.

**MCP-M3 (medium) Errors are prose and can leak.** `ToolResult::error(String)` is the only error a provider can return (`src/types.rs:132-137`). `Denied`, `Timeout`, `Io`, `Transport` and `Config` all map to `-32603` with no code, category, retryable flag or remediation. `NotApproved` includes the workspace path and fingerprint (`src/error.rs:72-76`); `Transport` includes 300 characters of the upstream HTTP body (`src/client.rs:913-914`). §5.4, Principles 11, 19.

**MCP-M4 (medium) No `outputSchema`, `title`, task support or `_meta` in the tool model.** `ToolDescriptor` is name, description, `inputSchema`, annotations (`src/types.rs:34-43`). Remote servers' `outputSchema` and `execution` are dropped by the client parser (`src/client.rs:1389`). `ToolResult::json` accepts non-object `structuredContent` (`src/types.rs:138-146`), which is invalid in the 2025-06-18 revision the crate speaks (2026-07-28 allows any JSON value). Principles 7, 12.

**MCP-M5 (medium) Cancellation is incomplete.** Concurrent stdio runs calls one at a time and records a cancel only for the running request (`src/server.rs:1313-1321`, `1399-1411`), so cancelling a queued request is lost and it later runs. Over HTTP and `dispatch*` the call handle has no cancel or progress (`:136-138`). Principle 12.

**MCP-M6 (medium) Approval does not cover the code it launches, and hides injected secrets.** The trust fingerprint hashes command, args, env and cwd (`src/trust.rs:47-68`), not the script those strings run: after a `git pull` changes `./mcp/server.js`, the approval still holds. `PendingApproval` omits `cwd` (`:25-33`), and `${VAR}` expansion from the host environment into a workspace server's env (`src/config.rs:79-82`) is shown to the approver by variable name only, so `"LOG_LEVEL": "${AWS_SECRET_ACCESS_KEY}"` reads as `LOG_LEVEL` (this path has no tests or in-repo callers; plausible). A live client respawns without re-checking trust (`src/client.rs:1289-1300`). Principles 15, 17. (The reviewer rated the fingerprint gap high; I rate it medium because the README promises configuration binding, which holds, and content binding is a new design decision.)

**MCP-M7 (medium) `RawJson` can break stdio framing.** `src/raw.rs:31-33` keeps text verbatim, including newlines, and the stdio writers emit it with `writeln!` (`src/server.rs:1256`, `1287`). A pretty-printed body splits the reply across lines; the crate's own client drops unparseable lines (`src/client.rs:215`), so the call hangs to the 60 s timeout. (read; relies on `serde_json::RawValue` preserving whitespace)

**Low.**
- Version negotiation (raised to medium if apps must interoperate with current clients): the crate supports neither 2025-11-25 nor the current 2026-07-28 (stateless, `server/discover`, no `ping`, resource-not-found `-32602`, `ttlMs`/`cacheScope` on lists, `resultType` on results). `SUPPORTED_VERSIONS` is 2025-06-18, 2025-03-26, 2024-11-05, default 2025-03-26 (`src/types.rs:260-261`). A client asking for 2025-11-25 is answered 2025-03-26 rather than the newest supported 2025-06-18 (`src/server.rs:1682-1686`). `resources`/`prompts` are advertised only when non-empty but always served; `InitializeOverrides` merges unchecked capabilities.
- Client pagination has no cycle or page limit (`src/client.rs:1368-1387`).
- Plain stdio `connect` has no frame limit (`:405`); plain HTTP has no address policy (`:797-817`), unlike `advanced-http`.
- HTTP server: one thread per connection before auth (`src/server.rs:1885-1888`), `MCP-Protocol-Version` not validated, sessions never expire.
- Lenient mode drops malformed frames silently; provider panics are not caught and end stdio serving.
- Response size limits apply only in `dispatch_checked` (`src/server.rs:990-1002`); no resource-link content type.
- `ConnectionPool::get` connects while holding the slots lock (`src/pool.rs:107-130`).
- Qualified tool names have no length limit (`src/sanitize.rs:24-26`); model APIs reject names over 64 characters.

**Conforms well.** Nothing on stdout; unknown tool `-32602` and unknown resource `-32002` per spec; admission runs before every provider path (typed, raw, async, batch) and untagged tools are treated as mutating; trust gate runs before spawn, binds env values, saves atomically, fails closed on load; HTTP server is loopback-only with constant-time bearer comparison and exact Host/Origin checks (blocks DNS rebinding); input frames capped; client timeouts send `notifications/cancelled`; no redirects; cross-origin SSE endpoints refused; catalog collisions detected.

## 5 `crates/rightkit-agent-tools` (`right-ctl`) and `crates/rightkit-agents`

Delegated read-only review; nothing built. I re-read AT-H1, AT-H2 and AT-M3 (including the CHANGELOG claim) in source before including them. Prior reports: `crates-g1.md`'s duplicated `CONTROL_METHODS` is resolved. The round-3 `rightkit-agents` 0.3.0 patch (`docs/audits/2026-10-18-round3/component-pass/REPORT-agents.txt`) was never applied to this tree (manifest still 0.2.0), so its fixes (#571 ApprovalContext, #704 PATH filtering, #706 control responses, #707 rate-limit rules) are missing; AG-M2, AG-L2 and AG-L3 below are those items, still present.

### 5.1 `rightkit-agent-tools` / `right-ctl`

This crate is the closest thing in RightKit to an AXA-1 operation registry (§5.1): one `Toolbox` with JSON schemas, an `Effect` per tool, and a fixed gate order (exposure, allowlist, arguments, admission, journal).

**AT-H1 (high) The agent confirms its own risky desktop actions.** `src/backends/desktop.rs:219` fills `ActOptions.confirm` from the agent's own `confirm` argument; `rightkit-automation/src/engine.rs:37` documents that field as "risk classes the caller has confirmed with the user", and `right-ctl` installs no admission hook (`Toolbox::set_admission` is never called in `src/`). So `desktop.act` returns `needs_confirmation` and the agent retries with `confirm: ["send","delete","purchase"]`. The risk check also applies only to Press/Click/MenuPress labels (`engine.rs:1114-1118`), so a `keys` action (a send shortcut) skips it. Principle 17 (approval from deterministic data, not the model), Principle 15.

**AT-H2 (high) A failed `batch` is reported as success.** The in-app server returns `Ok({ok:false, failedStep, ...})` for a failed batch (`rightkit-qa/src/embedded/mod.rs:884-895`); `src/backends/control.rs:321` passes it through as a successful call. Exit code 0, journal `ok:true`, and `right-ctl run` continues to the next call. §5.3, Principle 8, §13.4.

**AT-M1 (medium) Authority comes from inputs the caller controls.** The desktop allowlist is `RIGHT_CTL_DESKTOP_ALLOW` from the invoking process's environment (`src/bin/right-ctl.rs:85-92`); `right-ctl` grants itself `desktop.*`, `browser.*` and the full profile of any app named in its input (`:95`, `:101`, `src/lib.rs:42-46`). `--suite-root`/`RIGHTKIT_SUITE_ROOT` select which service records and sockets are trusted, and the client does not verify the listener (`control.rs:319`); for `speech.transcribe` (labelled `Effect::Observe`) a record under that root names an executable that is spawned (`speech.rs:67`, `rightkit-speech-client/src/lib.rs:262-265`). `--journal` appends to any path. Whether an agent can use these depends on the host's permission matcher (e.g. a `Bash(right-ctl ...)` allow rule, as in `rightkit-agents/tests/claude.rs:183`); code paths verified, attack chains plausible. Principle 15 ("authority can narrow automatically but should not silently widen").

**AT-M2 (medium) HeardRight's lock is an exact string match.** `control.rs:48`, `:125`, `:170` compare the app name to `"heardright"`; the name is not validated before it becomes `control-{app}` and is joined into a path without `valid_service_name` (`rightkit-daemon/src/record.rs:84`, `suite.rs:240`). `HeardRight` on case-insensitive filesystems, or `genright/../control-heardright`, reaches HeardRight with the standard profile, which includes `eval`. Development and QA builds only (the control server is not compiled into release builds). Principles 5, 15.

**AT-M3 (medium) Argument validation is lenient, and the CHANGELOG says it is strict.** `src/toolbox.rs:336` calls `validate_args`, not the existing `validate_args_strict`, and no shipped schema sets `additionalProperties: false`, while `CHANGELOG.md:19` (0.2.0) says "Tool schemas set `additionalProperties: false`". A typo such as `buton: "right"` is ignored and produces a left click; undeclared fields are forwarded to the app (`control.rs:302`). Lease mode works only because validation is lenient, since the schemas omit `modifiers`, `leaseEpoch` and `inputSequence` that the server accepts, so `spec.rs` is not yet a canonical descriptor (§5.1). Principles 6, 24.

**AT-M4 (medium) `batch` bypasses the per-caller allowlist and admission.** The toolbox sees only `<app>.batch`; `check_batch` checks steps against the app profile, not the caller's patterns (`control.rs:360-390` vs `toolbox.rs:323`), and step parameters are not validated. A host that allows `["genright.batch","genright.click"]` still lets `eval` or `type` run inside a batch. Not reachable through `right-ctl` itself, where the patterns equal the profile.

**AT-M5 (medium) `Effect` is a five-value label, not an effect profile, and admission cannot see arguments.** `spec.rs:13-24`. `speech.transcribe` is Observe but can spawn a process and read any file; screenshots are Capture but write files; `browser.open` is Network but launches Chrome. `ToolRequest` deliberately carries no argument values (`toolbox.rs:15-25`), so an admission hook cannot base approval on the arguments, including `confirm`. §5.8, Principles 14, 17.

**AT-M6 (medium) Screenshot and transcription paths are not contained.** `desktop.screenshot` and `browser.screenshot` check only that the path is absolute (`desktop.rs:234`, `engine.rs:673`, `web.rs:135`) and can overwrite any user-writable file; the desktop capture is written unmasked and masked afterwards (`engine.rs:722`, `:745`), so a crash in between leaves unmasked pixels on disk (plausible); `browser.screenshot` is never masked; `speech.transcribe` reads any absolute path. §5.5, §13.8.

**AT-M7 (medium, non-default feature) The demo service cannot be hosted.** `DEMO_SERVICE = "rightkit.agent.demo"` (`demo.rs:29`) fails `valid_service_name` (`suite.rs:536-541`), which `host` enforces (`server.rs:561`); every `demo.*` call ends in `service_unavailable`. Tests cover only the no-host case.

**Low.** No `--help` (`right-ctl <app> --help` sends a command named `--help`, `right-ctl.rs:276-289`); a mistyped method becomes an app command; `--journal`/`--suite-root` are stripped from anywhere in argv including command arguments (`:143-155`); refusals, `needs_confirmation`, `blocked` and internal errors all exit 1 (`:48`); Ctrl-C does not cancel; `run` with only `speech.*` tools returns `unknown_tool` unless heardright is listed (`lib.rs:63-67`). The journal has no timestamp, request id, subject, effect or argument digest (`toolbox.rs:43-61`), does not record calls to unknown tools, and `JsonlJournal` swallows I/O errors (fails open). Principle 19.

**Conforms well.** Gate order is tested and fail-closed before any side effect; exposure tokenising handles camelCase, acronyms and separator-free names and applies to batch steps; the journal stores no argument values; foreground input refused; secure field values omitted; relative paths rejected; stable `code` constants with a `retryable` flag; JSON on stdout, errors on stderr, no prompts.

### 5.2 `rightkit-agents`

**AG-M1 (medium) Claude control requests other than `can_use_tool` are dropped unanswered.** No event, no reply (`claude.rs:833-856`, `session.rs:294-312`), so a turn can hang with nothing reaching the host. Codex correctly answers unknown server requests with -32601 (`session.rs:350-361`). Plausible; depends on which subtypes the CLI sends.

**AG-M2 (medium) Approvals are not exact or digest-bound.** Claude: the allow response echoes the stored input (good), but approvals are keyed by id only, a duplicate id overwrites the pending entry (`session.rs:299`), and `tool_use_id`, `agent_id` and `blocked_path` are parsed and then dropped (`claude.rs:507-519`), so the host cannot tell a subagent's request from the main agent's (prior #571). Codex: decisions are bound only to the JSON-RPC id; `fileChange` approvals carry no diff and diff notifications are discarded (`codex.rs:58-74`), so the host approves a change set it cannot see; the scope of `AcceptForSession` is not shown. Principle 17.

**AG-M3 (medium) Fitness as an AXA-1 Phase D agent adapter.** It captures the tool-call sequence, Claude cost, model id, permission denials and subagent parentage. It does not capture the final answer (Claude `result.result`, `claude.rs:920-948`), the CLI version (Claude `claude_code_version`, Codex `userAgent` discarded, `session.rs:799-803`), Codex cost or tool output, reasoning tokens, or timestamps; Claude `Usage` is per message and Codex `Usage` cumulative (`codex.rs:186-204`), so summing double-counts. These are the fields a `right-ax` matrix adapter needs (§6.6, §9.6), so wiring the two together needs these additions first.

**Low.** Claude `cancel` drains pending approvals without answering them (`session.rs:653-667`); Codex `serverRequest/resolved` never removes the pending entry, so stale approvals stay answerable. `RateLimitState` keeps one slot and ignores `rate_limit_type` (`rate_limit.rs:36`), so a later five-hour "allowed" erases a seven-day "rejected"; a usage limit is detected by the substring "usage limit" in error text (`codex.rs:279-283`), so free text drives control state (Principle 16). `find_binaries` still accepts empty or relative PATH entries (`discovery.rs:52-66`, prior #704). `allowed_tools`/`disallowed_tools` are not shape-checked (`claude.rs:237-244`), so a host forwarding untrusted names could pass a flag such as `--dangerously-skip-permissions` past the `bypassPermissions` refusal. The event channel is unbounded.

**Conforms well.** Refuses `bypassPermissions`; shape-checks ids; unanswered, timed-out or disconnected approvals are denied; Codex unknown server requests get -32601; process tree torn down through `OwnedChild`; unknown messages degrade to `Unknown`; FailClosed available for Codex.

## 6 Agent-facing Node CLIs against the AXA-1 CLI profile (§12.1)

Delegated review; the reviewer ran CLIs and tests in scratch repositories in this container (no publish, push or network writes) and traced the rest. No `node_modules` exist in this checkout, so most `right-release` subcommands (need `smol-toml`) and `right-update` (needs a `tsc` build) were traced, not run. Test suites run: hooks 6/6, issues-tracker 21/21, ci 216 pass / 5 skipped / 0 fail. I re-read the code for CLI-H1, CLI-M1 and `parseFlags` before including them.

### 6.1 Scorecard

| CLI | Help (root/sub) | Unknown flags rejected | Exit taxonomy | JSON on stdout | Preview / idempotent effects |
|---|---|---|---|---|---|
| `right-release` | partial: subcommand `--help` exits 0, 1 or 2 by subcommand; 125 in a public checkout without a grant | partial (`ownership` uses `args.includes`) | fail: child codes pass through; crash = 1 with stack; 2 = usage and publish refusal | partial, some subcommands, no schema | partial: GitHub publish resumable; preview polarity differs by platform |
| `rightkit-gate` | partial | fail (any `-` token accepted, `:861-877`) | partial: 1 = policy fail and environment precondition and scanner crash | fail (none) | pass (`--plan`) |
| `right-git` | partial | fail (`parseFlags`, `cli/right-git.mjs:52-74`) | fail: drift exits 1 for drift and read errors; `drift --all /nonexistent` exits 0 silently | partial (`artifacts fetch --json` only) | fail (`uninstall` has none) |
| `right-issues` | pass | pass | fail: every error exits 1 | fail | pass (temp file + rename; incomplete inventory refuses to write) |
| `rightkit-hooks` | fail (`--help` treated as a module path) | n/a | partial: 2 = bad payload and import failure | pass (`schemaVersion` always present) | fail (timeout does not stop the handler) |
| `right-update` (traced) | fail (subcommand `--help` rejected) | pass | pass (documented per command) | pass (JSON and JSON lines) | n/a; unknown entries reported clean |

### 6.2 Findings

**CLI-H1 (high) `right-git uninstall <repo> --dry-run` deletes files.** `runUninstall` (`packages/ci/cli/right-git.mjs:196-206`) reads only positionals; `--dry-run` is silently ignored and there is no preview mode. Verified by the reviewer in a scratch repo: `ci.yml` and `.rightgit.json` removed, exit 0. §12.1 "no accepted-but-ignored arguments", Principle 13 (preview before destructive effect).

**CLI-H2 (high) `right-git sync` ignores misspelled flags and `--help`.** `parseFlags` accepts any `--flag` (`:52-74`), so `sync <repo> --dryrun` or `sync <repo> --help` would run a real sync (traced; the write path needs `gh` to confirm the repo is public, so it was not run). `sync --dry-run <repo>` swallows the repo path as the flag's value and fails with an unexplained usage error (verified).

**CLI-M1 (medium) The mandatory pre-push gate passes a deletion-only push without scanning it.** `packages/release/bin/rightkit-gate.mjs:954` diffs with `--diff-filter=ACMR` and then keeps only files that exist in the export, so deleting a module that other code imports yields 0 scans and `pass (0 changed file(s))`, exit 0 (`:1066`). The module header says "nothing is partly covered silently". Verified in a scratch repo. CI still tests everything, which is why this is medium. §13.5 "empty means clean".

**CLI-M2 (medium) `rightkit-gate` accepts unknown flags on state-changing commands.** `install-hook --dry-run` wrote `.githooks/pre-push` and set `core.hooksPath`; the typo `--plna` ran the full gate instead of the plan. Verified.

**CLI-M3 (medium) `right-release --help`, `-h` and `--version` exit 125 in a public Orthic-Labs checkout** without a local grant or with a malformed `.rightgit.json`: the policy check (`cli/right-release.mjs:66-77`) runs before help (`:79`). Verified. An agent cannot read the help it is told to read.

**CLI-M4 (medium) Cancelling `right-release` leaves the work running.** The dispatcher spawns its module without forwarding signals (`:173-185`); after SIGTERM the dispatcher exited 143 and the grandchild kept running. Affects `upload`, `publish-update`, `qualify`. Verified with the `test` subcommand.

**CLI-M5 (medium) `rightkit-hooks` reports a timeout and lets the handler continue.** "timed out" at 3 s, handler still running and process alive until 8 s (`packages/hooks/runtime.mjs:26-48`), so side effects land after the hook was reported failed. Verified.

**CLI-M6 (medium) Exit codes conflate a broken tool with a policy failure.** In `rightkit-gate` a scanner crash ("Cannot find package smol-toml") is reported as `BLOCKED`, exit 1 (verified); a crash path exits 2 with `e.stack` (`:1078`). `right-issues` exits 1 for usage errors, missing `gh` and incomplete inventory alike. §13.4.

**CLI-M7 (medium) `upload` previews or commits depending on platform.** mac/win upload for real unless `--dry-run`; android/ios only preview unless `--apply` (`upload-release.mjs:16-44`). Traced; the tier, sealed-release and clean-tree gates reduce the risk. Principle 5 (poka-yoke).

**CLI-M8 (medium) `right-update` reports unknown entries as clean.** Status `unknown` without an error (e.g. "no upstream version found") exits 0 and `run` reports "all current" (`check.ts:107-109`, `548-552`; `run.ts:225`). Traced. §13.5.

**CLI-M9 (medium) Agent instructions point at retired or refused commands.** `ownership trust-sync [--check]` is a no-op that exits 0 (`ownership-command.mjs:78`), while root help still says it advances trust and exits 3 on drift (`right-release.mjs:320-321`) and `AGENTS.md:164` still directs agents to it. `AGENTS.md:150` says to publish with `right-release publish <npm|cargo> --local`, which is always refused with exit 2 (`right-release.mjs:118-126`; the refusal message is clear). §13.18 documentation as a second implementation.

**Low.** `right-issues` writes and exits 0 when the search response lacks `total_count` and ignores `incomplete_results` (`right-issues.mjs:83-85`, `143`); resolves `--out/--issues/--groups` against `--root`, undocumented (`:129-135`); `rightkit-gate` waits for stdin EOF when stdin is a pipe (`:856`, held 12 s); `rightkit-hooks` has no `--help`/`--version` and reads stdin before parsing arguments. Info: `rightkit-hooks` echoes handler error text unredacted into stdout JSON (a test `password=hunter2` came through) and `right-issues` forwards `gh`'s first stderr line; no real secret was seen printed.

**Not covered:** `right-release upload/build/publish-update/ownership/qualify` runs, `right-update` runs, paths with spaces or Unicode, human-versus-JSON equivalence.

## 7 Does AXA-1 need a refresh?

Yes. Checked 2026-10-23 by a delegated reviewer against primary sources; I confirmed the MCP items myself at modelcontextprotocol.io (versioning page, 2026-07-28 changelog, 2026-07-28 tools page). Items the reviewer could not confirm are marked unverified.

### 7.1 Factual corrections

1. **R6, R7 superseded.** The current MCP revision is 2026-07-28 ([versioning](https://modelcontextprotocol.io/specification/versioning), [changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)), released nine days before AXA-1's date. Changes that affect §12.2, §6.4 Phase C and Principle 12:
   - The `initialize`/`notifications/initialized` handshake is removed. Each request carries its protocol version and client capabilities in `_meta`; servers MUST implement `server/discover`; a version mismatch returns `UnsupportedProtocolVersionError` (-32022). §12.2's "`initialize` and capability negotiation" and Phase C's "Initialize and discover capabilities" should say "`server/discover` and per-request `_meta`, with `initialize` only for peers on 2025-11-25 or earlier".
   - Tasks moved out of core into the official extension `io.modelcontextprotocol/tasks` (SEP-2663): `tasks/get` polling, `tasks/update` for client input, `tasks/result` and `tasks/list` removed. "Task support" now means the extension capability.
   - `inputSchema`/`outputSchema` may use any JSON Schema 2020-12 keyword, with `$ref` resolution requirements; `structuredContent` may be any JSON value.
   - `tools/list` MUST NOT vary per connection, MAY vary by authorization, SHOULD be deterministic in order, and carries `ttlMs`/`cacheScope`. Add an audit case: advertised tools are callable for each credential scope.
   - Roots, Sampling and Logging are deprecated; `ping` and `logging/setLevel` are removed; catalog-change notifications go through `subscriptions/listen`; resource-not-found changes from -32002 to -32602; server-initiated requests become multi-round-trip `input_required` results.
   - Unchanged and worth stating in §5.3: input-validation errors are tool execution errors (`isError: true`), so AXA-1's `usage_error` maps to `isError`, not JSON-RPC -32602; servers MUST validate all tool inputs; clients MUST treat annotations as untrusted unless the server is trusted ([tools](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)).
2. **§16 metadata.** R24 MiniScope v2 (2026-10-06) is retitled "MiniScope: Authorizing Agents with Least-Privilege Permissions" ([arXiv](https://arxiv.org/abs/2512.11147v2)). R26 ToolACE was published at ICLR 2025. R5 OpenAI function calling now recommends fewer than 20 functions at the start of a turn and tool search with deferred loading (launch date unverified). R21: the initiative page was updated 2026-08-14 with no new deliverables listed (what changed is unverified), and CAISI now appears as CAISSI.

### 7.2 New evidence to add

- **Principles 2 and 9 (progressive disclosure, context efficiency):** Anthropic "Introducing advanced tool use" (2025-11-24): tool search with `defer_loading`, programmatic tool calling, tool-use examples; Claude tool-search documentation (selection degrades past 30-50 tools); OpenAI tool search; MCP deterministic, cacheable tool lists.
- **Principle 10:** MCP 2026-07-28 non-normative "Stateful Tools" guidance (opaque, server-minted handles with stated lifetime and expiry errors).
- **Principles 18, 24, §7.3, §13.15 (claims and verification):** OverclaimBench (arXiv 2609.20812), a false-success study finding no LLM-judge configuration above AUROC 0.65 (arXiv 2606.09863), and "Unreliable Progress Bar" (arXiv 2609.08589). These strengthen the case against LLM-judge-only verification and against controlling flow on the model's own state reports, which bears directly on AX-02 and AX-03.
- **Principle 16 and Phase E:** adaptive attacks overstate static AgentDojo defenses (AutoDojo, arXiv 2606.15057; arXiv 2606.26479). Require adaptive, not only static, injection cases.
- **Principle 15:** MiniScope v2, SkillScope (arXiv 2605.05868), MCP 2026-07-28 authorization (`iss` validation, issuer-bound credentials, Dynamic Client Registration deprecated in favour of Client ID Metadata Documents).
- **Principles 21, 22, §7.2:** AXIS v1.20.0 `--runs` (median run, spread, reliability fraction) and v1.19.0 hard-fail on misconfigured scenarios ([CHANGELOG](https://github.com/netlify/axis/blob/main/CHANGELOG.md)); τ-bench 2.0 (2026-10-19), whose scores are not comparable with 1.x, so the standard should require pinning the benchmark version; MCPEvol-Bench (arXiv 2607.14642) on 13.7-14.4% loss when servers evolve; MCP-Atlas (arXiv 2602.00933) on cognitive failures such as premature stopping.
- **Security companions:** OWASP Agent Control Standard v0.1 preview, the 2026 LLM Top 10 (2026-09-02) and the GenAI Red Teaming Manual v1.0 (2026-10-20); the OWASP Agentic Top 10 itself (R22) has no revision. NIST CAISSI/NSA/CISA incident-readiness practices (2026-10-16) cover uninstructed agent actions during evaluation (relevant to AX-05).
- **§12.4 packaging:** the Agent Skills specification is unversioned and `allowed-tools` is still experimental; Agent Plugins 1.0.0 is the current release (1.1.0 is a working draft, editorially different only), so `right-ax`'s vendored 1.0.0 schemas are current.

### 7.3 Process changes for the standard

- Put the standard under version control next to its implementation (for example `rightkit/docs/standards/AXA-1.md`), with a version-of-record line, and have `right-ax` report which AXA-1 version its rules encode (AX-18). Today the design doc points at `D:\Downloads\`.
- Add a review cadence to §15 and re-check references at each MCP revision; this one was missed because the revision landed days before publication.

The reviewer's unverified items: venues for OSWorld-MCP (R19) and Progent (R23); NCCoE agent-identity dates after February; OpenAI tool-search launch date; Agent Plugins 1.0.0 release date; the 2026-08-14 change to the NIST initiative page. The arXiv identifiers above were reported by the reviewer from arXiv; I did not open each one.

## 8 Recommended order of work

1. **Make `right-ax`'s behavioral harness independent (AX-01, AX-02, AX-03, AX-11).** Send the adapter only the goal, user messages, initial state, allowed operations and limits; add a verifier hook that reads final state itself; evaluate claims structurally or with a recorded rule-plus-model judgment, not substrings. Until then, no `right-ax` matrix result should be cited as behavioral evidence.
2. **Stop `right-ax` from passing or harming what it cannot inspect (AX-04, AX-05, AX-06, AX-08).** Unknown or uninspected surfaces yield `unproven`; scrub the child environment and do not call tools with non-read-only effects without a manifest example; follow `nextCursor`; treat unsupported schema keywords as `unverifiable`.
3. **Fix the three agent-facing correctness bugs that produce wrong outcomes today:** `right-ctl` `confirm` must come from host admission, not the agent's arguments (AT-H1); a failed `batch` must be an error (AT-H2); `right-git uninstall`/`sync` must reject unknown flags and honour `--dry-run` (CLI-H1, CLI-H2). Each is a small, local change.
4. **`rightkit-mcp` conformance:** make `expose` agree with `admit` (MCP-H1); validate input against `inputSchema` and return validation and execution failures as `isError` results with a stable code (MCP-M2, MCP-M3); add 2025-11-25 and plan 2026-07-28 support; stop replaying non-idempotent calls (MCP-M1).
5. **Build the §10 Phase 1 shared contract layer in `contracts/`:** operation descriptor, result envelope with separate invocation / outcome / claim boundary, typed error envelope, effect profile. Then derive `right-ctl`'s schemas from it (AT-M3, AT-M5) and point `right-ax` rule 5 at it.
6. **Refresh AXA-1 to 1.1** per section 7, and refresh `right-ax` to the new MCP revision and a CLI profile (AX-04, AX-07).
7. **Dogfood:** add `.rightax.json` for `rightkit-mcp`'s example server and, once the CLI profile exists, for `right-ax`, `right-ctl` and the release CLIs, and correct the stale AGENTS.md commands (CLI-M9).

## Appendix: reproductions

The `packages/ax` reproductions ran under Node 22.22.0 in this container. The scripts are in the session scratchpad (`ax-repro/`, not committed): `repro.mjs` (AX-03, AX-08, AX-09, AX-11, AX-15), `repro-cli.mjs` (AX-04), `repro-page.mjs` with a paginated fixture server (AX-06, AX-07), and `repro-conf.mjs` (AX-05). The AX-01 evidence is in the repository: `packages/ax/fixtures/fake-ax-adapter.mjs` and `packages/ax/ax-behavioral.test.mjs:309`. CLI reproductions by the delegated reviewer are in the scratchpad's `clis/` folder.
