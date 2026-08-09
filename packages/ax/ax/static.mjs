// Phase B (AXA-1): static contract inspection, 14 rules (DESIGN §6). Each
// rule is a pure function of the declared contract — the live tools/list
// payload (fetched once via a minimal MCP session, no probing calls) plus the
// repo's plugin/skill filesystem layout — never of behavior under call
// (that is Phase C, ax/conformance.mjs). Every finding is
// {ruleId, severity, claim, evidence, remediation}; nothing here signs off
// (I1) — it only reports.

import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { validateRepoLayout } from "../plugin/validate.mjs";
import { openMcpSession } from "./lib/mcp-stdio-client.mjs";

function finding(ruleId, severity, status, claim, evidence, remediation) {
  return { ruleId, severity, status, claim, evidence, remediation };
}

// Rule 1 — small distinct tool set; no semantic-duplicate tools (P2).
function rule1(tools) {
  const findings = [];
  const byDescription = new Map();
  for (const tool of tools) {
    const key = String(tool.description ?? "").trim().toLowerCase();
    if (!key) continue;
    if (!byDescription.has(key)) byDescription.set(key, []);
    byDescription.get(key).push(tool.name);
  }
  const duplicates = [...byDescription.values()].filter((names) => names.length > 1);
  if (duplicates.length) {
    findings.push(finding("static-1-tool-set", "error", "fail", "no semantic-duplicate tools", `identical descriptions: ${duplicates.map((names) => names.join("=")).join(", ")}`, "merge or differentiate the duplicate tools' descriptions and behavior"));
  } else {
    findings.push(finding("static-1-tool-set", "info", "pass", "no semantic-duplicate tools", `${tools.length} tools, ${byDescription.size} distinct descriptions`, null));
  }
  return findings;
}

// Rule 2 — wire descriptions carry when-to-use / when-NOT-to-use, in
// tools/list, not only in docs (P4).
// F14 fix: the original patterns only matched one narrow phrasing ("use ...
// when" / "do not use"); a well-formed, semantically explicit description
// using common paraphrases ("Best for X. Avoid for Y.") false-failed. Widened
// to recognize the common ways tool authors phrase when-to-use /
// when-NOT-to-use guidance, while still requiring both directions be present
// somewhere in the description (this remains a lexical heuristic, not a
// semantic check).
const WHEN_TO_USE = /\buse (this|it)?\s*when\b|\bwhen to use\b|\bbest (for|when)\b|\bideal for\b|\bdesigned for\b|\bsuited for\b|\buse (this|it) to\b|\bfor use when\b/i;
const WHEN_NOT_TO_USE = /\bdo not use\b|\bdon'?t use\b|\bnever use\b|\bwhen not to use\b|\bnot for\b|\bavoid (for|using|when)\b|\bnot (suited|recommended) for\b|\bshould not be used\b/i;
function rule2(tools) {
  return tools.map((tool) => {
    const description = String(tool.description ?? "");
    const hasWhen = WHEN_TO_USE.test(description);
    const hasWhenNot = WHEN_NOT_TO_USE.test(description);
    const status = hasWhen && hasWhenNot ? "pass" : "fail";
    return finding("static-2-wire-description", "error", status, "wire description states when-to-use and when-NOT-to-use", `${tool.name}: description=${JSON.stringify(description.slice(0, 160))}`, status === "pass" ? null : `add when-to-use and when-NOT-to-use guidance to the ${tool.name} tools/list description itself, not only to docs`);
  });
}

// Rule 3 — strict input schemas: additionalProperties:false, unknown fields
// rejected (P6).
function rule3(tools) {
  return tools.map((tool) => {
    const strict = tool.inputSchema?.additionalProperties === false;
    return finding("static-3-strict-input-schema", "error", strict ? "pass" : "fail", "inputSchema rejects unknown fields (additionalProperties:false)", `${tool.name}: additionalProperties=${JSON.stringify(tool.inputSchema?.additionalProperties)}`, strict ? null : `set additionalProperties:false on ${tool.name}'s inputSchema (strictObject)`);
  });
}

// Rule 4 — output schemas advertised (P7). Structural validation against real
// responses is Phase C (ax/conformance.mjs); this rule checks presence only.
function rule4(tools) {
  return tools.map((tool) => finding("static-4-output-schema", "warning", tool.outputSchema ? "pass" : "fail", "outputSchema is advertised", `${tool.name}: outputSchema=${tool.outputSchema ? "present" : "absent"}`, tool.outputSchema ? null : `advertise an outputSchema for ${tool.name}`));
}

// Rule 5 — invocation status, domain outcome, and claim boundary are three
// separate, machine-readable fields (P8, P24).
const OUTCOME_FIELDS = ["state", "status", "outcome", "reasonCode"];
function rule5(tools, manifest) {
  if (!manifest?.ax?.claimBoundaryRequired) return [finding("static-5-claim-boundary", "info", "not-applicable", "claim boundary is machine-readable and separate from invocation/outcome", ".rightax.json ax.claimBoundaryRequired=false", null)];
  return tools.map((tool) => {
    const outputProps = tool.outputSchema?.properties ?? {};
    const claimBoundary = outputProps.claimBoundary;
    const hasClaimBoundary = Boolean(claimBoundary);
    const claimBoundaryShapeOk = hasClaimBoundary && ["status", "cleanClaimAllowed", "safeClaims", "prohibitedClaims", "gaps"].every((key) => claimBoundary.properties?.[key] !== undefined || (claimBoundary.required ?? []).includes(key));
    const hasOutcome = OUTCOME_FIELDS.some((key) => outputProps[key] !== undefined);
    // F10 fix: claimBoundaryShapeOk was already computed above but never
    // consulted here — a tool advertising a claimBoundary object with the
    // wrong shape (missing safeClaims/prohibitedClaims/gaps/etc.) still
    // reported "pass" as long as SOME object was present under that key.
    const status = hasClaimBoundary && claimBoundaryShapeOk && hasOutcome ? "pass" : "fail";
    return finding("static-5-claim-boundary", "error", status, "claimBoundary is a separate, machine-readable field from domain outcome", `${tool.name}: claimBoundary=${hasClaimBoundary}(shapeOk=${claimBoundaryShapeOk}) outcomeField=${hasOutcome}`, status === "pass" ? null : `advertise a distinct domain-outcome field (${OUTCOME_FIELDS.join("|")}) and an outputSchema.claimBoundary object with status/cleanClaimAllowed/safeClaims/prohibitedClaims/gaps for ${tool.name}`);
  });
}

// Rule 6 — typed errors: stable code, retryable flag, remediation (P11).
// Static evidence: a redaction/error-envelope helper exists in source that
// carries all three keys together.
const GENERATED_OR_VENDOR_DIRS = new Set(["node_modules", ".git", "target", "dist", "build", "out", "coverage", ".next"]);

function isIgnoredScanDirectory(name) {
  return GENERATED_OR_VENDOR_DIRS.has(name) || name.startsWith(".audit");
}

export function grepRepoForTokens(repoPath, tokenSets, maxFiles = 400) {
  const hits = [];
  const stack = [repoPath];
  let visited = 0;
  while (stack.length && visited < maxFiles) {
    const dir = stack.pop();
    let entries;
    try { entries = readdirSync(dir, { withFileTypes: true }); } catch { continue; }
    for (const entry of entries) {
      if (entry.isDirectory() && isIgnoredScanDirectory(entry.name)) continue;
      const path = join(dir, entry.name);
      if (entry.isDirectory()) { stack.push(path); continue; }
      if (!/\.(mjs|js|ts|mts|cts)$/.test(entry.name)) continue;
      visited += 1;
      let text;
      try { text = readFileSync(path, "utf8"); } catch { continue; }
      for (const tokens of tokenSets) if (tokens.every((token) => text.includes(token))) hits.push({ path: relative(repoPath, path), tokens });
      if (visited >= maxFiles) break;
    }
  }
  return hits;
}
// F10 fix: a lexical grep for three words appearing anywhere in the same
// file (even in unrelated comments, as the QA repro showed with a
// `// code retryable remediation` comment) proves nothing about whether any
// real error path actually returns a typed {code,retryable,remediation}
// envelope. This can never legitimately report "pass" from static text
// alone — presence of the tokens is at best "unverifiable" evidence;
// genuine proof requires observing an actual typed error on the wire
// (ax/conformance.mjs's probes already do this for whatever tools are live).
// Absence of any evidence at all remains a real "fail": nothing suggests the
// property might hold.
function rule6(repoPath) {
  const hits = grepRepoForTokens(repoPath, [["code", "retryable", "remediation"]]);
  const status = hits.length ? "unverifiable" : "fail";
  return [finding("static-6-typed-errors", "error", status, "typed errors carry stable code, retryable flag, and remediation", hits.length ? `lexical evidence only (not proof): ${hits.map((hit) => hit.path).join(", ")}` : "no source file declares code+retryable+remediation together", status === "fail" ? "return a typed error envelope with code/retryable/remediation for every failure path" : "confirm via ax/conformance.mjs's live probes that typed errors are actually returned on the wire — a source-text match does not prove this")];
}

// Rule 7 — bounded output: truncation receipts + continuation cursors (P9).
function rule7(tools) {
  const withBoth = tools.filter((tool) => {
    const props = tool.outputSchema?.properties ?? {};
    return (props.truncated !== undefined) && (props.continuationCursor !== undefined || props.nextCursor !== undefined);
  });
  const status = tools.length === 0 ? "not-applicable" : withBoth.length ? "pass" : "report";
  return [finding("static-7-bounded-output", "warning", status, "bounded output declares truncation receipts and continuation cursors", `${withBoth.length}/${tools.length} tools declare truncated+cursor fields`, status === "pass" ? null : "advertise truncated + continuationCursor/nextCursor on any tool that can return more results than it emits")];
}

// Rule 8 — opaque handles over arbitrary paths; no model-selectable
// filesystem roots (P10, P15).
const SAFE_PATH_LIKE_NAMES = new Set(["anchor", "repoId", "cursor"]);
const PATH_LIKE_NAME = /^(path|filePath|file|repoRoot|root|dir|directory)$/i;
function rule8(tools) {
  const findings = [];
  for (const tool of tools) {
    const properties = tool.inputSchema?.properties ?? {};
    for (const [name, schema] of Object.entries(properties)) {
      if (SAFE_PATH_LIKE_NAMES.has(name)) continue;
      if (!PATH_LIKE_NAME.test(name)) continue;
      const constrained = Boolean(schema.enum || schema.pattern || schema.const);
      findings.push(finding("static-8-opaque-handles", "error", constrained ? "pass" : "fail", "no model-selectable arbitrary filesystem root", `${tool.name}.${name}: type=${schema.type ?? "unknown"} constrained=${constrained}`, constrained ? null : `replace ${tool.name}.${name} with an opaque handle (id/enum/anchor) instead of an unconstrained filesystem path`));
    }
  }
  if (!findings.length) findings.push(finding("static-8-opaque-handles", "info", "pass", "no model-selectable arbitrary filesystem root", "no path-like input properties found", null));
  return findings;
}

// Rule 9 — effect declarations per operation (P14): reads/writes/executes/
// network/destructive/idempotent/approval, via MCP annotations or a
// conventional effects module.
const EFFECT_KEYS = ["reads", "writes", "executesProjectCode", "network", "installsSoftware", "destructive", "idempotent", "approval"];
function findEffectsModule(repoPath) {
  const candidates = ["mcp/effects.mjs", "effects.mjs", "effects.json", "ax/effects.mjs"];
  for (const relPath of candidates) {
    const path = join(repoPath, relPath);
    if (existsSync(path)) return path;
  }
  return null;
}
function rule9(tools, repoPath) {
  const effectsModulePath = findEffectsModule(repoPath);
  let declaredNames = new Set();
  if (effectsModulePath) {
    try {
      const text = readFileSync(effectsModulePath, "utf8");
      for (const tool of tools) if (text.includes(tool.name) && EFFECT_KEYS.some((key) => text.includes(key))) declaredNames.add(tool.name);
    } catch { /* treated as no evidence */ }
  }
  return tools.map((tool) => {
    const viaAnnotations = tool.annotations && ["readOnlyHint", "destructiveHint", "idempotentHint"].some((key) => tool.annotations[key] !== undefined);
    const viaModule = declaredNames.has(tool.name);
    // F10 fix: viaAnnotations is genuine evidence — it comes from the tool's
    // own live tools/list payload. viaModule is a lexical grep (the tool's
    // name plus any one effect-key word appearing anywhere in a candidate
    // file, even a comment, as the QA repro showed) and can never prove the
    // declaration is real or bound to this tool — it reports "unverifiable",
    // not "pass", when it is the only evidence.
    const status = viaAnnotations ? "pass" : viaModule ? "unverifiable" : "fail";
    return finding("static-9-effect-declarations", "error", status, "each operation declares its effect profile", `${tool.name}: annotations=${Boolean(viaAnnotations)} effectsModule=${viaModule}(${effectsModulePath ?? "none found"})`, status === "pass" ? null : status === "unverifiable" ? `${tool.name} has lexical evidence only in ${effectsModulePath} — confirm it is a real, bound effect declaration, or advertise MCP annotations instead` : `advertise MCP annotations (readOnlyHint/destructiveHint/idempotentHint) or declare ${tool.name} in an effects module covering ${EFFECT_KEYS.join("/")}`);
  });
}

// Rule 10 — long-running operations expose task semantics or document the
// omission explicitly (P12).
const LONG_RUNNING_LANGUAGE = /\b(long[- ]running|background job|asynchronous job)\b/i;
function rule10(tools, repoPath) {
  return tools.map((tool) => {
    const description = String(tool.description ?? "");
    if (!LONG_RUNNING_LANGUAGE.test(description)) return finding("static-10-long-running", "info", "not-applicable", "long-running operations expose task semantics or document the omission", `${tool.name}: no long-running language in description`, null);
    const hasTaskField = Boolean(tool.outputSchema?.properties?.taskId || tool.outputSchema?.properties?.jobId);
    const documentedOmission = existsSync(join(repoPath, "SKILL.md")) && /synchronous only|no task semantics/i.test(readFileSync(join(repoPath, "SKILL.md"), "utf8"));
    const status = hasTaskField || documentedOmission ? "pass" : "fail";
    return finding("static-10-long-running", "warning", status, "long-running operations expose task semantics or document the omission", `${tool.name}: taskField=${hasTaskField} documentedOmission=${documentedOmission}`, status === "pass" ? null : `add taskId/jobId polling semantics to ${tool.name}, or document the synchronous-only omission in SKILL.md`);
  });
}

// Rule 11 — no secret material in errors/logs/egress (P19). Static evidence:
// a redaction helper exists somewhere in source.
// F10 fix: the presence of the literal word "redact" anywhere in the repo
// (even in a comment, as the QA repro showed) is not proof that a redaction
// layer exists, is wired into the actual error/egress path, or works — it
// can never report "pass" on that basis alone. Absence stays "report"
// (unchanged): this rule was already a warning-severity heuristic, and "no
// evidence found" is a legitimately weaker claim than "found evidence I
// can't verify", so it keeps its original, more permissive label.
function rule11(repoPath) {
  const hits = grepRepoForTokens(repoPath, [["redact"]]);
  const status = hits.length ? "unverifiable" : "report";
  return [finding("static-11-no-secrets", "warning", status, "no secret material in errors, logs, or egress", hits.length ? `lexical evidence only (not proof): ${hits.map((hit) => hit.path).join(", ")}` : "no redaction helper found in source", status === "unverifiable" ? "confirm the redaction layer is actually wired into every error/egress path, and prove it with a known-secret corpus in ax/adversarial coverage" : "add a redaction layer before any tool result/error leaves the process, and prove it with a known-secret corpus in ax/adversarial coverage")];
}

// Rule 12 — docs/examples validate against live schemas; no capability
// advertised without a working handler (P3, §5.1 invariant).
function extractSkillToolMentions(skillMarkdown) {
  const matches = skillMarkdown.matchAll(/`([a-z][a-z0-9_]*_[a-z0-9_]+)`/g);
  return [...matches].map((match) => match[1]);
}
function rule12(tools, repoPath) {
  const toolNames = new Set(tools.map((tool) => tool.name));
  const skillPaths = [join(repoPath, "SKILL.md"), ...(existsSync(join(repoPath, "skills")) ? readdirSync(join(repoPath, "skills")).map((name) => join(repoPath, "skills", name, "SKILL.md")) : [])];
  const findings = [];
  for (const skillPath of skillPaths) {
    if (!existsSync(skillPath)) continue;
    const text = readFileSync(skillPath, "utf8");
    const mentioned = extractSkillToolMentions(text).filter((name) => name.includes("_"));
    for (const name of mentioned) {
      if (toolNames.has(name)) continue;
      // Only flag mentions that look like this repo's own tool naming
      // convention (shares a prefix with an advertised tool) to avoid
      // false positives on unrelated backticked identifiers.
      const prefix = name.split("_")[0];
      if (![...toolNames].some((toolName) => toolName.startsWith(`${prefix}_`))) continue;
      findings.push(finding("static-12-no-phantom-capabilities", "error", "fail", "no capability advertised without a working handler", `${relative(repoPath, skillPath)} references \`${name}\`, which is not in the live tools/list`, `remove the phantom reference or ship a working ${name} handler`));
    }
  }
  if (!findings.length) findings.push(finding("static-12-no-phantom-capabilities", "info", "pass", "no capability advertised without a working handler", "no phantom tool references found in SKILL.md files", null));
  return findings;
}

// Rule 13 — agent-plugins layout valid (delegates to plugin/validate.mjs).
function rule13(repoPath) {
  const { findings } = validateRepoLayout(repoPath);
  if (!findings.length) return [finding("static-13-plugin-layout", "info", "pass", "agent-plugins layout is valid", repoPath, null)];
  return findings.map((item) => finding(`static-13-${item.ruleId}`, item.severity, item.severity === "error" ? "fail" : item.severity === "info" ? "pass" : "report", item.claim, item.evidence, item.remediation));
}

// Rule 14 — SKILL.md references no nonexistent files (the Cortex SKILL.md
// failure, generalized: every referenced path must exist in the package).
const REFERENCED_PATH = /`((?:\.{0,2}\/)?[\w.-]+(?:\/[\w.-]+)+\.[a-zA-Z0-9]+)`/g;
function extractReferencedPaths(markdown) {
  return [...markdown.matchAll(REFERENCED_PATH)].map((match) => match[1]).filter((path) => !path.includes("://") && !path.startsWith("http"));
}
function rule14(repoPath) {
  const findings = [];
  const skillPaths = [join(repoPath, "SKILL.md"), ...(existsSync(join(repoPath, "skills")) ? readdirSync(join(repoPath, "skills")).map((name) => join(repoPath, "skills", name, "SKILL.md")) : [])];
  for (const skillPath of skillPaths) {
    if (!existsSync(skillPath)) continue;
    const text = readFileSync(skillPath, "utf8");
    const skillDir = dirname(skillPath);
    for (const referenced of extractReferencedPaths(text)) {
      // F14 fix: a skills/<name>/SKILL.md's own relative references (e.g.
      // `./references/guide.md`) are conventionally relative to that
      // SKILL.md's own directory, not the package root — the original code
      // always resolved against repoPath, false-failing every skill-relative
      // reference. Both bases are tried (repo-root-relative references, e.g.
      // `mcp/effects.mjs` from a nested SKILL.md, remain valid) so this only
      // adds a resolution path; it removes no existing pass.
      const resolvedFromSkillDir = join(skillDir, referenced);
      const resolvedFromRepoRoot = join(repoPath, referenced);
      if (existsSync(resolvedFromSkillDir) || existsSync(resolvedFromRepoRoot)) continue;
      findings.push(finding("static-14-skill-references", "error", "fail", "SKILL.md references no nonexistent files", `${relative(repoPath, skillPath)} references \`${referenced}\`, which does not exist (checked relative to both the SKILL.md's own directory and the package root)`, `remove the reference or ship the file at ${referenced}`));
    }
  }
  if (!findings.length) findings.push(finding("static-14-skill-references", "info", "pass", "SKILL.md references no nonexistent files", "no dangling file references found", null));
  return findings;
}

export async function fetchLiveTools({ command, args = [], cwd, timeoutMs }) {
  const session = await openMcpSession({ command, args, cwd, timeoutMs });
  await session.close();
  return session.tools;
}

export async function runStaticChecks({ repoPath, manifest, tools }) {
  const liveTools = tools ?? (manifest?.surfaces?.mcp?.serve
    ? await fetchLiveTools(splitServeCommand(manifest.surfaces.mcp.serve, repoPath))
    : []);
  const rules = [
    ...rule1(liveTools),
    ...rule2(liveTools),
    ...rule3(liveTools),
    ...rule4(liveTools),
    ...rule5(liveTools, manifest),
    ...rule6(repoPath),
    ...rule7(liveTools),
    ...rule8(liveTools),
    ...rule9(liveTools, repoPath),
    ...rule10(liveTools, repoPath),
    ...rule11(repoPath),
    ...rule12(liveTools, repoPath),
    ...rule13(repoPath),
    ...rule14(repoPath),
  ];
  const verdict = rules.every((rule) => rule.status !== "fail") ? "pass" : "fail";
  return { schemaVersion: 1, gate: "static", toolCount: liveTools.length, rules, verdict };
}

export function splitServeCommand(serveCommand, cwd) {
  const [command, ...args] = String(serveCommand).split(/\s+/).filter(Boolean);
  return { command, args, cwd };
}
