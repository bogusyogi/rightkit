import assert from "node:assert/strict";
import test from "node:test";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import {
  checkRuntimeBudgetReadme,
  isRuntimeBudgetCrate,
  RUNTIME_BUDGET_CRATES,
  runtimeBudgetCheck,
  runtimeBudgetOptIn,
  runtimeBudgetSection,
} from "./runtime-budgets.mjs";

const COMPLIANT_README = `# rightkit-fsindex

Intro text.

## Runtime budget

- Resident memory: 6 MB per 1,000 indexed entries; ceiling 512 MB for the whole index.
- CPU: under 2% steady state with no change; 15% during a full rescan.
- Disk operations: 3 per second under change.
- Measured: Mac mini M2, 16 GB; 400,000 synthetic files; \`cargo bench -p rightkit-fsindex --bench budget\`.

## Usage

Other text.
`;

test("a compliant README passes the runtime budget contract", () => {
  assert.deepEqual(checkRuntimeBudgetReadme(COMPLIANT_README), { ok: true, problems: [] });
});

test("a README without a Runtime budget section fails with the section named", () => {
  const result = checkRuntimeBudgetReadme("# rightkit-watch\n\n## Usage\n\nNo budget here.\n");
  assert.equal(result.ok, false);
  assert.deepEqual(result.problems, ['no "## Runtime budget" section']);
});

test("a section missing its CPU line names only the CPU line", () => {
  const readme = COMPLIANT_README.replace(/^- CPU:.*\n/m, "");
  const result = checkRuntimeBudgetReadme(readme);
  assert.equal(result.ok, false);
  assert.equal(result.problems.length, 1);
  assert.match(result.problems[0], /has no CPU line/);
});

test("a Measured line that omits the command and data set names exactly what is missing", () => {
  const readme = COMPLIANT_README.replace(/^- Measured:.*$/m, "- Measured: Mac mini M2, 16 GB, one run.");
  const result = checkRuntimeBudgetReadme(readme);
  assert.equal(result.ok, false);
  assert.equal(result.problems.length, 1);
  assert.match(result.problems[0], /does not name the data set, command \(in backticks\)/);
  assert.doesNotMatch(result.problems[0], /machine/);
});

test("a missing resident-memory line and a missing Measured line are both reported", () => {
  const readme = COMPLIANT_README.replace(/^- Resident memory:.*\n/m, "").replace(/^- Measured:.*\n/m, "");
  const result = checkRuntimeBudgetReadme(readme);
  assert.equal(result.problems.length, 2);
  assert.match(result.problems[0], /no resident-memory line/);
  assert.match(result.problems[1], /no "Measured" line/);
});

test("lines outside the Runtime budget section do not satisfy it", () => {
  const readme = [
    "# rightkit-jobs",
    "## Runtime budget",
    "- Resident memory: 1 MB per job.",
    "## Performance notes",
    "- CPU: 3% idle.",
    "- Measured: a host, 10k rows, `cargo bench`.",
  ].join("\n");
  const section = runtimeBudgetSection(readme);
  assert.deepEqual(section, ["- Resident memory: 1 MB per job."]);
  const result = checkRuntimeBudgetReadme(readme);
  assert.match(result.problems.join("\n"), /no CPU line/);
  assert.match(result.problems.join("\n"), /no "Measured" line/);
});

test("a heading inside a fenced code block does not end the section", () => {
  const readme = COMPLIANT_README.replace("- Disk operations", "```bash\n# not a heading\n```\n- Disk operations");
  assert.equal(checkRuntimeBudgetReadme(readme).ok, true);
});

test("runtime budget class: listed crates are in the class and an unlisted crate is not", () => {
  assert.equal(RUNTIME_BUDGET_CRATES.length, 11);
  assert.equal(isRuntimeBudgetCrate("rightkit-send"), true);
  assert.equal(isRuntimeBudgetCrate("rightkit-license"), false);
});

test("a crate opts in through [package.metadata.rightkit] runtime_budget = true", () => {
  const optedIn = '[package]\nname = "rightkit-new"\n[package.metadata.rightkit]\nruntime_budget = true\n';
  assert.equal(runtimeBudgetOptIn(optedIn), true);
  assert.equal(isRuntimeBudgetCrate("rightkit-new", optedIn), true);
  const optedOut = '[package]\nname = "rightkit-new"\n[package.metadata.rightkit]\nruntime_budget = false\n';
  assert.equal(isRuntimeBudgetCrate("rightkit-new", optedOut), false);
  assert.equal(isRuntimeBudgetCrate("rightkit-new", '[package]\nname = "rightkit-new"\n'), false);
  assert.throws(() => runtimeBudgetOptIn('[package.metadata.rightkit]\nruntime_budget = "yes"\n'), /must be true or false/);
});

function crateFixture(t, { manifest, readme }) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-runtime-budget-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  mkdirSync(root, { recursive: true });
  if (manifest !== undefined) writeFileSync(path.join(root, "Cargo.toml"), manifest);
  if (readme !== undefined) writeFileSync(path.join(root, "README.md"), readme);
  return root;
}

test("a listed crate with a compliant README produces an ok runtime-budget check", (t) => {
  const crateDir = crateFixture(t, { manifest: '[package]\nname = "rightkit-send"\n', readme: COMPLIANT_README });
  const check = runtimeBudgetCheck({ crate: "rightkit-send", crateDir });
  assert.equal(check.name, "runtime-budget");
  assert.equal(check.status, "ok");
});

test("a listed crate whose README lacks the section produces a fail check that names the README", (t) => {
  const crateDir = crateFixture(t, { manifest: '[package]\nname = "rightkit-send"\n', readme: "# rightkit-send\n" });
  const check = runtimeBudgetCheck({ crate: "rightkit-send", crateDir });
  assert.equal(check.status, "fail");
  assert.match(check.detail, /^rightkit-send: no "## Runtime budget" section$/);
  assert.match(check.fix, /README\.md/);
});

test("a crate with no README at all fails the runtime-budget check", (t) => {
  const crateDir = crateFixture(t, { manifest: '[package]\nname = "rightkit-watch"\n' });
  const check = runtimeBudgetCheck({ crate: "rightkit-watch", crateDir });
  assert.equal(check.status, "fail");
  assert.match(check.detail, /README\.md is missing/);
});

test("a non-class crate gets no runtime-budget check, even without a Runtime budget section", (t) => {
  const crateDir = crateFixture(t, { manifest: '[package]\nname = "rightkit-license"\n', readme: "# rightkit-license\n" });
  assert.equal(runtimeBudgetCheck({ crate: "rightkit-license", crateDir }), null);
});
