// Runtime budget gate for long-running shared crates (docs/RUNTIME-BUDGETS.md).
//
// A crate in this class must declare a `## Runtime budget` section in its README before
// `right-release publish cargo` uploads it. The gate reads the README text and refuses a
// declaration that lacks the required lines. It does not run the bench: the measured figures
// are the author's claim, and the bench is the author's job.
//
// Class membership: a crate is in the class when it is listed in RUNTIME_BUDGET_CRATES, or when
// its Cargo.toml opts in with:
//
//   [package.metadata.rightkit]
//   runtime_budget = true
//
// `runtime_budget = false` does not remove a listed crate. A non-boolean value is refused.
//
// Check objects use the preflight shape ({ name, status, detail, fix }) so formatPreflight() in
// preflight.mjs prints them as `[FAIL] runtime-budget: ...`.

import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { parse } from "smol-toml";

export const RUNTIME_BUDGET_CHECK = "runtime-budget";
export const RUNTIME_BUDGET_CODE = "RUNTIME_BUDGET_MISSING";

export const RUNTIME_BUDGET_CRATES = Object.freeze([
  "rightkit-fsindex",
  "rightkit-watch",
  "rightkit-jobs",
  "rightkit-service",
  "rightkit-daemon",
  "rightkit-cache",
  "rightkit-sync",
  "rightkit-logs",
  "rightkit-shell",
  "rightkit-qa",
  "rightkit-send",
]);

const MACHINE = /\b(?:machine|host|hardware|mac\w*|linux|windows|arm64|aarch64|x86_64|apple|intel|m[1-9])\b/i;
const DATA_SET = /\b(?:data\s*set|dataset|data|corpus|fixtures?|synthetic)\b|\b\d[\d,_.]*\s*[km]?\s*(?:files?|entries|rows|records|paths|documents|items|events|lines|jobs|watches)\b/i;
const COMMAND = /`[^`\n]+`/;

/** Reads `[package.metadata.rightkit] runtime_budget` from Cargo.toml text. Undefined when unset. */
export function runtimeBudgetOptIn(manifestText) {
  const manifest = parse(manifestText);
  const value = manifest.package?.metadata?.rightkit?.runtime_budget;
  if (value !== undefined && typeof value !== "boolean") {
    throw new Error("[package.metadata.rightkit] runtime_budget must be true or false");
  }
  return value;
}

/** True when the crate is listed, or its manifest opts in with runtime_budget = true. */
export function isRuntimeBudgetCrate(crate, manifestText) {
  if (RUNTIME_BUDGET_CRATES.includes(crate)) return true;
  return manifestText !== undefined && runtimeBudgetOptIn(manifestText) === true;
}

/**
 * Lines of the `## Runtime budget` section (exact level-2 heading), ending at the next `#` or `##`
 * heading. `###` subsections stay inside. Headings inside fenced code blocks are ignored.
 * Returns null when the section is absent.
 */
export function runtimeBudgetSection(readmeText) {
  const lines = String(readmeText).split(/\r?\n/);
  let fenced = false;
  let start = -1;
  let end = lines.length;
  for (let index = 0; index < lines.length; index += 1) {
    const line = lines[index];
    if (/^\s*(```|~~~)/.test(line)) { fenced = !fenced; continue; }
    if (fenced) continue;
    if (start < 0) {
      if (/^##\s+Runtime budget\s*$/.test(line)) start = index;
    } else if (/^#{1,2}\s/.test(line)) {
      end = index;
      break;
    }
  }
  return start < 0 ? null : lines.slice(start + 1, end);
}

/**
 * Validates a README against the runtime budget contract. Returns { ok, problems }; problems name
 * each missing line so the author can fix all of them in one pass.
 */
export function checkRuntimeBudgetReadme(readmeText) {
  const section = runtimeBudgetSection(readmeText);
  if (section === null) return { ok: false, problems: ['no "## Runtime budget" section'] };
  const problems = [];
  if (!section.some((line) => /resident/i.test(line) && /\d/.test(line))) {
    problems.push('"## Runtime budget" has no resident-memory line (resident memory with a figure)');
  }
  if (!section.some((line) => /\bCPU\b/i.test(line) && /\d/.test(line))) {
    problems.push('"## Runtime budget" has no CPU line (steady-state CPU with a figure)');
  }
  const measured = section.find((line) => /^\s*(?:[-*+]\s+)?(?:\*\*)?measured\b/i.test(line));
  if (!measured) {
    problems.push('"## Runtime budget" has no "Measured" line (machine, data set and command of the run)');
  } else {
    const missing = [
      MACHINE.test(measured) ? null : "machine",
      DATA_SET.test(measured) ? null : "data set",
      COMMAND.test(measured) ? null : "command (in backticks)",
    ].filter(Boolean);
    if (missing.length) problems.push(`the "Measured" line does not name the ${missing.join(", ")}`);
  }
  return { ok: problems.length === 0, problems };
}

/**
 * Preflight-shaped check for one crate directory, or null when the crate is outside the class.
 * `readFile` and `exists` are injectable for tests.
 */
export function runtimeBudgetCheck({ crate, crateDir }, { readFile = readFileSync, exists = existsSync } = {}) {
  const manifestPath = path.join(crateDir, "Cargo.toml");
  const manifestText = exists(manifestPath) ? readFile(manifestPath, "utf8") : undefined;
  if (!isRuntimeBudgetCrate(crate, manifestText)) return null;
  const readmePath = path.join(crateDir, "README.md");
  const readme = exists(readmePath) ? readFile(readmePath, "utf8") : null;
  const result = readme === null ? { ok: false, problems: ["README.md is missing"] } : checkRuntimeBudgetReadme(readme);
  if (result.ok) {
    return { name: RUNTIME_BUDGET_CHECK, status: "ok", detail: `${crate} declares a runtime budget in README.md` };
  }
  return {
    name: RUNTIME_BUDGET_CHECK,
    status: "fail",
    detail: `${crate}: ${result.problems.join("; ")}`,
    fix: `add a "## Runtime budget" section to ${readmePath} with the resident-memory, CPU and "Measured" lines (docs/RUNTIME-BUDGETS.md)`,
  };
}
