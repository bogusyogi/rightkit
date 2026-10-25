// C4-02 (owner protection): the QA admission and ownership policy files decide release admission,
// so .github/CODEOWNERS must be tracked and must own each of them. This file holds CODEOWNERS to
// that: every policy file is listed with an owner, and every concrete path the file names exists
// in the checkout, so a rename cannot silently drop protection.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const CODEOWNERS = ".github/CODEOWNERS";
const POLICY_FILES = [
  "/packages/release/qa-admission-policy.json",
  "/packages/release/release-qa-floor.json",
  "/packages/release/rightkit-ownership.json",
  "/packages/release/devtools-policy.json",
  "/packages/release/release-qa-policy.mjs",
  "/packages/release/release-qa-hardening.mjs",
  "/packages/release/release-qa-audit.mjs",
  "/packages/release/release-qa-receipt.mjs",
  "/packages/release/ownership-policy-bundle.mjs",
  "/.github/CODEOWNERS",
];

/** CODEOWNERS rules in file order: { pattern, owners }. Blank and "#" comment lines are skipped. */
function rules() {
  return fs
    .readFileSync(path.join(ROOT, CODEOWNERS), "utf8")
    .replace(/\r\n/g, "\n")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !line.startsWith("#"))
    .map((line) => {
      const [pattern, ...owners] = line.split(/\s+/);
      return { pattern, owners };
    });
}

test("C4-02: .github/CODEOWNERS is tracked", () => {
  const result = spawnSync("git", ["-C", ROOT, "ls-files", "--error-unmatch", CODEOWNERS], {
    encoding: "utf8",
    windowsHide: true,
  });
  assert.equal(
    result.status,
    0,
    `${CODEOWNERS} is not tracked by git: ${result.stderr || result.error?.message}`,
  );
});

test("C4-02: CODEOWNERS owns the QA admission and ownership policy files", () => {
  // The last rule for a path wins, as on GitHub, so a later line for a path replaces an earlier one.
  const owned = new Map(rules().map((rule) => [rule.pattern, rule.owners]));
  for (const file of POLICY_FILES) {
    assert.ok(owned.get(file)?.length > 0, `${file} has no owner in ${CODEOWNERS}`);
  }
});

test("C4-02: every concrete CODEOWNERS path exists in the checkout", () => {
  // A pattern with a glob character or a trailing slash names no single file; every other one must.
  const concrete = rules().filter(({ pattern }) => !/[*?[]/.test(pattern) && !pattern.endsWith("/"));
  assert.ok(concrete.length >= POLICY_FILES.length, `expected at least ${POLICY_FILES.length} paths`);
  for (const { pattern } of concrete) {
    const stat = fs.statSync(path.join(ROOT, pattern.replace(/^\//, "")), { throwIfNoEntry: false });
    assert.ok(stat?.isFile(), `${pattern} in ${CODEOWNERS} names no file; update it with the rename`);
  }
});
