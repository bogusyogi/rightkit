// Phase C (AXA-1): executable conformance. Generalizes Cortex's
// evals/ax/run-conformance.mjs — spawns the target's real MCP server over
// stdio (command from .rightax.json's surfaces.mcp.serve), probes each tool
// with valid args / missing-required / unknown-field, validates
// structuredContent against the advertised outputSchema, and asserts no raw
// stack trace leaks. Invariant I5: probes run against whatever
// surfaces.mcp.serve actually launches — callers that want the packaged
// artifact tested point that command at the built/installed bin, not source.

import { resolve, sep } from "node:path";
import { validateAgainstSchema } from "../lib/json-schema-lite.mjs";
import { describePathHazard, isBareToken } from "../lib/path-safety.mjs";
import { redactText } from "../lib/redact.mjs";
import { classifyResponse, findStackTraces, openMcpSession, tryParseJson } from "./lib/mcp-stdio-client.mjs";
import { splitServeCommand } from "./static.mjs";

const OMIT = Symbol("omit");
const PROBE_KINDS = ["valid-args", "missing-required", "unknown-field"];

function semanticValue(type, schema) {
  if (type === "string") return "conformance-probe";
  if (type === "integer" || type === "number") return Math.min(Math.max(typeof schema.minimum === "number" ? schema.minimum : 1, 1), typeof schema.maximum === "number" ? schema.maximum : 100);
  if (type === "boolean") return true;
  if (type === "object") return {};
  if (type === "array") return [];
  return OMIT;
}

function buildValidArgs(tool) {
  const properties = tool.inputSchema?.properties ?? {};
  const required = new Set(tool.inputSchema?.required ?? []);
  const args = {};
  for (const [name, schema] of Object.entries(properties)) {
    if (!required.has(name)) continue; // conservative: only fill required fields generically
    const type = Array.isArray(schema.type) ? schema.type[0] : schema.type;
    const value = semanticValue(type, schema);
    if (value !== OMIT) args[name] = value;
  }
  return args;
}

function omitRequired(args, tool) {
  const required = new Set(tool.inputSchema?.required ?? []);
  const next = { ...args };
  for (const key of required) delete next[key];
  return next;
}

function evaluateProbe(kind, classification, tool) {
  if (kind === "valid-args") {
    // F3: valid-args is built to satisfy the tool's own advertised required
    // fields. A typed error in response to that call means the tool rejects
    // a call it should accept — that is the conformance failure the probe
    // exists to catch, not a pass. (A tool that genuinely cannot accept a
    // generic probe value needs a documented example, not credit for
    // rejecting every call.)
    if (classification.kind === "success") return { status: "pass", detail: "schema-valid result" };
    if (classification.kind === "stack-trace") return { status: "fail", detail: "raw stack trace in tool result" };
    if (classification.kind === "typed-error" || classification.kind === "jsonrpc-error") return { status: "fail", detail: `server rejected a call built to satisfy its own advertised required-argument schema (typed-code error ${classification.code})`, sample: classification.sample ?? classification.message };
    return { status: "fail", detail: "untyped error", sample: classification.sample };
  }
  if (kind === "missing-required") {
    const required = (tool.inputSchema?.required ?? []).filter((key) => tool.inputSchema?.properties?.[key] !== undefined);
    if (required.length === 0) return { status: "skipped-pending", detail: "tool declares no required input properties" };
    if (classification.kind === "typed-error" || classification.kind === "jsonrpc-error") return { status: "pass", detail: `rejected with typed stable-code error ${classification.code}` };
    if (classification.kind === "success") return { status: "fail", detail: "server accepted call missing required arguments" };
    if (classification.kind === "stack-trace") return { status: "fail", detail: "raw stack trace in tool result" };
    return { status: "fail", detail: "untyped error", sample: classification.sample };
  }
  if (kind === "unknown-field") {
    // F3: a tool that advertises additionalProperties:false (static rule 3)
    // but then accepts a call carrying an unknown field is not enforcing its
    // own declared schema — that combination is a failure. A tool that never
    // declared additionalProperties:false made no promise to violate.
    const declaredStrict = tool.inputSchema?.additionalProperties === false;
    if (classification.kind === "success") {
      if (declaredStrict) return { status: "fail", detail: "inputSchema declares additionalProperties:false but the server accepted a call containing an unknown field" };
      return { status: "pass", detail: "unknown field tolerated: success result (inputSchema does not declare additionalProperties:false)" };
    }
    if (classification.kind === "typed-error" || classification.kind === "jsonrpc-error") return { status: "pass", detail: `rejected with typed stable-code error ${classification.code}` };
    if (classification.kind === "stack-trace") return { status: "fail", detail: "raw stack trace in tool result" };
    return { status: "fail", detail: "untyped error", sample: classification.sample };
  }
  return { status: "fail", detail: `unknown probe kind ${kind}` };
}

// F12 (invariant I5, partial): probes must run against something inside the
// repo the caller pointed us at, not an arbitrary external path — the exact
// shape the QA repro exploited (a manifest command/arg pointing at a scratch
// file outside repoPath). This is containment only: it cannot by itself
// distinguish a packaged tarball/installed bin from the source tree, which
// would require a packaging-identity convention this package does not yet
// define (tracked as an open decision, not decided here).
function findArtifactContainmentViolation({ command, args, cwd }) {
  const candidates = [["command", command], ...args.map((arg, index) => [`args[${index}]`, arg])];
  const normalizedRoot = resolve(cwd);
  for (const [field, value] of candidates) {
    if (typeof value !== "string" || isBareToken(value)) continue; // PATH-resolved executable name, e.g. "node" — nothing to contain
    const resolved = resolve(cwd, value);
    if (resolved === normalizedRoot || resolved.startsWith(normalizedRoot + sep)) continue;
    return { field, value, hazard: describePathHazard(value) ?? "resolves outside the repo" };
  }
  return null;
}

function rowAggregate(row) {
  const items = [
    ...row.probes.map((probe) => ({ kind: `probe ${probe.kind}`, status: probe.status, reason: probe.detail ?? probe.sample ?? null })),
    ...Object.entries(row.checks).map(([name, check]) => ({ kind: `check ${name}`, status: check.status, reason: check.reason ?? (check.errors?.length ? check.errors.join("; ") : null) })),
  ];
  const failed = items.filter((item) => item.status === "fail");
  if (failed.length) return { status: "fail", reasons: failed.map((item) => `${item.kind}: ${item.reason ?? "failed"}`) };
  const skipped = items.filter((item) => item.status === "skipped-pending");
  if (skipped.length) return { status: "skipped-pending", reasons: skipped.map((item) => `${item.kind}: ${item.reason ?? "skipped"}`) };
  return { status: "pass", reasons: [] };
}

export async function runConformance({ repoPath, manifest, timeoutMs = 60000 }) {
  if (!manifest?.surfaces?.mcp?.serve) {
    return { schemaVersion: 1, gate: "conformance", fatal: { code: "no_mcp_surface", message: ".rightax.json declares no surfaces.mcp.serve command" }, rows: [], verdict: "fail" };
  }
  const { command, args, cwd } = splitServeCommand(manifest.surfaces.mcp.serve, repoPath);
  const report = { schemaVersion: 1, gate: "conformance", server: { command, args, cwd }, initialize: null, resources: null, prompts: null, rows: [], rawStackTraces: [], totals: { pass: 0, fail: 0, "skipped-pending": 0 }, fatal: null, verdict: "fail" };

  const containmentViolation = findArtifactContainmentViolation({ command, args, cwd });
  if (containmentViolation) {
    report.fatal = { code: "artifact_outside_repo", message: `invariant I5: surfaces.mcp.serve ${containmentViolation.field} (${JSON.stringify(containmentViolation.value)}) resolves outside the repo (${containmentViolation.hazard}) — probes must run against an artifact inside the package, not an arbitrary external path` };
    return report;
  }

  let session = null;
  try {
    session = await openMcpSession({ command, args, cwd, timeoutMs, listResourcesAndPrompts: true });
    report.initialize = session.initialize;
    report.resources = session.resources;
    report.prompts = session.prompts;

    // F4: a JSON-RPC error on tools/list must never silently become "zero
    // tools, vacuously pass" — and zero tools with no error is equally
    // untestable, so it is fatal too, not a passing empty rowset.
    if (session.toolsListError) {
      report.fatal = { code: "tools_list_failed", message: `tools/list returned an error: ${JSON.stringify(session.toolsListError)}` };
    } else if (session.tools.length === 0) {
      report.fatal = { code: "no_tools", message: "tools/list returned zero tools; there is nothing to probe, so conformance cannot be established" };
    }

    for (const tool of report.fatal ? [] : session.tools) {
      const row = { tool: tool.name, inputSchema: tool.inputSchema ?? null, outputSchema: tool.outputSchema ?? null, probes: [], checks: {} };
      const validArgs = buildValidArgs(tool);
      const probes = [
        { kind: "valid-args", args: validArgs },
        { kind: "missing-required", args: omitRequired(validArgs, tool) },
        { kind: "unknown-field", args: { ...validArgs, __right_ax_unknown_field__: "probe" } },
      ];
      for (const probe of probes) {
        const started = Date.now();
        let response;
        try {
          response = await session.rpc.request("tools/call", { name: tool.name, arguments: probe.args });
        } catch (error) {
          row.probes.push({ kind: probe.kind, status: "fail", resultKind: "client-error", detail: redactText(String(error?.message ?? error)), durationMs: Date.now() - started });
          continue;
        }
        const classification = classifyResponse(response);
        const outcome = evaluateProbe(probe.kind, classification, tool);
        const stack = classification.kind === "stack-trace" ? classification.stack : [];
        if (stack.length) report.rawStackTraces.push({ tool: tool.name, probe: probe.kind, hits: stack });
        row.probes.push({ kind: probe.kind, status: outcome.status, resultKind: classification.kind, code: classification.code ?? null, structuredContent: classification.kind === "success" ? (classification.structuredContent ?? null) : null, detail: outcome.detail, sample: outcome.sample ?? classification.sample ?? null, durationMs: Date.now() - started });
      }

      if (!tool.outputSchema) {
        row.checks.structuredContentValidation = { status: "skipped-pending", reason: "tool advertises no outputSchema" };
      } else {
        const withStructured = row.probes.find((probe) => probe.structuredContent !== null && probe.structuredContent !== undefined);
        if (!withStructured) {
          row.checks.structuredContentValidation = { status: "skipped-pending", reason: "no tool call returned structuredContent" };
        } else {
          const validation = validateAgainstSchema(withStructured.structuredContent, tool.outputSchema);
          row.checks.structuredContentValidation = { status: validation.valid ? "pass" : "fail", errors: validation.errors, reason: validation.valid ? "structuredContent matches advertised outputSchema" : "structuredContent does not match advertised outputSchema" };
        }
      }
      row.checks.rawStackTrace = row.probes.some((probe) => report.rawStackTraces.some((entry) => entry.tool === tool.name)) ? { status: "fail", reason: "raw stack trace in tool result" } : { status: "pass", reason: "no raw stack traces" };
      const aggregate = rowAggregate(row);
      row.status = aggregate.status;
      row.reasons = aggregate.reasons;
      report.rows.push(row);
    }

    const exit = await session.close();
    report.server.exitCode = exit.code;
    report.server.exitSignal = exit.signal;
    const stderrStack = findStackTraces(session.getStderr());
    if (stderrStack.length) report.rawStackTraces.push({ source: "stderr", hits: stderrStack });
  } catch (error) {
    report.fatal = { message: redactText(String(error?.message ?? error)) };
    try { await session?.close(); } catch { /* already tearing down */ }
  }

  for (const row of report.rows) for (const probe of row.probes) report.totals[probe.status] = (report.totals[probe.status] ?? 0) + 1;
  report.verdict = !report.fatal && report.initialize?.status === "pass" && report.rawStackTraces.length === 0 && report.rows.every((row) => row.status !== "fail") ? "pass" : "fail";
  return report;
}

export { buildValidArgs, evaluateProbe, tryParseJson };
