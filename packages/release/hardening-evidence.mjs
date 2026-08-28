import { createHash } from "node:crypto";
import { readFileSync, realpathSync, writeFileSync } from "node:fs";
import { isAbsolute, relative, resolve } from "node:path";

export const HARDENING_RULES = Object.freeze({
  "system-prompt-marker": Object.freeze({ exact: "system_prompt" }),
});

function hashFile(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function containedPath(root, candidate, label) {
  const canonicalRoot = realpathSync(root);
  const canonical = realpathSync(candidate);
  const rel = relative(canonicalRoot, canonical);
  if (rel === ".." || rel.startsWith("../") || rel.startsWith("..\\") || isAbsolute(rel)) throw new Error(label + " escapes release root");
  return canonical;
}

export function materializeHardeningEvidence({ root, allowances, outputPath }) {
  const normalized = (allowances ?? []).map((allowance) => {
    const rule = HARDENING_RULES[allowance.rule];
    if (!rule) throw new Error("unknown hardening allowance rule: " + allowance.rule);
    if (allowance.exact !== rule.exact) throw new Error(allowance.rule + " allowance must use exact token " + rule.exact);
    if (typeof allowance.file !== "string" || !allowance.file) throw new Error("hardening allowance file is required");
    const sourceMatch = typeof allowance.sourceEvidence === "string" ? /^(.*):([1-9]\d*)$/.exec(allowance.sourceEvidence) : null;
    if (!sourceMatch || isAbsolute(sourceMatch[1])) {
      throw new Error("hardening allowance requires sourceEvidence path:line");
    }
    if (typeof allowance.rationale !== "string" || allowance.rationale.trim().length < 12) {
      throw new Error("hardening allowance requires concrete rationale");
    }
    const file = containedPath(root, isAbsolute(allowance.file) ? allowance.file : resolve(root, allowance.file), "hardening allowance artifact");
    const source = containedPath(root, resolve(root, sourceMatch[1]), "hardening allowance source");
    const sourceLine = Number(sourceMatch[2]);
    const line = readFileSync(source, "utf8").split(/\r?\n/)[sourceLine - 1];
    if (typeof line !== "string" || !line.includes(allowance.exact)) {
      throw new Error(`hardening allowance source line does not contain exact token ${allowance.exact}`);
    }
    return {
      rule: allowance.rule,
      exact: allowance.exact,
      artifactSha256: hashFile(file),
      sourceEvidence: allowance.sourceEvidence,
      sourceSha256: hashFile(source),
      sourceLine,
      rationale: allowance.rationale.trim(),
    };
  });
  const evidence = { schemaVersion: 1, kind: "rightkit-hardening-evidence", allowances: normalized };
  writeFileSync(outputPath, JSON.stringify(evidence, null, 2) + "\n");
  return evidence;
}
