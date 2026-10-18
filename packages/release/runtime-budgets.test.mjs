import assert from "node:assert/strict";
import crypto from "node:crypto";
import test from "node:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "./background-process.mjs";
import {
  BUDGET_FIGURES,
  CONTINUOUS_PATTERNS,
  evaluateRuntimeBudget,
  isRuntimeBudgetCrate,
  measuredPathspecs,
  measurementStaleness,
  parseBudgetResult,
  recordMeasurement,
  renderRuntimeBudgetBody,
  RUNTIME_BUDGET_CRATES,
  runtimeBudgetAdoptionCheck,
  runtimeBudgetCheck,
  runtimeBudgetOptIn,
  runtimeBudgetSection,
  scanContinuousFiles,
  scanContinuousPatterns,
  appRuntimeBudgetAdoptionCheck,
  heldResourceClasses,
  renderClasses,
  statusApiDeclared,
  syncRuntimeBudgetReadme,
} from "./runtime-budgets.mjs";
import { fileURLToPath } from "node:url";

const COMMIT = "a".repeat(40);
const CEILINGS = { residentMemoryBytes: 536870912, idleCpuPercent: 2, wakeupsPerSecond: 10, diskOpsPerSecond: 5 };
const OBSERVED = { residentMemoryBytes: 100000000, idleCpuPercent: 0.5, wakeupsPerSecond: 3, diskOpsPerSecond: 1 };
const SCALE = { unit: "entries", minCount: 1000000, source: "benches/budget.rs:12" };
const resultLine = (observed, { crate = "rightkit-fsindex", version = "0.2.0", unit = "entries", count = 1000000 } = {}) =>
  `RESULT budget crate=${crate} version=${version} resident_bytes=${observed.residentMemoryBytes} idle_cpu_percent=${observed.idleCpuPercent} wakeups_per_s=${observed.wakeupsPerSecond} disk_ops_per_s=${observed.diskOpsPerSecond} scale_unit=${unit} scale_count=${count}`;
const measurement = (osName, over = {}) => ({
  host: { os: osName, arch: osName === "darwin" ? "arm64" : "x64", model: "test host" },
  crateVersion: "0.2.0", commit: COMMIT, date: "2026-10-11", scope: "synthetic",
  dataScale: { description: "generated tree", unit: "entries", count: 1000000 },
  command: "rightkit cargo bench -p rightkit-fsindex --bench budget",
  observed: { ...OBSERVED }, ...over,
});
const budgetFor = (over = {}) => ({
  schema: 1, crate: "rightkit-fsindex", ceilings: { ...CEILINGS }, requiredScale: { ...SCALE },
  status: { api: "Index::status()", file: "src/index.rs", exposes: ["residentMemoryBytes", "idleCpuPercent"] },
  measurements: [measurement("darwin"), measurement("win32")], ...over,
});
const MANIFEST = '[package]\nname = "rightkit-fsindex"\nversion = "0.2.0"\n';

// Real files in a temp crate directory. Each measurement gets raw bench output as evidence unless told
// otherwise; git is a fake that answers "fresh" unless told otherwise.
function crate(t, { budget = budgetFor(), readme = "render", file = true, manifest = MANIFEST, evidence = "write", src = "pub fn status(&self) -> Status {}\n", extra = {} } = {}) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "rightkit-runtime-budget-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  mkdirSync(path.join(dir, "src"), { recursive: true });
  writeFileSync(path.join(dir, "src", "index.rs"), src);
  if (manifest !== null) writeFileSync(path.join(dir, "Cargo.toml"), manifest);
  if (typeof budget === "object" && evidence === "write") {
    budget = structuredClone(budget);
    budget.measurements = (budget.measurements ?? []).map((m, i) => {
      if (!m || typeof m !== "object" || !m.observed || m.evidence) return m;
      const rel = `runtime-budget.evidence/${i}-${m.host?.os}.txt`;
      const raw = `noise\n${resultLine(m.observed, { crate: budget.crate, version: m.crateVersion, unit: m.dataScale?.unit, count: m.dataScale?.count })}\n`;
      mkdirSync(path.join(dir, "runtime-budget.evidence"), { recursive: true });
      writeFileSync(path.join(dir, rel), raw);
      return { ...m, evidence: { file: rel, sha256: crypto.createHash("sha256").update(raw).digest("hex") } };
    });
  }
  if (file) writeFileSync(path.join(dir, "runtime-budget.json"), typeof budget === "string" ? budget : JSON.stringify(budget));
  if (readme === "render") writeFileSync(path.join(dir, "README.md"), `# x\n\n## Runtime budget\n\n${renderRuntimeBudgetBody(budget).join("\n")}\n\n## Usage\n\ntext\n`);
  else if (readme !== null) writeFileSync(path.join(dir, "README.md"), readme);
  for (const [name, text] of Object.entries(extra)) { mkdirSync(path.dirname(path.join(dir, name)), { recursive: true }); writeFileSync(path.join(dir, name), text); }
  return dir;
}
const freshGit = (over = {}) => (args) => {
  const ok = (stdout = "") => ({ status: 0, stdout });
  if (args[0] === "cat-file") return over.cat ?? ok("commit");
  if (args[0] === "merge-base") return over.ancestor ?? ok();
  if (args[0] === "diff") return over.diff ?? ok("");
  if (args[0] === "ls-files") return over.untracked ?? ok("");
  return { status: 1, stdout: "" };
};
const evaluate = (dir, over = {}, git = freshGit()) =>
  evaluateRuntimeBudget({ crate: "rightkit-fsindex", crateDir: dir, version: "0.2.0", ...over }, { git });
const refused = (result, pattern) => {
  assert.equal(result.ok, false, "expected a refusal");
  assert.match(result.problems.join("\n"), pattern);
};

// ---------------------------------------------------------------- evaluation (fake git)

test("a measured, fresh, in-ceiling, at-scale budget with evidence on both required hosts passes", (t) => {
  assert.deepEqual(evaluate(crate(t)), { ok: true, problems: [] });
});

test("refuses when runtime-budget.json is missing, unreadable, the wrong schema or another crate's", (t) => {
  refused(evaluate(crate(t, { file: false })), /runtime-budget\.json is missing/);
  refused(evaluate(crate(t, { budget: "{nope", readme: null })), /not valid JSON/);
  refused(evaluate(crate(t, { budget: budgetFor({ schema: 2 }) })), /schema must be 1/);
  refused(evaluate(crate(t, { budget: budgetFor({ crate: "rightkit-file-watch" }) })), /is for rightkit-file-watch, expected rightkit-fsindex/);
});

test("refuses an undeclared or non-positive ceiling, naming it", (t) => {
  for (const key of BUDGET_FIGURES) {
    refused(evaluate(crate(t, { budget: budgetFor({ ceilings: { ...CEILINGS, [key]: null } }) })), new RegExp(`ceiling ${key} is undeclared`));
  }
  refused(evaluate(crate(t, { budget: budgetFor({ ceilings: { ...CEILINGS, idleCpuPercent: 0 } }) })), /ceiling idleCpuPercent must be a positive number/);
});

test("refuses when no measurement is recorded: a ceiling alone is not evidence", (t) => {
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [] }) })), /no measurements recorded/);
});

test("refuses measurements taken at a different crate version than the one being published", (t) => {
  const old = budgetFor({ measurements: [measurement("darwin", { crateVersion: "0.1.9" }), measurement("win32", { crateVersion: "0.1.9" })] });
  refused(evaluate(crate(t, { budget: old })), /no measurement for version 0\.2\.0 \(recorded: 0\.1\.9\)/);
  refused(evaluate(crate(t), { version: undefined }), /publishing version is unknown/);
});

test("refuses an observed value above its ceiling, one test per figure", (t) => {
  const labels = { residentMemoryBytes: "resident memory", idleCpuPercent: "idle CPU", wakeupsPerSecond: "wake-ups", diskOpsPerSecond: "disk operations" };
  for (const key of BUDGET_FIGURES) {
    const over = measurement("darwin", { observed: { ...OBSERVED, [key]: CEILINGS[key] + 1 } });
    const result = evaluate(crate(t, { budget: budgetFor({ measurements: [over, measurement("win32")] }) }));
    refused(result, new RegExp(`exceeds the ${labels[key]} ceiling`));
    refused(result, /no macOS measurement within all ceilings/);
  }
});

test("a failing run is not hidden by a second passing run on the same host", (t) => {
  const failing = measurement("darwin", { observed: { ...OBSERVED, wakeupsPerSecond: 99 } });
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [failing, measurement("darwin"), measurement("win32")] }) })), /exceeds the wake-ups ceiling/);
});

test("observed resident memory of exactly 0 is refused as implausible", (t) => {
  const zero = measurement("darwin", { observed: { ...OBSERVED, residentMemoryBytes: 0 } });
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [zero, measurement("win32")] }) })), /residentMemoryBytes of 0 is implausible/);
});

test("refuses when a required host is missing, saying which and why", (t) => {
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("darwin")] }) })), /no Windows measurement for version 0\.2\.0 \(required hosts: macOS and Windows; apps ship on both/);
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("win32")] }) })), /no macOS measurement/);
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("darwin"), measurement("linux")] }) })), /no Windows measurement/);
});

test("darwin-only comes from the release evidence and the budget file may not disagree", (t) => {
  const policy = { hostPolicy: "darwin-only", hostPolicyReason: "no Windows backend (README Known gaps)" };
  const mac = budgetFor({ measurements: [measurement("darwin")], ...policy });
  assert.deepEqual(evaluate(crate(t, { budget: mac })), { ok: true, problems: [] });
  assert.deepEqual(evaluate(crate(t, { budget: mac }), { evidence: policy }), { ok: true, problems: [] });
  // Evidence is the authority: a budget claiming darwin-only against two-host evidence is refused, and so is the reverse.
  refused(evaluate(crate(t, { budget: mac }), { evidence: {} }), /hostPolicy disagrees with the release evidence \(evidence: macOS and Windows; runtime-budget\.json: darwin-only\)/);
  refused(evaluate(crate(t), { evidence: policy }), /hostPolicy disagrees with the release evidence \(evidence: darwin-only; runtime-budget\.json: macOS and Windows\)/);
  // A policy without a reason is not a policy anywhere.
  const noReason = budgetFor({ measurements: [measurement("darwin")], hostPolicy: "darwin-only" });
  const result = evaluate(crate(t, { budget: noReason }));
  refused(result, /darwin-only needs a hostPolicyReason/);
  refused(result, /no Windows measurement/);
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("darwin")], hostPolicy: "mac-first", hostPolicyReason: "x" }) })), /hostPolicy must be "darwin-only"/);
});

test("win32-only comes from the release evidence the same way: the Windows measurement alone, and the files may not disagree", (t) => {
  const policy = { hostPolicy: "win32-only", hostPolicyReason: "published from the Windows host under the local-build grant" };
  const win = budgetFor({ measurements: [measurement("win32")], ...policy });
  assert.deepEqual(evaluate(crate(t, { budget: win }), { evidence: policy }), { ok: true, problems: [] });
  assert.deepEqual(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("win32")] }) }), { evidence: policy }).ok, false);
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("win32")] }) }), { evidence: policy }), /hostPolicy disagrees with the release evidence \(evidence: win32-only; runtime-budget\.json: macOS and Windows\)/);
  refused(evaluate(crate(t, { budget: win }), { evidence: {} }), /hostPolicy disagrees with the release evidence \(evidence: macOS and Windows; runtime-budget\.json: win32-only\)/);
  refused(evaluate(crate(t, { budget: win }), { evidence: { hostPolicy: "darwin-only", hostPolicyReason: "r" } }), /hostPolicy disagrees with the release evidence \(evidence: darwin-only; runtime-budget\.json: win32-only\)/);
  // The required measurement is the Windows one; a macOS measurement alone does not satisfy a win32-only policy.
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("darwin")], ...policy }) }), { evidence: policy }), /no Windows measurement for version 0\.2\.0 \(required hosts: Windows; win32-only policy: published from the Windows host/);
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("win32")], hostPolicy: "win32-only" }) })), /hostPolicy win32-only needs a hostPolicyReason/);
});

test("refuses a missing, wrong-crate or below-scale measurement: the doc's scale is enforced per crate", (t) => {
  refused(evaluate(crate(t, { budget: budgetFor({ requiredScale: null }) })), /requiredScale is undeclared/);
  refused(evaluate(crate(t, { budget: budgetFor({ requiredScale: { unit: "entries", minCount: 0 } }) })), /requiredScale must name unit and a positive minCount/);
  const small = (osName) => measurement(osName, { dataScale: { description: "tiny", unit: "entries", count: 1000 } });
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [small("darwin"), small("win32")] }) })), /no macOS measurement at the required scale \(1000000 entries or more; recorded: 1000 entries\)/);
  const wrongUnit = (osName) => measurement(osName, { dataScale: { description: "files", unit: "files", count: 9000000 } });
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [wrongUnit("darwin"), wrongUnit("win32")] }) })), /no macOS measurement at the required scale/);
});

test("refuses a measurement older than the crate or its dependencies", (t) => {
  const dir = crate(t);
  refused(evaluate(dir, {}, freshGit({ diff: { status: 0, stdout: "src/index.rs" } })), /macOS measurement is stale: crate or workspace dependency changed since commit aaaaaaaaaaaa: src\/index\.rs/);
  refused(evaluate(dir, {}, freshGit({ untracked: { status: 0, stdout: "src/new.rs" } })), /untracked files since the measurement: src\/new\.rs/);
});

test("refuses a measurement whose commit is unknown, not an ancestor, or not checkable", (t) => {
  const dir = crate(t);
  refused(evaluate(dir, {}, freshGit({ cat: { status: 128, stdout: "" } })), /is not a commit in this repository/);
  refused(evaluate(dir, {}, freshGit({ ancestor: { status: 1, stdout: "" } })), /is not an ancestor of HEAD/);
  refused(evaluate(dir, {}, freshGit({ diff: { status: 128, stdout: "" } })), /cannot diff the crate/);
  refused(evaluate(dir, {}, freshGit({ untracked: { status: 128, stdout: "" } })), /cannot list untracked/);
  assert.match(measurementStaleness("short", dir, freshGit()), /full 40-character SHA/);
  assert.equal(measurementStaleness(COMMIT, dir, freshGit()), null);
});

test("refuses malformed measurement records", (t) => {
  const bad = measurement("darwin", { commit: "abc", date: "yesterday", scope: "guess", command: "", dataScale: { description: "x", unit: "u", count: 0 }, observed: { residentMemoryBytes: "big" } });
  const result = evaluate(crate(t, { budget: budgetFor({ measurements: [bad, measurement("win32")] }), evidence: "skip" }));
  refused(result, /commit must be a full 40-character SHA/);
  refused(result, /date must be an ISO date/);
  refused(result, /scope must be one of/);
  refused(result, /has no command/);
  refused(result, /dataScale must name description, unit and a positive count/);
  refused(result, /observed\.residentMemoryBytes must be a non-negative number/);
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [measurement("darwin", { scope: "installed" }), measurement("win32")] }) })), /installed scope must name app\.name and app\.version/);
});

test("refuses a measurement without bench evidence, with a changed file, a missing file, or values that differ", (t) => {
  const bare = (osName) => measurement(osName);
  refused(evaluate(crate(t, { budget: budgetFor({ measurements: [bare("darwin"), bare("win32")] }), evidence: "skip" })), /measurement\[0\] has no evidence/);
  const dir = crate(t);
  const written = JSON.parse(readFileSync(path.join(dir, "runtime-budget.json"), "utf8"));
  const evidenceFile = path.join(dir, written.measurements[0].evidence.file);
  writeFileSync(evidenceFile, `${resultLine({ ...OBSERVED, residentMemoryBytes: 1 })}\n`);
  refused(evaluate(dir), /does not match its recorded sha256/);
  // Rehash after tampering: the recorded observed value no longer matches the raw output.
  const raw = readFileSync(evidenceFile);
  written.measurements[0].evidence.sha256 = crypto.createHash("sha256").update(raw).digest("hex");
  writeFileSync(path.join(dir, "runtime-budget.json"), JSON.stringify(written));
  writeFileSync(path.join(dir, "README.md"), `## Runtime budget\n\n${renderRuntimeBudgetBody(written).join("\n")}\n`);
  refused(evaluate(dir), /evidence residentMemoryBytes 1 != recorded 100000000/);
  rmSync(evidenceFile);
  refused(evaluate(dir), /evidence file .* is missing/);
  written.measurements[0].evidence.file = "../../etc/passwd";
  writeFileSync(path.join(dir, "runtime-budget.json"), JSON.stringify(written));
  refused(evaluate(dir), /evidence file must be under runtime-budget\.evidence\//);
});

test("evidence for another crate or version is refused", (t) => {
  const dir = crate(t);
  const written = JSON.parse(readFileSync(path.join(dir, "runtime-budget.json"), "utf8"));
  const file = path.join(dir, written.measurements[1].evidence.file);
  const raw = `${resultLine(OBSERVED, { crate: "rightkit-file-watch", version: "0.1.0" })}\n`;
  writeFileSync(file, raw);
  written.measurements[1].evidence.sha256 = crypto.createHash("sha256").update(raw).digest("hex");
  writeFileSync(path.join(dir, "runtime-budget.json"), JSON.stringify(written));
  writeFileSync(path.join(dir, "README.md"), `## Runtime budget\n\n${renderRuntimeBudgetBody(written).join("\n")}\n`);
  const result = evaluate(dir);
  refused(result, /evidence is for rightkit-file-watch, not rightkit-fsindex/);
  refused(result, /evidence version 0\.1\.0 != recorded 0\.2\.0/);
});

test("the status pointer must be a pub item, not a comment or a private fn", (t) => {
  const status = (over = {}) => budgetFor({ status: { api: "Index::status()", file: "src/index.rs", exposes: ["residentMemoryBytes"], ...over } });
  refused(evaluate(crate(t, { budget: status(), src: "// pub fn status(&self) -> Status\n/* pub fn status() */\n" })), /no pub item matching Index::status\(\)/);
  refused(evaluate(crate(t, { budget: status(), src: "fn status(&self) {}\n" })), /no pub item matching/);
  assert.deepEqual(evaluate(crate(t, { budget: status(), src: "impl Index {\n    pub(crate) fn status(&self) -> S {}\n}\n" })), { ok: true, problems: [] });
  assert.deepEqual(evaluate(crate(t, { budget: status({ api: "Status" }), src: "pub struct Status { pub entries: usize }\n" })), { ok: true, problems: [] });
  refused(evaluate(crate(t, { budget: status({ api: "Status" }), src: "// pub struct Status\n" })), /no pub item matching Status/);
  refused(evaluate(crate(t, { budget: budgetFor({ status: undefined }) })), /status exposure is not recorded/);
  refused(evaluate(crate(t, { budget: status({ file: "src/gone.rs" }) })), /status\.file src\/gone\.rs does not exist/);
  refused(evaluate(crate(t, { budget: status({ file: "../outside.rs" }) })), /must stay inside the crate/);
  refused(evaluate(crate(t, { budget: status({ exposes: [] }) })), /does not expose residentMemoryBytes/);
  refused(evaluate(crate(t, { budget: status({ exposes: ["residentMemoryBytes", "bogus"] }) })), /unknown figure bogus/);
  assert.equal(statusApiDeclared("Type::run()", "pub async fn run() {}"), true);
});

test("the README is a generated view: a missing file, missing section or disagreeing figure is refused", (t) => {
  refused(evaluate(crate(t, { readme: null })), /README\.md is missing/);
  refused(evaluate(crate(t, { readme: "# x\n" })), /no "## Runtime budget" section/);
  const stale = `# x\n\n## Runtime budget\n\n${renderRuntimeBudgetBody(budgetFor()).join("\n").replace("2%", "50%")}\n`;
  refused(evaluate(crate(t, { readme: stale })), /differs from runtime-budget\.json/);
});

test("sync regenerates the section in place, keeps the rest of the README and makes the gate agree", (t) => {
  const dir = crate(t, { readme: null });
  const budget = JSON.parse(readFileSync(path.join(dir, "runtime-budget.json"), "utf8"));
  const before = "# x\n\nintro\n\n## Runtime budget\n\n- Resident memory: made up\n\n## Usage\n\nkeep me\n";
  const synced = syncRuntimeBudgetReadme(before, budget);
  assert.match(synced, /intro/);
  assert.match(synced, /## Usage\n\nkeep me/);
  assert.doesNotMatch(synced, /made up/);
  writeFileSync(path.join(dir, "README.md"), synced);
  assert.deepEqual(evaluate(dir), { ok: true, problems: [] });
  const appended = syncRuntimeBudgetReadme("# x\n\ntext\n", budget);
  assert.equal(runtimeBudgetSection(appended) !== null, true);
  assert.equal(syncRuntimeBudgetReadme(appended, budget), appended);
});

test("an unmeasured crate (everything undeclared, no measurements) renders honestly and is refused", (t) => {
  const unmeasured = budgetFor({ ceilings: { residentMemoryBytes: null, idleCpuPercent: null, wakeupsPerSecond: null, diskOpsPerSecond: null }, requiredScale: null, measurements: [] });
  assert.match(renderRuntimeBudgetBody(unmeasured).join("\n"), /Resident memory ceiling: undeclared\./);
  assert.match(renderRuntimeBudgetBody(unmeasured).join("\n"), /Required scale: undeclared\./);
  assert.match(renderRuntimeBudgetBody(unmeasured).join("\n"), /Measured: not yet measured\./);
  const result = evaluate(crate(t, { budget: unmeasured }));
  refused(result, /ceiling residentMemoryBytes is undeclared/);
  refused(result, /no measurements recorded/);
});

test("adoption needs a fresh in-ceiling installed-app measurement on top of the publish checks", (t) => {
  refused(evaluate(crate(t), { requireInstalled: true }), /no installed-app measurement for version 0\.2\.0/);
  const installed = (over = {}) => measurement("darwin", { scope: "installed", app: { name: "HeardRight", version: "1.2.3" }, ...over });
  const withApp = () => crate(t, { budget: budgetFor({ measurements: [installed(), measurement("win32")] }) });
  assert.deepEqual(evaluate(withApp(), { requireInstalled: true, app: "HeardRight" }), { ok: true, problems: [] });
  refused(evaluate(withApp(), { requireInstalled: true, app: "CutRight" }), /no installed-app measurement for version 0\.2\.0 in CutRight/);
  refused(evaluate(withApp(), { requireInstalled: true }, freshGit({ diff: { status: 0, stdout: "src/a.rs" } })), /stale/);
});

// ---------------------------------------------------------------- membership (fail closed)

const quietLib = "pub fn status() {}\npub fn add(a: u32, b: u32) -> u32 { a + b }\n";
const cont = (extraToml = "") => `[package]\nname = "rightkit-new"\nversion = "0.1.0"\n${extraToml}`;
const declared = (reason) => cont(`[package.metadata.rightkit]\ncontinuous = false\ncontinuous_reason = "${reason}"\n`);
const check = (dir, crateName = "rightkit-new", over = {}) => runtimeBudgetCheck({ crate: crateName, crateDir: dir, version: "0.1.0", ...over }, { git: freshGit() });

test("a crate with no declaration at all is refused, never skipped", (t) => {
  const result = check(crate(t, { file: false, readme: null, manifest: cont(), src: quietLib }));
  assert.equal(result.status, "fail");
  assert.match(result.detail, /undeclared: none of runtime-budget\.json, continuous = false with a continuous_reason, or continuous = "held-resource" with resources/);
});

test("Cargo.toml [package].name must equal the crate being published", (t) => {
  const result = check(crate(t, { file: false, readme: null, manifest: declared("pure library"), src: quietLib }), "rightkit-other");
  assert.equal(result.status, "fail");
  assert.match(result.detail, /\[package\]\.name "rightkit-new" differs from --crate rightkit-other/);
});

test("continuous = false needs a reason and passes on quiet source", (t) => {
  assert.match(check(crate(t, { file: false, readme: null, manifest: cont("[package.metadata.rightkit]\ncontinuous = false\n"), src: quietLib })).detail, /needs a one-line continuous_reason/);
  const ok = check(crate(t, { file: false, readme: null, manifest: declared("pure library, no work of its own"), src: quietLib }));
  assert.equal(ok.status, "ok");
  assert.match(ok.detail, /continuous = false: pure library/);
});

test("continuous = false is refused when src matches a continuous-work pattern the reason does not name", (t) => {
  const cases = {
    "thread-spawn": "pub fn go() { std::thread::spawn(|| {}); }",
    "task-spawn": "pub async fn go() { tokio::spawn(async {}); }",
    sleep: "pub fn go() { std::thread::sleep(std::time::Duration::from_secs(1)); }",
    timer: "pub async fn go() { let mut t = tokio::time::interval(D); }",
    listener: "pub fn go() { let l = std::net::TcpListener::bind(A); }",
    watcher: "pub fn go() { let w = notify::recommended_watcher(f); }",
  };
  for (const id of Object.keys(cases)) assert.ok(id in CONTINUOUS_PATTERNS, id);
  for (const [id, code] of Object.entries(cases)) {
    const dir = crate(t, { file: false, readme: null, manifest: declared("pure library"), src: `${code}\n` });
    assert.deepEqual(Object.keys(scanContinuousPatterns(dir)), [id]);
    const refusal = check(dir);
    assert.equal(refusal.status, "fail", id);
    assert.match(refusal.detail, new RegExp(`src matches ${id} \\(src/index\\.rs\\)`));
    // Naming the pattern is the reviewed decision that resolves a false positive.
    const named = check(crate(t, { file: false, readme: null, manifest: declared(`${id} is a one-shot call used only by a test helper`), src: `${code}\n` }));
    assert.equal(named.status, "ok", id);
  }
});

test("the continuous-work scan ignores comments and #[cfg(test)] modules but not shipped code after them", (t) => {
  const src = [
    "// std::thread::spawn in a comment",
    "/* tokio::spawn in a block comment */",
    "pub fn quiet() {}",
    "#[cfg(test)]",
    "mod tests {",
    "    fn t() { std::thread::spawn(|| { if true { } }); std::thread::sleep(D); }",
    "}",
    "pub fn after() { let l = UnixListener::bind(P); }",
    "",
  ].join("\n");
  const dir = crate(t, { file: false, readme: null, manifest: declared("pure"), src });
  assert.deepEqual(Object.keys(scanContinuousPatterns(dir)), ["listener"]);
});

test("the continuous-work scan skips use and re-export declarations but not real code after them", (t) => {
  const reexport = [
    "use std::net::TcpListener;",
    "pub use server::ServerOptions;",
    "pub(crate) use pool::ConnectionPool;",
    "pub fn quiet() {}",
    "",
  ].join("\n");
  assert.deepEqual(scanContinuousFiles(crate(t, { file: false, readme: null, manifest: declared("pure"), src: reexport })), {});

  const real = "pub use server::ServerOptions;\npub fn go() {\n    let l =\n        TcpListener::bind(A);\n}\n";
  assert.deepEqual(Object.keys(scanContinuousPatterns(crate(t, { file: false, readme: null, manifest: declared("pure"), src: real }))), ["listener"]);

  const multi = "pub use x::{\n    ServerOptions,\n    ConnectionPool\n};\npub fn quiet() {}\n";
  assert.deepEqual(scanContinuousFiles(crate(t, { file: false, readme: null, manifest: declared("pure"), src: multi })), {});
});

test("a listed continuous owner can never declare continuous = false, and a budget file plus continuous = false conflicts", (t) => {
  const listed = check(crate(t, { file: false, readme: null, manifest: '[package]\nname = "rightkit-sync"\nversion = "0.1.0"\n[package.metadata.rightkit]\ncontinuous = false\ncontinuous_reason = "x"\n', src: quietLib }), "rightkit-sync");
  assert.match(listed.detail, /listed as a continuous owner in RUNTIME_BUDGET_CRATES; it cannot declare continuous = false/);
  const both = check(crate(t, { budget: budgetFor({ crate: "rightkit-new" }), manifest: declared("pure"), src: quietLib }));
  assert.match(both.detail, /declares continuous = false but also carries runtime-budget\.json/);
  assert.match(check(crate(t, { file: false, readme: null, manifest: cont("[package.metadata.rightkit]\ncontinuous = \"no\"\n"), src: quietLib })).detail, /continuous must be false, true, "always-on" or "held-resource"/);
});

test("continuous = true or runtime_budget = true without a measured budget file is refused as missing", (t) => {
  for (const toml of ["continuous = true", "runtime_budget = true"]) {
    const result = check(crate(t, { file: false, readme: null, manifest: cont(`[package.metadata.rightkit]\n${toml}\n`), src: quietLib }));
    assert.equal(result.status, "fail");
    assert.match(result.detail, /runtime-budget\.json is missing/);
  }
});

test("runtimeBudgetCheck returns preflight-shaped ok/fail objects and the adoption wrapper requires installed", (t) => {
  const dir = crate(t);
  const ok = runtimeBudgetCheck({ crate: "rightkit-fsindex", crateDir: dir, version: "0.2.0" }, { git: freshGit() });
  assert.equal(ok.name, "runtime-budget");
  assert.equal(ok.status, "ok");
  const noFile = crate(t, { file: false, manifest: '[package]\nname = "rightkit-send"\nversion = "0.1.0"\n' });
  const fail = runtimeBudgetCheck({ crate: "rightkit-send", crateDir: noFile, version: "0.1.0" }, { git: freshGit() });
  assert.equal(fail.status, "fail");
  assert.match(fail.detail, /^rightkit-send: runtime-budget\.json is missing/);
  assert.match(fail.fix, /runtime-budget\.json/);
  assert.equal(runtimeBudgetAdoptionCheck({ crate: "rightkit-fsindex", crateDir: dir, version: "0.2.0" }, { git: freshGit() }).status, "fail");
});

test("runtime budget class: listed crates are in the class and an unlisted crate is not", () => {
  assert.equal(RUNTIME_BUDGET_CRATES.length, 10);
  assert.equal(isRuntimeBudgetCrate("rightkit-send"), true);
  assert.equal(isRuntimeBudgetCrate("rightkit-license"), false);
});

test("a crate opts in through [package.metadata.rightkit] runtime_budget = true", () => {
  const optedIn = '[package]\nname = "rightkit-new"\n[package.metadata.rightkit]\nruntime_budget = true\n';
  assert.equal(runtimeBudgetOptIn(optedIn), true);
  assert.equal(isRuntimeBudgetCrate("rightkit-new", optedIn), true);
  assert.equal(isRuntimeBudgetCrate("rightkit-new", '[package]\nname = "rightkit-new"\n'), false);
  assert.throws(() => runtimeBudgetOptIn('[package.metadata.rightkit]\nruntime_budget = "yes"\n'), /must be true or false/);
});

// ---------------------------------------------------------------- bench output format

test("parseBudgetResult accepts exactly one well-formed RESULT budget line", () => {
  const good = parseBudgetResult(`RESULT mem entries=5\n${resultLine(OBSERVED)}\nRESULT churn x=1\n`);
  assert.equal(good.ok, true);
  assert.deepEqual(good.value, { crate: "rightkit-fsindex", version: "0.2.0", scale_unit: "entries", ...OBSERVED, scaleCount: 1000000 });
  assert.match(parseBudgetResult("RESULT mem entries=5\n").problems[0], /found 0/);
  assert.match(parseBudgetResult(`${resultLine(OBSERVED)}\n${resultLine(OBSERVED)}\n`).problems[0], /found 2/);
  assert.match(parseBudgetResult(`${resultLine(OBSERVED)} extra=1\n`).problems.join(), /unknown field extra/);
  assert.match(parseBudgetResult(`${resultLine(OBSERVED)} crate=x\n`).problems.join(), /duplicate field crate/);
  assert.match(parseBudgetResult("RESULT budget crate=a version=1\n").problems.join(), /missing field scale_unit.*field resident_bytes/);
  assert.match(parseBudgetResult(resultLine({ ...OBSERVED, idleCpuPercent: -1 }) + "\n").problems.join(), /idle_cpu_percent must be a non-negative number/);
});

// ---------------------------------------------------------------- real git: freshness

const git = (cwd, ...args) => execFileSync("git", ["-c", "user.name=t", "-c", "user.email=t@example.test", "-c", "commit.gpgsign=false", ...args], { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
const LOCK = (serdeChecksum, tokioVersion) => `version = 4

[[package]]
name = "svc"
version = "0.2.0"
dependencies = ["dep-b", "serde 1.0.0"]

[[package]]
name = "dep-b"
version = "0.1.0"
dependencies = ["serde 1.0.0"]

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "${serdeChecksum}"

[[package]]
name = "sibling"
version = "0.1.0"
dependencies = ["tokio"]

[[package]]
name = "tokio"
version = "${tokioVersion}"
source = "registry+https://github.com/rust-lang/crates.io-index"
`;

function repo(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-budget-git-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const put = (rel, text) => { mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); writeFileSync(path.join(root, rel), text); };
  put("Cargo.toml", '[workspace]\nmembers = ["crates/*"]\n');
  put("Cargo.lock", LOCK("aa", "1.0.0"));
  put("crates/svc/Cargo.toml", '[package]\nname = "svc"\nversion = "0.2.0"\n[features]\ndefault = []\n[dependencies]\ndep-b = { path = "../dep-b", version = "0.1.0" }\nserde = "1"\n');
  put("crates/svc/src/lib.rs", "pub fn status() {}\n");
  put("crates/svc/benches/budget.rs", "fn main() {}\n");
  put("crates/svc/README.md", "# svc\n");
  put("crates/dep-b/Cargo.toml", '[package]\nname = "dep-b"\nversion = "0.1.0"\n[dependencies]\nserde = "1"\n');
  put("crates/dep-b/src/lib.rs", "pub fn b() {}\n");
  put("crates/sibling/Cargo.toml", '[package]\nname = "sibling"\nversion = "0.1.0"\n[dependencies]\ntokio = "1"\n');
  put("crates/sibling/src/lib.rs", "pub fn s() {}\n");
  git(root, "init", "-q", "-b", "main");
  git(root, "add", "-A");
  git(root, "commit", "-q", "-m", "measured");
  const measured = git(root, "rev-parse", "HEAD");
  const svc = path.join(root, "crates/svc");
  const edit = (rel, text, message = "change") => { put(rel, text); git(root, "add", "-A"); git(root, "commit", "-q", "-m", message); };
  return { root, svc, measured, put, edit };
}

test("real git: a measurement is fresh on an unchanged repository, and measuredPathspecs names the dependency paths", (t) => {
  const r = repo(t);
  assert.equal(measurementStaleness(r.measured, r.svc), null);
  const { specs } = measuredPathspecs(r.svc);
  assert.deepEqual(specs.sort(), ["crates/dep-b/Cargo.toml", "crates/dep-b/build.rs", "crates/dep-b/src", "crates/svc/Cargo.toml", "crates/svc/benches", "crates/svc/build.rs", "crates/svc/src"]);
});

test("real git: a change to a workspace path dependency makes the measurement stale", (t) => {
  const r = repo(t);
  r.edit("crates/dep-b/src/lib.rs", "pub fn b() { regress(); }\n");
  assert.match(measurementStaleness(r.measured, r.svc), /crate or workspace dependency changed since commit .*: crates\/dep-b\/src\/lib\.rs/);
});

test("real git: build.rs, features in Cargo.toml and benches of the crate make it stale; so does a dependency's build.rs", (t) => {
  const own = repo(t);
  own.edit("crates/svc/build.rs", "fn main() {}\n");
  assert.match(measurementStaleness(own.measured, own.svc), /crates\/svc\/build\.rs/);
  const feat = repo(t);
  feat.edit("crates/svc/Cargo.toml", '[package]\nname = "svc"\nversion = "0.2.0"\n[features]\ndefault = ["fast"]\nfast = []\n[dependencies]\ndep-b = { path = "../dep-b", version = "0.1.0" }\nserde = "1"\n');
  assert.match(measurementStaleness(feat.measured, feat.svc), /crates\/svc\/Cargo\.toml/);
  const bench = repo(t);
  bench.edit("crates/svc/benches/budget.rs", "fn main() { println!(); }\n");
  assert.match(measurementStaleness(bench.measured, bench.svc), /crates\/svc\/benches\/budget\.rs/);
  const dep = repo(t);
  dep.edit("crates/dep-b/build.rs", "fn main() {}\n");
  assert.match(measurementStaleness(dep.measured, dep.svc), /crates\/dep-b\/build\.rs/);
});

test("real git: uncommitted edits and untracked files under measured paths are stale", (t) => {
  const edited = repo(t);
  edited.put("crates/dep-b/src/lib.rs", "pub fn b() { dirty(); }\n");
  assert.match(measurementStaleness(edited.measured, edited.svc), /crates\/dep-b\/src\/lib\.rs/);
  const added = repo(t);
  added.put("crates/svc/src/extra.rs", "pub fn x() {}\n");
  assert.match(measurementStaleness(added.measured, added.svc), /untracked files since the measurement: crates\/svc\/src\/extra\.rs/);
});

test("real git: a Cargo.lock change inside the crate's dependency graph is stale, one outside it is not", (t) => {
  const inside = repo(t);
  inside.edit("Cargo.lock", LOCK("bb", "1.0.0"));
  assert.match(measurementStaleness(inside.measured, inside.svc), /Cargo\.lock changed in the dependency graph of svc/);
  const outside = repo(t);
  outside.edit("Cargo.lock", LOCK("aa", "9.9.9"));
  assert.equal(measurementStaleness(outside.measured, outside.svc), null);
  const removed = repo(t);
  rmSync(path.join(removed.root, "Cargo.lock"));
  assert.match(measurementStaleness(removed.measured, removed.svc), /Cargo\.lock was removed/);
});

test("real git: an untouched sibling crate outside the graph does not make the measurement stale", (t) => {
  const r = repo(t);
  r.edit("crates/sibling/src/lib.rs", "pub fn s() { changed(); }\n");
  r.put("crates/sibling/src/new.rs", "pub fn n() {}\n");
  r.edit("crates/svc/README.md", "# svc changed\n");
  assert.equal(measurementStaleness(r.measured, r.svc), null);
});

test("real git: a detached HEAD at a descendant is fresh, a detached HEAD at an unrelated commit is refused", (t) => {
  const r = repo(t);
  r.edit("crates/sibling/src/lib.rs", "pub fn s() { later(); }\n");
  const later = git(r.root, "rev-parse", "HEAD");
  git(r.root, "checkout", "-q", "--detach", later);
  assert.equal(measurementStaleness(r.measured, r.svc), null);
  git(r.root, "checkout", "-q", "--detach", r.measured);
  r.edit("crates/svc/src/lib.rs", "pub fn status() { branch(); }\n");
  const other = git(r.root, "rev-parse", "HEAD");
  git(r.root, "checkout", "-q", "--detach", r.measured);
  git(r.root, "checkout", "-q", "-b", "side");
  r.edit("crates/sibling/src/lib.rs", "pub fn s() { side(); }\n");
  assert.match(measurementStaleness(other, r.svc), /is not an ancestor of HEAD/);
});

test("real git: a shallow clone that lacks the measured commit refuses, one that has it passes", (t) => {
  const r = repo(t);
  r.edit("crates/sibling/src/lib.rs", "pub fn s() { second(); }\n");
  r.edit("crates/sibling/src/lib.rs", "pub fn s() { third(); }\n");
  const clones = mkdtempSync(path.join(os.tmpdir(), "rightkit-budget-clone-"));
  t.after(() => rmSync(clones, { recursive: true, force: true }));
  const shallow = path.join(clones, "shallow");
  execFileSync("git", ["clone", "-q", "--depth", "1", `file://${r.root}`, shallow], { stdio: "ignore" });
  assert.match(measurementStaleness(r.measured, path.join(shallow, "crates/svc")), /is not a commit in this repository \(shallow clone\?\)/);
  const deep = path.join(clones, "deep");
  execFileSync("git", ["clone", "-q", "--depth", "5", `file://${r.root}`, deep], { stdio: "ignore" });
  git(deep, "checkout", "-q", "--detach");
  assert.equal(measurementStaleness(r.measured, path.join(deep, "crates/svc")), null);
});

// ---------------------------------------------------------------- record: provenance end to end (real git)

test("record writes host, HEAD, evidence file and hash itself, and the gate then accepts it", (t) => {
  const r = repo(t);
  const crateName = "svc";
  const put = (rel, text) => r.put(rel, text);
  // A continuous crate: budget file with ceilings, scale, status pointer; no measurements yet.
  put("crates/svc/runtime-budget.json", JSON.stringify({
    schema: 1, crate: crateName, ceilings: { ...CEILINGS }, requiredScale: { unit: "entries", minCount: 1000 },
    status: { api: "status()", file: "src/lib.rs", exposes: ["residentMemoryBytes"] }, measurements: [],
  }));
  git(r.root, "add", "-A"); git(r.root, "commit", "-q", "-m", "budget file");
  const out = path.join(r.root, "bench-out.txt");
  writeFileSync(out, `bench noise\n${resultLine(OBSERVED, { crate: crateName, version: "0.2.0", count: 5000 })}\n`);
  const common = { crateDir: r.svc, resultFile: out, scope: "synthetic", command: "rightkit cargo bench -p svc --bench budget", description: "generated entries" };
  recordMeasurement(common, { host: { os: "darwin", arch: "arm64", model: "m" }, now: new Date("2026-10-11T00:00:00Z") });
  const record = recordMeasurement(common, { host: { os: "win32", arch: "x64" }, now: new Date("2026-10-11T00:00:00Z") });
  const written = JSON.parse(readFileSync(path.join(r.svc, "runtime-budget.json"), "utf8"));
  assert.equal(written.measurements.length, 2);
  assert.equal(record.commit, git(r.root, "rev-parse", "HEAD"));
  assert.deepEqual(record.dataScale, { description: "generated entries", unit: "entries", count: 5000 });
  assert.equal(record.evidence.file, "runtime-budget.evidence/win32-x64-0.2.0-synthetic.txt");
  assert.equal(record.evidence.sha256, crypto.createHash("sha256").update(readFileSync(out)).digest("hex"));
  assert.equal(readFileSync(path.join(r.svc, record.evidence.file), "utf8"), readFileSync(out, "utf8"));
  // Recording again for the same host replaces the record instead of stacking.
  recordMeasurement(common, { host: { os: "win32", arch: "x64" }, now: new Date("2026-10-12T00:00:00Z") });
  assert.equal(JSON.parse(readFileSync(path.join(r.svc, "runtime-budget.json"), "utf8")).measurements.length, 2);
  writeFileSync(path.join(r.svc, "README.md"), syncRuntimeBudgetReadme("# svc\n", JSON.parse(readFileSync(path.join(r.svc, "runtime-budget.json"), "utf8"))));
  const result = evaluateRuntimeBudget({ crate: crateName, crateDir: r.svc, version: "0.2.0" });
  assert.deepEqual(result, { ok: true, problems: [] });
  // Changing the measured code afterwards makes the recorded measurements stale.
  r.edit("crates/dep-b/src/lib.rs", "pub fn b() { regress(); }\n");
  refused(evaluateRuntimeBudget({ crate: crateName, crateDir: r.svc, version: "0.2.0" }), /stale: crate or workspace dependency changed/);
});

test("record refuses output for another crate or version, bad flags, a missing line, and uncommitted code", (t) => {
  const r = repo(t);
  r.put("crates/svc/runtime-budget.json", JSON.stringify({ schema: 1, crate: "svc", measurements: [] }));
  git(r.root, "add", "-A"); git(r.root, "commit", "-q", "-m", "budget file");
  const out = path.join(r.root, "out.txt");
  const base = { crateDir: r.svc, resultFile: out, scope: "synthetic", command: "c", description: "d" };
  const deps = { host: { os: "darwin", arch: "arm64" } };
  writeFileSync(out, resultLine(OBSERVED, { crate: "other", version: "0.2.0" }) + "\n");
  assert.throws(() => recordMeasurement(base, deps), /bench output is for other, crate is svc/);
  writeFileSync(out, resultLine(OBSERVED, { crate: "svc", version: "9.9.9" }) + "\n");
  assert.throws(() => recordMeasurement(base, deps), /bench output is for version 9\.9\.9, crate is 0\.2\.0/);
  writeFileSync(out, "nothing\n");
  assert.throws(() => recordMeasurement(base, deps), /found 0/);
  writeFileSync(out, resultLine(OBSERVED, { crate: "svc", version: "0.2.0" }) + "\n");
  assert.throws(() => recordMeasurement({ ...base, scope: "installed" }, deps), /needs --app and --app-version/);
  assert.throws(() => recordMeasurement({ ...base, scope: "guess" }, deps), /--scope must be one of/);
  assert.throws(() => recordMeasurement({ ...base, command: "" }, deps), /--command/);
  assert.throws(() => recordMeasurement({ ...base, description: "" }, deps), /--description/);
  r.put("crates/svc/src/lib.rs", "pub fn status() { uncommitted(); }\n");
  assert.throws(() => recordMeasurement(base, deps), /refusing to record: crate or workspace dependency changed/);
});

// ---------------------------------------------------------------- scanner gaps (new patterns)

test("the scanner finds each long-lived construct the classification found it was missing", (t) => {
  const cases = {
    "runtime-builder": ["pub fn go() { let rt = tokio::runtime::Builder::new_multi_thread().build(); }", "pub fn go() { let rt = Runtime::new(); }"],
    "audio-stream": ["pub fn go() { let host = cpal::default_host(); }", "pub fn go() { dev.build_input_stream(c, f, e, None); }"],
    "onnx-session": ["pub fn go() { let s = Session::builder(); }", "pub fn go() { ort::init().commit(); }", "pub fn go() { b.with_intra_threads(4); }"],
    ggml: ["pub fn go() { unsafe { ggml_init(p); } }", "pub fn go() { unsafe { ggml_backend_cpu_init(); } }", "pub fn go() { threadpool_new(4); }"],
    "event-loop": ["pub fn go() { let k = unsafe { kqueue() }; }", "pub fn go() { epoll_wait(fd, ev, 8, -1); }", "pub fn go() { CreateIoCompletionPort(h, 0, 0, 0); }"],
    "conn-pool": ["pub fn go() { let p = r2d2::Pool::builder(); }", "pub fn go() { let c = reqwest::Client::new(); }"],
    "resident-static": ["static CACHE: OnceLock<HashMap<String, u32>> = OnceLock::new();", "static MODELS: Mutex<Vec<Model>> = Mutex::new(Vec::new());", "lazy_static! { static ref X: u8 = 1; }"],
    "detached-child": ["pub fn go() { rightkit_daemon::spawn_detached(p, a, e); }", "pub fn go() { cmd.process_group(0); }"],
  };
  for (const [id, snippets] of Object.entries(cases)) {
    for (const code of snippets) {
      const dir = crate(t, { file: false, readme: null, manifest: declared("x"), src: `${code}\n` });
      assert.ok(Object.keys(scanContinuousPatterns(dir)).includes(id), `${id}: ${code}`);
    }
  }
  const quiet = crate(t, { file: false, readme: null, manifest: declared("x"), src: "static COUNTER: AtomicU64 = AtomicU64::new(1);\nstatic NAME: &str = \"x\";\nconst LIMIT: usize = 4;\n" });
  assert.deepEqual(scanContinuousFiles(quiet), {});
});

// ---------------------------------------------------------------- held-resource class

const resourceToml = (entries, head = 'continuous = "held-resource"') => `[package]\nname = "rightkit-held"\nversion = "0.1.0"\n[package.metadata.rightkit]\n${head}\n${entries}`;
const entry = (over = {}) => {
  const v = { api: "Engine::open", file: "src/engine.rs", holds: "thread", scale: "one worker thread per engine; queue grows with unread results", release: "Drop", ...over };
  return `[[package.metadata.rightkit.resources]]\n${Object.entries(v).filter(([, x]) => x !== undefined).map(([k, x]) => `${k} = ${JSON.stringify(x)}`).join("\n")}\n`;
};
const ENGINE = "pub struct Engine;\nimpl Engine {\n    pub fn open() -> Engine { std::thread::spawn(|| {}); Engine }\n}\nimpl Drop for Engine { fn drop(&mut self) {} }\n";
const held = (t, { toml = resourceToml(entry()), files = { "src/engine.rs": ENGINE }, name = "rightkit-held" } = {}) => {
  const dir = crate(t, { file: false, readme: null, manifest: toml.replace("rightkit-held", name), src: "", extra: files });
  return dir;
};
const heldCheck = (dir, over = {}) => runtimeBudgetCheck({ crate: "rightkit-held", crateDir: dir, version: "0.1.0", ...over }, { git: freshGit() });
const heldRefusal = (dir, pattern, over) => { const r = heldCheck(dir, over); assert.equal(r.status, "fail", r.detail); assert.match(r.detail, pattern); };

test("held-resource: a complete table that covers every flagged file passes at publish with no measurement", (t) => {
  const r = heldCheck(held(t));
  assert.equal(r.status, "ok", r.detail);
  assert.match(r.detail, /held-resource \(1 resource\(s\)\); measurement is required at app adoption, not at crate publish/);
});

test("held-resource: refuses a missing or empty table and an unknown class value", (t) => {
  heldRefusal(held(t, { toml: resourceToml("") }), /needs a non-empty \[\[package\.metadata\.rightkit\.resources\]\] table/);
  heldRefusal(held(t, { toml: resourceToml("resources = []\n") }), /non-empty/);
  heldRefusal(held(t, { toml: resourceToml(entry(), 'continuous = "sometimes"') }), /continuous must be false, true, "always-on" or "held-resource"/);
});

test("held-resource: every entry needs api, file, holds, scale and a real release", (t) => {
  for (const key of ["api", "file", "scale", "release"]) heldRefusal(held(t, { toml: resourceToml(entry({ [key]: undefined })) }), new RegExp(`resources\\[0\\] has no ${key}`));
  heldRefusal(held(t, { toml: resourceToml(entry({ holds: "everything" })) }), /holds must be one of thread\|task\|listener\|child\|memory/);
  heldRefusal(held(t, { toml: resourceToml(entry({ holds: undefined })) }), /holds must be one of/);
  heldRefusal(held(t, { toml: resourceToml(entry({ release: "never" })) }), /release must name Drop or the function that ends it/);
  assert.equal(heldCheck(held(t, { toml: resourceToml(entry({ holds: ["thread", "memory"] })) })).status, "ok");
});

test("held-resource: api must resolve to a pub item in the named file, which must be under src/", (t) => {
  heldRefusal(held(t, { toml: resourceToml(entry({ api: "Engine::missing" })) }), /api Engine::missing is not a pub item in src\/engine\.rs/);
  heldRefusal(held(t, { toml: resourceToml(entry({ api: "Engine::open" })), files: { "src/engine.rs": "// pub fn open()\nfn open() { std::thread::spawn(|| {}); }\n" } }), /not a pub item/);
  heldRefusal(held(t, { toml: resourceToml(entry({ file: "src/gone.rs" })) }), /file src\/gone\.rs does not exist/);
  heldRefusal(held(t, { toml: resourceToml(entry({ file: "build.rs" })), files: { "src/engine.rs": ENGINE, "build.rs": "pub fn open() {}" } }), /must be under src\//);
});

test("held-resource: a continuous pattern in a file no resource lists is refused, so new long-lived code cannot hide", (t) => {
  heldRefusal(held(t, { files: { "src/engine.rs": ENGINE, "src/pool.rs": "pub fn warm() { std::thread::sleep(D); let l = TcpListener::bind(A); }\n" } }), /src\/pool\.rs matches sleep, listener but no resource lists it \(new long-lived code cannot hide\)/);
});

test("held-resource: a flagged file needs a resource that holds what the pattern implies", (t) => {
  heldRefusal(held(t, { toml: resourceToml(entry({ holds: "memory" })) }), /src\/engine\.rs matches thread-spawn but no resource for it holds thread/);
  const task = "pub struct E;\nimpl E { pub fn open() { tokio::spawn(async {}); } }\n";
  heldRefusal(held(t, { files: { "src/engine.rs": task } }), /matches task-spawn but no resource for it holds task/);
  assert.equal(heldCheck(held(t, { toml: resourceToml(entry({ holds: "task" })), files: { "src/engine.rs": task } })).status, "ok");
  const listener = "pub struct E;\nimpl E { pub fn open() { let l = TcpListener::bind(A); } }\n";
  heldRefusal(held(t, { files: { "src/engine.rs": listener } }), /matches listener but no resource for it holds listener/);
});

test("held-resource: a process-wide static needs a static_note, and a detached child is never held", (t) => {
  const withStatic = "pub struct Engine;\nimpl Engine { pub fn open() -> Engine { Engine } }\nstatic CATALOG: OnceLock<HashMap<String, Font>> = OnceLock::new();\n";
  heldRefusal(held(t, { toml: resourceToml(entry({ holds: "memory" })), files: { "src/engine.rs": withStatic } }), /matches resident-static: a resource entry for it needs a static_note/);
  assert.equal(heldCheck(held(t, { toml: resourceToml(entry({ holds: "memory", static_note: "catalog is built on first use and bounded at 40 fonts" })), files: { "src/engine.rs": withStatic } })).status, "ok");
  const detached = "pub struct Engine;\nimpl Engine { pub fn open() { rightkit_daemon::spawn_detached(p, a, e); } }\n";
  heldRefusal(held(t, { toml: resourceToml(entry({ holds: "child" })), files: { "src/engine.rs": detached } }), /starts a detached child, which outlives any held value; the crate is always-on/);
});

test("held-resource: the 11 named owners cannot use it", (t) => {
  heldRefusal(held(t, { toml: resourceToml(entry()).replace("rightkit-held", "rightkit-sync"), name: "rightkit-sync" }), /listed continuous owner .* cannot use held-resource; it stays always-on/, { crate: "rightkit-sync" });
});

test("held-resource: adoption requires an installed measurement of the named app, publish does not", (t) => {
  const budget = (app) => ({ schema: 1, crate: "rightkit-held", ceilings: { ...CEILINGS }, requiredScale: { unit: "sessions", minCount: 2 },
    status: { api: "Engine::open", file: "src/engine.rs", exposes: ["residentMemoryBytes"] },
    measurements: app ? [measurement("darwin", { crateVersion: "0.1.0", scope: "installed", app, dataScale: { description: "two live sessions", unit: "sessions", count: 2 } })] : [] });
  const build = (b) => crate(t, { budget: b, manifest: resourceToml(entry()), src: "", extra: { "src/engine.rs": ENGINE } });
  assert.equal(heldCheck(build(budget(null))).status, "ok");
  const noApp = runtimeBudgetAdoptionCheck({ crate: "rightkit-held", crateDir: build(budget(null)), version: "0.1.0" }, { git: freshGit() });
  assert.match(noApp.detail, /must name the app whose installed measurement is required/);
  const none = runtimeBudgetAdoptionCheck({ crate: "rightkit-held", crateDir: build(budget(null)), version: "0.1.0", app: "HeardRight" }, { git: freshGit() });
  assert.match(none.detail, /no measurements recorded/);
  const good = runtimeBudgetAdoptionCheck({ crate: "rightkit-held", crateDir: build(budget({ name: "HeardRight", version: "2.0.0" })), version: "0.1.0", app: "HeardRight" }, { git: freshGit() });
  assert.equal(good.status, "ok", good.detail);
  assert.doesNotMatch(good.detail, /every required host/);
  const other = runtimeBudgetAdoptionCheck({ crate: "rightkit-held", crateDir: build(budget({ name: "HeardRight", version: "2.0.0" })), version: "0.1.0", app: "CutRight" }, { git: freshGit() });
  assert.match(other.detail, /no installed-app measurement for version 0\.1\.0 in CutRight/);
});

// ---------------------------------------------------------------- app qualification (preflight)

const lockWith = (...pairs) => pairs.map(([n, v]) => `[[package]]\nname = "${n}"\nversion = "${v}"\n`).join("\n");
const CLASSES = { "rightkit-held": { ceilings: { ...CEILINGS }, requiredScale: { unit: "sessions", minCount: 2 } } };
function appFixture(t, { records, lock = lockWith(["rightkit-held", "0.1.0"], ["serde", "1.0.0"]), evidence = "write" } = {}) {
  const root = mkdtempSync(path.join(os.tmpdir(), "rightkit-app-adoption-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  writeFileSync(path.join(root, "Cargo.lock"), lock);
  if (records) {
    mkdirSync(path.join(root, "runtime-budget.evidence"), { recursive: true });
    const written = records.map((m, i) => {
      if (evidence !== "write") return m;
      const rel = `runtime-budget.evidence/${i}.txt`;
      const raw = `${resultLine(m.observed, { crate: m.crate, version: m.crateVersion, unit: m.dataScale.unit, count: m.dataScale.count })}\n`;
      writeFileSync(path.join(root, rel), raw);
      return { ...m, evidence: { file: rel, sha256: crypto.createHash("sha256").update(raw).digest("hex") } };
    });
    writeFileSync(path.join(root, "runtime-budget-adoption.json"), JSON.stringify({ schema: 1, app: "HeardRight", measurements: written }));
  }
  return root;
}
const appRecord = (over = {}) => ({ crate: "rightkit-held", ...measurement("darwin", { crateVersion: "0.1.0", scope: "installed", app: { name: "HeardRight", version: "2.0.0" }, dataScale: { description: "two live sessions", unit: "sessions", count: 2 } }), ...over });
const appCheck = (root, over = {}) => appRuntimeBudgetAdoptionCheck({ appRoot: root, app: "HeardRight", version: "2.0.0", cargoLockPaths: [path.join(root, "Cargo.lock")], ...over }, { classes: CLASSES });

test("app qualification: an app that links no held-resource crate has no check", (t) => {
  assert.equal(appCheck(appFixture(t, { lock: lockWith(["serde", "1.0.0"]) })), null);
  assert.equal(appRuntimeBudgetAdoptionCheck({ appRoot: appFixture(t), app: "A", cargoLockPaths: [] }, { classes: CLASSES }), null);
  // The shipped classes file lists none yet, so every app passes today without a check.
  assert.equal(appRuntimeBudgetAdoptionCheck({ appRoot: appFixture(t), app: "A", cargoLockPaths: [] }), null);
});

test("app qualification: a missing adoption file, undeclared ceilings or scale, and a missing measurement refuse", (t) => {
  assert.match(appCheck(appFixture(t)).detail, /runtime-budget-adoption\.json is missing/);
  const undeclared = (c) => appRuntimeBudgetAdoptionCheck({ appRoot: appFixture(t, { records: [appRecord()] }), app: "HeardRight", version: "2.0.0", cargoLockPaths: [] , ...{} }, { classes: c });
  assert.equal(undeclared(CLASSES), null, "no lock path, no dependency");
  const root = appFixture(t, { records: [appRecord()] });
  const withClasses = (c) => appRuntimeBudgetAdoptionCheck({ appRoot: root, app: "HeardRight", version: "2.0.0", cargoLockPaths: [path.join(root, "Cargo.lock")] }, { classes: c });
  // A crate whose RightKit budget is undeclared never blocks the app: it warns as pending.
  const noCeilings = withClasses({ "rightkit-held": { ceilings: null, requiredScale: CLASSES["rightkit-held"].requiredScale } });
  assert.equal(noCeilings.status, "warn");
  assert.match(noCeilings.detail, /ceilings are not declared yet: rightkit-held/);
  assert.equal(withClasses({ "rightkit-held": { ceilings: CEILINGS, requiredScale: null } }).status, "warn");
  const pendingOnly = appRuntimeBudgetAdoptionCheck({ appRoot: appFixture(t), app: "HeardRight", version: "2.0.0", cargoLockPaths: [path.join(appFixture(t), "Cargo.lock")] }, { classes: { "rightkit-held": { ceilings: null, requiredScale: null } } });
  assert.ok(pendingOnly === null || pendingOnly.status === "warn", "a missing adoption file with only undeclared crates does not fail");
  assert.match(appCheck(appFixture(t, { records: [] })).detail, /no installed-scope measurement of HeardRight/);
});

test("app qualification: an installed measurement of this app at the locked version passes", (t) => {
  const result = appCheck(appFixture(t, { records: [appRecord()] }));
  assert.equal(result.status, "ok", result.detail);
});

test("app qualification refuses a measurement for another app, an older app version, another crate version, over ceiling, below scale, or without evidence", (t) => {
  assert.match(appCheck(appFixture(t, { records: [appRecord({ app: { name: "CutRight", version: "2.0.0" } })] })).detail, /is for app CutRight, not HeardRight/);
  assert.match(appCheck(appFixture(t, { records: [appRecord()] }), { version: "2.1.0" }).detail, /was taken at HeardRight 2\.0\.0, not the current 2\.1\.0/);
  assert.match(appCheck(appFixture(t, { records: [appRecord({ crateVersion: "0.0.9" })] })).detail, /no installed-scope measurement of HeardRight/);
  assert.match(appCheck(appFixture(t, { records: [appRecord({ observed: { ...OBSERVED, idleCpuPercent: 50 } })] })).detail, /exceeds idleCpuPercent/);
  assert.match(appCheck(appFixture(t, { records: [appRecord({ dataScale: { description: "one", unit: "sessions", count: 1 } })] })).detail, /below the required scale 2 sessions/);
  assert.match(appCheck(appFixture(t, { records: [appRecord()], evidence: "skip" })).detail, /has no evidence/);
  assert.match(appCheck(appFixture(t, { records: [appRecord({ observed: { ...OBSERVED, residentMemoryBytes: 0 } })] })).detail, /implausible/);
});

test("app qualification is wired into preflight, so doctor, --dry-run and every build run it", async () => {
  const { collectPreflight, preflightFailures } = await import("./preflight.mjs");
  const base = { platform: "mac", appRoot: process.cwd(), repoRoot: null, app: "HeardRight", version: "2.0.0", cargoLockPaths: ["/x/Cargo.lock"], isBrokerManagedHost: () => true, versionProbe: () => "x", ownership: false, projectType: "tauri" };
  const calls = [];
  const checks = collectPreflight({ ...base, runtimeBudgetAdoption: (input) => { calls.push(input); return { name: "runtime-budget", status: "fail", detail: "x", fix: "y" }; } });
  assert.deepEqual(calls.map((c) => [c.app, c.version, c.cargoLockPaths]), [["HeardRight", "2.0.0", ["/x/Cargo.lock"]]]);
  assert.ok(preflightFailures(checks).some((c) => c.name === "runtime-budget"));
  assert.ok(!collectPreflight({ ...base, runtimeBudgetAdoption: () => null }).some((c) => c.name === "runtime-budget"));
});

test("runtime-budget-classes.json is exactly what the crates' manifests generate", (t) => {
  const cratesDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../crates");
  // A Windows checkout may hold the file with CRLF; the generator writes LF.
  const shipped = readFileSync(new URL("./runtime-budget-classes.json", import.meta.url), "utf8").replace(/\r\n/g, "\n");
  assert.equal(shipped, renderClasses(heldResourceClasses(cratesDir)), "run: node runtime-budgets.mjs classes ../../crates");
  const dir = mkdtempSync(path.join(os.tmpdir(), "rightkit-classes-"));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  mkdirSync(path.join(dir, "a"), { recursive: true });
  mkdirSync(path.join(dir, "b"), { recursive: true });
  writeFileSync(path.join(dir, "a/Cargo.toml"), resourceToml(entry()).replace("rightkit-held", "a"));
  writeFileSync(path.join(dir, "a/runtime-budget.json"), JSON.stringify(budgetFor({ requiredScale: { unit: "sessions", minCount: 2, source: "x" } })));
  writeFileSync(path.join(dir, "b/Cargo.toml"), declared("pure").replace("rightkit-new", "b"));
  assert.deepEqual(heldResourceClasses(dir), { a: { ceilings: CEILINGS, requiredScale: { unit: "sessions", minCount: 2 } } });
});

// ---------------------------------------------------------------- filled examples against the real crates

const REAL = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../crates");
export const VAD_EXAMPLE = `
[package.metadata.rightkit]
continuous = "held-resource"

[[package.metadata.rightkit.resources]]
api = "Silero::load"
file = "src/silero.rs"
holds = ["memory", "thread"]
scale = "one ONNX session: model weights, ORT arenas and intra-op threads; fixed per Silero value, independent of audio length"
release = "Drop"
`;
export const SPEECH_CLIENT_EXAMPLE = `
[package.metadata.rightkit]
continuous = "held-resource"

[[package.metadata.rightkit.resources]]
api = "connect"
file = "src/lib.rs"
holds = ["thread", "memory"]
scale = "one reader thread and one channel per connection; buffered frames grow with unread responses"
release = "Drop"
`;
function realWithExample(crateName, example) {
  const dir = path.join(REAL, crateName);
  const manifest = path.join(dir, "Cargo.toml");
  // Replace the crate's own metadata.rightkit table with the example; every other file is the real one.
  const original = readFileSync(manifest, "utf8").replace(/\n\[package\.metadata\.rightkit\][\s\S]*$/, "\n");
  const readFile = (p, enc) => (path.resolve(p) === manifest ? `${original}${example}` : readFileSync(p, enc));
  return runtimeBudgetCheck({ crate: crateName, crateDir: dir, version: "0.0.0" }, { readFile, git: freshGit() });
}

test("filled example: rightkit-vad passes as held-resource against its real source", () => {
  const result = realWithExample("rightkit-vad", VAD_EXAMPLE);
  assert.equal(result.status, "ok", result.detail);
});

test("filled example: rightkit-speech-client's table is complete but the gate refuses it for its detached engine launch", () => {
  const result = realWithExample("rightkit-speech-client", SPEECH_CLIENT_EXAMPLE);
  assert.equal(result.status, "fail");
  // The reader thread is covered; the only problem is start_installed_engine's spawn_detached.
  assert.equal(result.detail, "rightkit-speech-client: src/lib.rs starts a detached child, which outlives any held value; the crate is always-on, or the launch moves behind a held handle");
});

test("a pattern id counts as named only as a whole token in the reason", (t) => {
  const src = "pub fn go() { std::thread::sleep(D); }\n";
  const incidental = check(crate(t, { file: false, readme: null, manifest: declared("sleepy library, no sleeping here"), src }));
  assert.equal(incidental.status, "fail");
  assert.match(incidental.detail, /src matches sleep \(src\/index\.rs\)/);
  assert.equal(check(crate(t, { file: false, readme: null, manifest: declared("one-shot sleep in a retry helper, reviewed"), src })).status, "ok");
});
