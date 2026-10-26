import assert from "node:assert/strict";
import { spawnSync } from "./background-process.mjs";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RULES, runAppCheck } from "./app-check.mjs";
import { findRustfmt } from "./deterministic-scans.mjs";
import { PARTIAL_EXIT } from "./bin/rightkit-gate.mjs";
import { ANCHORS, LEGION_SEMANTIC_RULES, ledgerCovers, ledgerEntries, legacySkipped, retirementLedgerFor, rustStrList, tokenLines } from "./legion-semantic-contracts.mjs";
import { FIXTURE_IDENTITY, HISTORICAL_DELETIONS, LEDGER_HEAD, PRE_BASELINE_FILE, legionHistory, writeFiles } from "./fixtures/legion-semantic/history.mjs";

// Legion's historical catalog and provenance failures (Orthic-Labs/legion), each replayed through the real entrypoint,
// `rightkit-gate check` (bin/rightkit-gate.mjs), on a committed fixture repository: the original failure (red), its repair (green)
// and a legitimate case the contract must keep passing.

const HERE = path.dirname(fileURLToPath(import.meta.url));
const GATE = path.join(HERE, "bin", "rightkit-gate.mjs");
const FIXTURES = path.join(HERE, "fixtures", "legion-semantic");
const fixture = (rel) => fs.readFileSync(path.join(FIXTURES, rel), "utf8");
const env = () => Object.fromEntries(Object.entries({ ...process.env, ...FIXTURE_IDENTITY }).filter(([k]) => !/^GIT_(DIR|WORK_TREE|INDEX_FILE)$/.test(k)));

function sh(cwd, args, input) {
  const r = spawnSync("git", ["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false", ...args], { cwd, encoding: "utf8", env: env(), input });
  assert.equal(r.status, 0, `git ${args.join(" ")}: ${r.stderr}`);
  return r.stdout.trim();
}
const tmp = (t, tag) => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), `rk-legion-${tag}-`));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
};
/** A repository with one commit holding `files`. */
function repoWith(t, files, tag = "repo") {
  const dir = tmp(t, tag);
  sh(dir, ["init", "-q", "-b", "main"]);
  writeFiles(dir, files);
  sh(dir, ["add", "-A"]);
  sh(dir, ["commit", "-q", "-m", "fixture"]);
  return dir;
}
function commitChange(dir, { write = {}, remove = [], message = "change" }) {
  writeFiles(dir, write);
  for (const rel of remove) sh(dir, ["rm", "-q", rel]);
  sh(dir, ["add", "-A"]);
  sh(dir, ["commit", "-q", "-m", message]);
}
/** `rightkit-gate check --repo <dir> --commit HEAD --only <rule>`: exit code and lines. */
function check(dir, rule) {
  const r = spawnSync(process.execPath, [GATE, "check", "--repo", dir, "--commit", "HEAD", "--only", rule], { encoding: "utf8", env: env() });
  return { status: r.status, out: r.stdout, err: r.stderr, fails: r.stdout.split("\n").filter((l) => l.startsWith("FAIL ")) };
}
const green = (r) => { assert.equal(r.status, PARTIAL_EXIT, r.out + r.err); assert.match(r.out, /^check: \w{12} passed 1 rules \(selected\) in/m); };

test("legion semantic contracts: four LG rules, registered in the app check after the shell contract, anchored to Legion's own files", () => {
  assert.deepEqual(LEGION_SEMANTIC_RULES.map((r) => [r.id, r.gap]), [["legion-schema-projection", "LG-02"], ["legion-legacy-tokens", "LG-03"], ["legion-retirement-ledger", "LG-04"], ["legion-authority-body", "LG-05"]]);
  assert.deepEqual(RULES.slice(-11, -7).map((r) => r.id), LEGION_SEMANTIC_RULES.map((r) => r.id));
  const list = spawnSync(process.execPath, [GATE, "check", "--list"], { encoding: "utf8" });
  for (const r of LEGION_SEMANTIC_RULES) assert.match(list.stdout, new RegExp(`^${r.id} \\(${r.gap}\\) release$`, "m"), "Legion's rules are release tier");
  for (const anchors of Object.values(ANCHORS)) assert.ok(anchors.every((a) => !a.startsWith("/") && !a.includes("*")), "anchors are exact paths");
});

test("legion semantic contracts: a repository without Legion's anchors is not touched (no false red elsewhere)", (t) => {
  const dir = repoWith(t, { "schemas/provider-result-v1.schema.json": "{}\n", "docs/provenance/notes.md": "x\n", "agents/alchemist.md": "forge\n", "README.md": "sentinel forge\n" }, "other");
  for (const r of LEGION_SEMANTIC_RULES) green(check(dir, r.id));
});

// ---- contract 2: schema code-enum projection (run 31302567212, jobs 93217599020 and 93217599034) ----------------------------------
// The committed schemas were LF; Windows checked them out as CRLF (no .gitattributes at 8c275f0c) and the byte comparison of
// scripts/generate-schemas.mjs --check printed SCHEMA DRIFT for all three, while the Linux jobs printed OK. Legion's repair is the
// `* -text` .gitattributes it has today.
const HISTORICAL_SCHEMAS = ["schemas/provider-result-v1.schema.json", "schemas/security-verdict-v1.schema.json", "schemas/platform/web-actor-fixture-v1.schema.json"];
const at8c275f0c = () => ({ "scripts/generate-schemas.mjs": "// generator stand-in: the rule never runs it\n", ...Object.fromEntries(HISTORICAL_SCHEMAS.map((p) => [p, "{\n  \"type\": \"object\"\n}\n"])) });

test("LG-02 original negative: 8c275f0c's byte-compared schemas with no line-ending pin fail before any build", (t) => {
  const r = check(repoWith(t, at8c275f0c(), "schema-red"), "legion-schema-projection");
  assert.equal(r.status, 1);
  assert.deepEqual(r.fails.map((l) => l.split(" is byte-compared")[0]), HISTORICAL_SCHEMAS.map((p) => `FAIL legion-schema-projection (LG-02): ${p}`));
  assert.match(r.fails[0], /not pinned against line-ending conversion \(text: unspecified, eol: unspecified\).*SCHEMA DRIFT: schemas\/provider-result-v1\.schema\.json is not up to date with the code-owned enums/);
});

test("LG-02 repair: Legion's `* -text` .gitattributes passes", (t) => {
  green(check(repoWith(t, { ...at8c275f0c(), ".gitattributes": "* -text\n" }, "schema-green"), "legion-schema-projection"));
});

test("LG-02 negative: attributes outside the checked commit (an uncommitted .git/info/attributes, a global attributes file) do not pin it", (t) => {
  const dir = repoWith(t, at8c275f0c(), "schema-outside");
  fs.writeFileSync(path.join(dir, ".git", "info", "attributes"), "* -text\n");
  const home = tmp(t, "schema-home");
  fs.mkdirSync(path.join(home, "git"));
  fs.writeFileSync(path.join(home, "git", "attributes"), "* -text\n");
  writeFiles(dir, { ".gitattributes": "* -text\n" }); // on disk, never committed
  const r = spawnSync(process.execPath, [GATE, "check", "--repo", dir, "--commit", "HEAD", "--only", "legion-schema-projection"], { encoding: "utf8", env: { ...env(), XDG_CONFIG_HOME: home } });
  assert.equal(r.status, 1, r.stdout + r.stderr);
  assert.equal(r.stdout.split("\n").filter((l) => l.startsWith("FAIL ")).length, 3);
});

test("LG-02 negative: a later, more specific `text` line in the committed .gitattributes unpins one schema", (t) => {
  const r = check(repoWith(t, { ...at8c275f0c(), ".gitattributes": "* -text\nschemas/provider-result-v1.schema.json text\n" }, "schema-override"), "legion-schema-projection");
  assert.equal(r.status, 1);
  assert.equal(r.fails.length, 1);
  assert.match(r.fails[0], /^FAIL legion-schema-projection \(LG-02\): schemas\/provider-result-v1\.schema\.json is byte-compared .*\(text: set, eol: unspecified\)/);
});

const ENUMS_RS = `/// stand-in for engine/crates/legion-contracts/src/enums.rs
pub const PROVIDER_STATUS: &[&str] = &[
    "pass",
    "fail", // trailing comment
    "partial",
];
pub const EVIDENCE_STRENGTH: &[&str] = &["possible", "strong-inference", "verified"];
pub const EVIDENCE_STRENGTH_LEGACY: &[(&str, &str)] = &[("weak", "possible")];
pub const SECURITY_VERDICTS: &[&str] = &[
    "TRUE_POSITIVE",
    "FALSE_POSITIVE",
];
`;
const current = (status = ["pass", "fail", "partial"]) => ({
  ".gitattributes": "src/schemas/** eol=lf\n",
  "engine/bins/legion-dev/src/generators/schemas.rs": "// generator stand-in\n",
  "engine/crates/legion-contracts/src/enums.rs": ENUMS_RS,
  "src/schemas/provider-result-v1.schema.json": JSON.stringify({ properties: { status: { enum: status } } }, null, 2) + "\n",
  "src/schemas/security-verdict-v1.schema.json": JSON.stringify({ properties: { evidenceStrength: { enum: ["possible", "strong-inference", "verified"] }, verdict: { enum: ["TRUE_POSITIVE", "FALSE_POSITIVE"] } } }, null, 2) + "\n",
  "src/schemas/platform/web-actor-fixture-v1.schema.json": "{}\n",
});

test("LG-02 positive control: today's layout, pinned per path with eol=lf, every enum the projection of its Rust list", (t) => {
  green(check(repoWith(t, current(), "schema-pos"), "legion-schema-projection"));
});

test("LG-02 negative: a code-owned enum that gains a value without regenerating the schema fails (the --check's semantic half)", (t) => {
  const files = current();
  files["engine/crates/legion-contracts/src/enums.rs"] = ENUMS_RS.replace('"partial",', '"partial",\n    "deferred",');
  const r = check(repoWith(t, files, "schema-enum"), "legion-schema-projection");
  assert.equal(r.status, 1);
  assert.deepEqual(r.fails, ['FAIL legion-schema-projection (LG-02): src/schemas/provider-result-v1.schema.json/properties/status/enum is ["pass","fail","partial"] but engine/crates/legion-contracts/src/enums.rs PROVIDER_STATUS is ["pass","fail","partial","deferred"]: the schema is not the projection of its code-owned enum']);
});

test("LG-02 Rust list adapter: literals only; anything else is refused, never evaluated", () => {
  assert.deepEqual(rustStrList(ENUMS_RS, "EVIDENCE_STRENGTH"), { values: ["possible", "strong-inference", "verified"] });
  assert.match(rustStrList('pub const X: &[&str] = &[concat!("a", "b")];', "X").problem, /not a plain list of string literals/);
  assert.match(rustStrList("", "X").problem, /declares no/);
});

// ---- contract 3: legacy occurrence ledger (jobs 120707319434/491 at e93cb7c5, 118763807969/988 at b797eec5) ----------------------
const pad = (n, line) => `${"// filler\n".repeat(n - 1)}${line}\n`;
// The ledger classifies its own occurrences too, as Legion's does (its self-rule counts forge 64 times today).
const ALLOW = (rules) => {
  const doc = { schemaVersion: 1, rules: [{ path: "src/config/naming-legacy-allowlist.json", tokens: ["forge"], occurrences: { forge: 0 }, class: "R2", reason: "legacy-token classification source" }, ...rules] };
  doc.rules[0].occurrences.forge = tokenLines(JSON.stringify(doc, null, 2), "forge").length;
  return `${JSON.stringify(doc, null, 2)}\n`;
};
const CHECKLIST_RULE = (n) => ({ path: "skills/audit/references/security-checklist.md", tokens: ["forge"], occurrences: { forge: n }, class: "R0", reason: "ordinary security verb" });
// e93cb7c5: the two source lines verbatim (ingest.rs:67, audit_journal.rs:239); the checklist's three remaining forge lines at 105, 110, 520.
const checklist = () => `${"x\n".repeat(104)}*attackers forge tokens by switching it to "none"*\n${"x\n".repeat(4)}*Weak or static signing secrets let attackers forge tokens*\n${"x\n".repeat(409)}*attackers can forge entries or break your logging pipeline*\n`;
const atE93cb7c5 = () => ({
  "src/config/naming-legacy-allowlist.json": ALLOW([CHECKLIST_RULE(4)]),
  "engine/crates/legion-audit/src/native_providers/reasoning/ingest.rs": pad(67, "/// Who can forge receipts: the invoking OS user (and so any agent running as"),
  "engine/crates/legion-audit/tests/audit_journal.rs": pad(239, "    // Forge a later event bound to the superseded plan."),
  "skills/audit/references/security-checklist.md": checklist(),
});

test("LG-03 original negative (job 120707319434): unclassified tokens and a stale exact count fail with Legion's own lines", (t) => {
  const r = check(repoWith(t, atE93cb7c5(), "tokens-red"), "legion-legacy-tokens");
  assert.equal(r.status, 1);
  assert.deepEqual(r.fails, [
    "FAIL legion-legacy-tokens (LG-03): engine/crates/legion-audit/src/native_providers/reasoning/ingest.rs:67: unclassified legacy token (forge)",
    "FAIL legion-legacy-tokens (LG-03): engine/crates/legion-audit/tests/audit_journal.rs:239: unclassified legacy token (forge)",
    "FAIL legion-legacy-tokens (LG-03): skills/audit/references/security-checklist.md:105: legacy token occurrence count differs: expected 4, found 3 (forge)",
  ]);
});

test("LG-03 repair: the two lines reworded and the checklist count set to 3 (what Legion's tree holds today) passes", (t) => {
  const files = { ...atE93cb7c5(), "src/config/naming-legacy-allowlist.json": ALLOW([CHECKLIST_RULE(3)]) };
  files["engine/crates/legion-audit/src/native_providers/reasoning/ingest.rs"] = pad(67, "/// Who can mint receipts: the invoking OS user (and so any agent running as");
  files["engine/crates/legion-audit/tests/audit_journal.rs"] = pad(239, "    // Mint a later event bound to the superseded plan.");
  green(check(repoWith(t, files, "tokens-green"), "legion-legacy-tokens"));
});

// b797eec5 scanned immutable qualification evidence (records/<provider>/evidence/); Legion ratified it as exempt in its producer
// (SKIP_PREFIXES), so RightKit's copy keeps it exempt, and an evidence look-alike is still scanned (canonical_names.rs tests).
const EVIDENCE = "qualification/provider-accuracy/records/code.mobile/evidence/4922beb4d9e9327fe6766283b5c64ec91bad6b46aac38dd030b5efb9001d7b6c/mobile-native-after.json";
test("LG-03 positive control (job 118763807969's tree): immutable evidence and an R5 prefix rule without counts pass", (t) => {
  const files = { ...atE93cb7c5(), "src/config/naming-legacy-allowlist.json": ALLOW([CHECKLIST_RULE(3), { pathPrefix: "docs/audits/2026-10-08/", tokens: ["forge"], class: "R5", reason: "lineage evidence" }]) };
  files["engine/crates/legion-audit/src/native_providers/reasoning/ingest.rs"] = "// none\n";
  files["engine/crates/legion-audit/tests/audit_journal.rs"] = "// none\n";
  files[EVIDENCE] = '{"probe":"sentinel"}\n';
  files["docs/audits/2026-10-08/history.md"] = "Forge was the old name. forge forge\n";
  green(check(repoWith(t, files, "tokens-pos"), "legion-legacy-tokens"));
  assert.ok(legacySkipped(EVIDENCE));
  assert.ok(!legacySkipped("qualification/provider-accuracy/records/code.mobile/evidence-extra/payload.json"));
  assert.ok(!legacySkipped("qualification/provider-accuracy/records//evidence/payload.json"));
});

test("LG-03 negative: an evidence look-alike outside the ratified prefix and a legacy file name still fail", (t) => {
  const files = { ...atE93cb7c5(), "src/config/naming-legacy-allowlist.json": ALLOW([CHECKLIST_RULE(3)]) };
  files["engine/crates/legion-audit/src/native_providers/reasoning/ingest.rs"] = "// none\n";
  files["engine/crates/legion-audit/tests/audit_journal.rs"] = "// none\n";
  files["qualification/provider-accuracy/records/code.mobile/evidence-extra/payload.json"] = '{"probe":"sentinel"}\n';
  files["scripts/seer-report.md"] = "report\n";
  const r = check(repoWith(t, files, "tokens-neg"), "legion-legacy-tokens");
  assert.deepEqual(r.fails, [
    "FAIL legion-legacy-tokens (LG-03): qualification/provider-accuracy/records/code.mobile/evidence-extra/payload.json:1: unclassified legacy token (sentinel)",
    "FAIL legion-legacy-tokens (LG-03): scripts/seer-report.md: unclassified legacy filename (seer)",
  ]);
  assert.deepEqual(tokenLines("Forge\nforged xforge forge_x forge", "forge"), [1, 2, 2], "whole word, ASCII case-insensitive, `_` is a boundary");
});

// ---- contract 4: retirement ledger (job 119412709257/288 at 3c11aa76) --------------------------------------------------------------
// FIXTURE SEAM. Legion's baseline c1d80c9d190e0f45dfd630d4d65e8bed748381ce is not in any history this test can hold (the original
// object is not reproduced, and no prefix-colliding stand-in is made). The red, repair and positive replays below therefore run
// runAppCheck (the function `rightkit-gate check` calls, same export, baseline, ratchet and report) with the LG-04 rule bound to the
// fixture's own baseline through retirementLedgerFor(); they are fixture-seam evidence, not production-entrypoint evidence. The
// shipped rule stays bound to the full id c1d80c9d190e0f45dfd630d4d65e8bed748381ce; the two negatives after these run the
// real bin against that production binding. The eight deleted paths are the job's; their contents are stand-ins.
function seamCheck(dir, baseline) {
  const rules = [{ id: "legion-retirement-ledger", gap: "LG-04", run: retirementLedgerFor(baseline) }];
  const r = runAppCheck({ repo: dir, rules, central: null });
  return { status: r.exitCode, out: r.text, err: r.errText, fails: r.lines.filter((l) => l.startsWith("FAIL ")) };
}
const seamGreen = (r) => { assert.equal(r.status, 0, r.out + r.err); assert.match(r.out, /^check: \w{12} passed 1 rules \(selected\) in/m); };
const ledgerLine = (p, b) => `FAIL legion-retirement-ledger (LG-04): ${p}: deleted since ${b.slice(0, 8)} with no row in docs/provenance/retirements.md`;

test("LG-04 original negative [fixture seam]: 3c11aa76 deleted eight fixtures with no ledger row, and the check names each", (t) => {
  const dir = tmp(t, "ledger-red");
  const { baseline } = legionHistory(dir);
  commitChange(dir, { remove: HISTORICAL_DELETIONS, message: "reasoning corpus repair" });
  const r = seamCheck(dir, baseline);
  assert.equal(r.status, 1);
  assert.deepEqual(r.fails, [...HISTORICAL_DELETIONS].sort().map((p) => ledgerLine(p, baseline)));
});

test("LG-04 repair [fixture seam]: one row per path (Legion's ledger rows 300-307 today) passes", (t) => {
  const dir = tmp(t, "ledger-green");
  const { baseline } = legionHistory(dir);
  const rows = HISTORICAL_DELETIONS.map((p) => `| \`${p}\` | 2026-10-22 3c11aa76 reasoning corpus repair | none | intentionally dropped |\n`).join("");
  commitChange(dir, { remove: HISTORICAL_DELETIONS, write: { "docs/provenance/retirements.md": LEDGER_HEAD + rows } });
  seamGreen(seamCheck(dir, baseline));
});

test("LG-04 positive control [fixture seam]: parent and glob rows, the derived-manifest exemption, a rename, a restore, a non-ledger path and a pre-baseline deletion all pass", (t) => {
  const dir = tmp(t, "ledger-pos");
  const { baseline, git } = legionHistory(dir);
  assert.ok(!fs.existsSync(path.join(dir, PRE_BASELINE_FILE)), "deleted in the baseline commit itself: before the range");
  commitChange(dir, { remove: ["hooks/restored.sh", "docs/notes.md", "skills/manifests/demo.json", "scripts/tools/old-tool.mjs", "engine/bins/legion/src/commands/old_cmd.rs", ...HISTORICAL_DELETIONS] });
  fs.mkdirSync(path.join(dir, "skills/demo-renamed"));
  git(["mv", "skills/demo/SKILL.md", "skills/demo-renamed/SKILL.md"]);
  commitChange(dir, { write: { "hooks/restored.sh": "#!/bin/sh\nexit 0\n", "docs/provenance/retirements.md": `${LEDGER_HEAD}| \`scripts/ci/fixtures/audit-reasoning-runtime-accuracy/\` | x | none | dropped |\n| \`scripts/tools/*.mjs\`, \`engine/bins/**/old_cmd.rs\` | x | none | dropped |\n` } });
  seamGreen(seamCheck(dir, baseline));
  assert.deepEqual(ledgerEntries("| `a/b`, `./c/*.md` | x |\n|`d`|\nno | `e` |"), ["a/b", "c/*.md", "d"]);
  assert.ok(ledgerCovers("engine/bins/legion/src/commands/old_cmd.rs", ["engine/bins/**/old_cmd.rs"]));
  assert.ok(!ledgerCovers("scripts/tools/sub/x.mjs", ["scripts/tools/*.mjs"]), "* stays within one path segment");
  assert.ok(ledgerCovers("scripts/tools/x.mjs/inner", ["scripts/tools/*.mjs"]), "and covers anything below what it matches");
});

test("LG-04 negative [fixture seam]: a different object id that shares the baseline's first digits, or the bare prefix, is refused", (t) => {
  const dir = tmp(t, "ledger-prefix");
  const { baseline } = legionHistory(dir);
  commitChange(dir, { remove: HISTORICAL_DELETIONS });
  const lookalike = baseline.slice(0, 8) + (baseline.slice(8, 9) === "0" ? "1" : "0").repeat(32);
  assert.notEqual(lookalike, baseline);
  const refuse = (id) => runAppCheck({ repo: dir, rules: [{ id: "legion-retirement-ledger", gap: "LG-04", run: retirementLedgerFor(id) }], central: null }).lines.filter((l) => l.startsWith("FAIL "));
  assert.deepEqual(refuse(lookalike), [`FAIL legion-retirement-ledger (LG-04): cannot enumerate files deleted since ${baseline.slice(0, 8)}: ${lookalike} is not a commit here; a ledger that is not checked is not clean`]);
  assert.deepEqual(refuse(baseline.slice(0, 8)), [`FAIL legion-retirement-ledger (LG-04): cannot enumerate files deleted since ${baseline.slice(0, 8)}: the baseline ${baseline.slice(0, 8)} is not a full object id; a ledger that is not checked is not clean`]);
});

test("LG-04 negative [fixture seam]: a tag named after the baseline prefix at HEAD does not empty the range", (t) => {
  const dir = tmp(t, "ledger-ref2");
  const { baseline } = legionHistory(dir);
  commitChange(dir, { remove: HISTORICAL_DELETIONS });
  sh(dir, ["tag", baseline.slice(0, 12)]);
  const r = seamCheck(dir, baseline);
  assert.equal(r.status, 1);
  assert.equal(r.fails.length, HISTORICAL_DELETIONS.length);
});

test("LG-04 negative [production entrypoint]: a shallow clone cannot enumerate deletions and fails instead of passing unverified", (t) => {
  const dir = tmp(t, "ledger-src");
  legionHistory(dir);
  commitChange(dir, { remove: HISTORICAL_DELETIONS });
  const shallow = path.join(tmp(t, "ledger-shallow"), "c");
  sh(os.tmpdir(), ["clone", "-q", "--depth", "1", `file://${dir}`, shallow]);
  const r = check(shallow, "legion-retirement-ledger");
  assert.equal(r.status, 1);
  assert.deepEqual(r.fails, ["FAIL legion-retirement-ledger (LG-04): cannot enumerate files deleted since c1d80c9d: the clone is shallow; a ledger that is not checked is not clean"]);
});

test("LG-04 negative [production entrypoint]: a branch named c1d80c9d is not the baseline (no caller-chosen range)", (t) => {
  const dir = repoWith(t, { "docs/provenance/retirements.md": LEDGER_HEAD, "scripts/a.sh": "a\n" }, "ledger-ref");
  sh(dir, ["branch", "c1d80c9d"]);
  commitChange(dir, { remove: ["scripts/a.sh"] });
  const r = check(dir, "legion-retirement-ledger");
  assert.equal(r.status, 1);
  assert.match(r.fails.join("\n"), /cannot enumerate files deleted since c1d80c9d: c1d80c9d190e0f45dfd630d4d65e8bed748381ce is not a commit here/);
});

// ---- contract 5: authority-body parity (job 117161756781/757149 at 515ccd46) -------------------------------------------------------
// agents/alchemist.md, agents/alchemist-specialist.md, doctrine/alchemist.md and both roster files are 515ccd46's bytes; the other
// roles are minimal stand-ins that satisfy the relation.
const stand = (id) => ({ [`doctrine/${id}.md`]: `---\nname: ${id}\n---\n\n${id} method.\n`, [`agents/${id}.md`]: `---\nname: ${id}\n---\n\n## Method\n\n${id} method.\n` });
const at515ccd46 = () => ({
  ...stand("sage"), ...stand("oracle"), ...stand("sentry"),
  "doctrine/council-seat.md": "---\ndescription: seat\n---\n\nSeat body.\n", "agents/council-seat.md": "---\ndescription: seat\nmodel: x\n---\n\nSeat body.\n",
  ...Object.fromEntries(["agents/alchemist.md", "agents/alchemist-specialist.md", "doctrine/alchemist.md", "src/roster/alchemist.md", "src/roster/alchemist-specialist.md"].map((p) => [p, fixture(`authority-515ccd46/${p}`)])),
});
/** The card with its embedded method (it runs to the end of the card) replaced by the current doctrine body. */
const reembed = (card, doctrine) => card.slice(0, card.indexOf("You are **Alchemist**")) + doctrine.replace(/^---\n[\s\S]*?\n---\n/, "").trim() + "\n";

test("LG-05 original negative: doctrine/alchemist.md changed and neither alchemist card re-embedded it", (t) => {
  const r = check(repoWith(t, at515ccd46(), "authority-red"), "legion-authority-body");
  assert.equal(r.status, 1);
  assert.deepEqual(r.fails, [
    "FAIL legion-authority-body (LG-05): alchemist: agents/alchemist.md does not contain the doctrine/alchemist.md body verbatim under ## Method",
    "FAIL legion-authority-body (LG-05): alchemist-specialist: agents/alchemist-specialist.md does not contain the doctrine/alchemist.md body verbatim under ## Method",
  ]);
});

test("LG-05 repair: both cards carry the current doctrine body", (t) => {
  const files = at515ccd46();
  for (const card of ["agents/alchemist.md", "agents/alchemist-specialist.md"]) files[card] = reembed(files[card], files["doctrine/alchemist.md"]);
  green(check(repoWith(t, files, "authority-green"), "legion-authority-body"));
});

test("LG-05 positive control: a CRLF card, extra card-only sections and the producer anchor alone still pass", (t) => {
  const files = at515ccd46();
  for (const card of ["agents/alchemist.md", "agents/alchemist-specialist.md"]) files[card] = reembed(files[card], files["doctrine/alchemist.md"]);
  files["agents/sage.md"] = "---\r\nname: sage\r\n---\r\n\r\n## Dispatch\r\n\r\nCard-only text.\r\n\r\n## Method\r\n\r\nsage method.\r\n";
  files["engine/bins/legion-dev/src/checks/authority_parity.rs"] = "// producer stand-in\n";
  green(check(repoWith(t, files, "authority-pos"), "legion-authority-body"));
});

test("LG-05 negative: a card that points at doctrine/ outside its method, and a council seat whose body drifted, fail", (t) => {
  const files = at515ccd46();
  for (const card of ["agents/alchemist.md", "agents/alchemist-specialist.md"]) files[card] = reembed(files[card], files["doctrine/alchemist.md"]);
  files["agents/oracle.md"] = "---\nname: oracle\n---\n\nSee doctrine/oracle.md.\n\n## Method\n\noracle method.\n";
  files["agents/council-seat.md"] = "---\ndescription: seat\n---\n\nSeat body, edited.\n";
  const r = check(repoWith(t, files, "authority-neg"), "legion-authority-body");
  assert.deepEqual(r.fails, [
    "FAIL legion-authority-body (LG-05): oracle: agents/oracle.md refers to `doctrine/` outside its embedded method; the installed card cannot read that path",
    "FAIL legion-authority-body (LG-05): council-seat: agents/council-seat.md body differs from doctrine/council-seat.md body; the card must carry the doctrine body verbatim",
  ]);
});

// ---- contract 1: canonical regeneration (job 120769862741/762 at 7d9a8a35) --------------------------------------------------------
// Legion's regeneration produced one change: rustfmt re-wrapped a line of engine/crates/legion-audit/src/source_cache.rs. That half of
// canonical regeneration is the existing class A `rustfmt:<edition>` scan of the gate (deterministic-scans.mjs), which the pre-push
// hook runs right after `rightkit-gate check`; its generator half (`legion-dev generate-*`) is class B. Replayed through the real gate.
const RUSTFMT = findRustfmt();
const LEGION_CARGO = {
  "engine/Cargo.toml": '[workspace]\nmembers = ["crates/legion-audit"]\nresolver = "2"\n\n[workspace.package]\nedition = "2021"\nlicense = "MIT"\n',
  "engine/crates/legion-audit/Cargo.toml": '[package]\nname = "legion-audit"\nversion = "0.1.0"\nedition.workspace = true\nlicense.workspace = true\n\n[lib]\npath = "src/source_cache.rs"\n',
};
function gate(dir) {
  const head = sh(dir, ["rev-parse", "HEAD"]);
  const e = env();
  delete e.CI; delete e.GITHUB_ACTIONS; delete e.RIGHTKIT_GATE_NO_DEFER;
  const r = spawnSync(process.execPath, [GATE, "--base", "base", "--no-cache"], { cwd: dir, encoding: "utf8", env: e, input: `refs/heads/legion-fixture ${head} refs/heads/legion-fixture ${"0".repeat(40)}\n` });
  return { ...r, text: `${r.stdout}${r.stderr}` };
}
function rustRepo(t, source, tag) {
  const dir = repoWith(t, { ...LEGION_CARGO, "engine/crates/legion-audit/src/source_cache.rs": "pub fn placeholder() {}\n" }, tag);
  sh(dir, ["branch", "base"]);
  commitChange(dir, { write: { "engine/crates/legion-audit/src/source_cache.rs": source } });
  return dir;
}
const ORIGINAL = fixture("source_cache-7d9a8a35.rs.txt");
const ONE_LINE = "                return Ok(if cfg!(windows) { root.clone() } else { resolved });\n";
const REWRAPPED = "                return Ok(if cfg!(windows) {\n                    root.clone()\n                } else {\n                    resolved\n                });\n";

test("canonical regeneration original negative: 7d9a8a35's unformatted line fails the gate's rustfmt scan before any compile", { skip: RUSTFMT ? false : "no rustfmt on this host" }, (t) => {
  assert.ok(ORIGINAL.includes(ONE_LINE));
  const r = gate(rustRepo(t, ORIGINAL, "fmt-red"));
  assert.notEqual(r.status, 0, r.text);
  assert.match(r.text, /\] fail +rustfmt:2021 /);
});

test("canonical regeneration repair: the patch Legion's regeneration printed passes rustfmt", { skip: RUSTFMT ? false : "no rustfmt on this host" }, (t) => {
  const r = gate(rustRepo(t, ORIGINAL.replace(ONE_LINE, REWRAPPED), "fmt-green"));
  assert.match(r.text, /\] pass +rustfmt:2021 /, r.text);
  assert.doesNotMatch(r.text, /fail +rustfmt/);
});

test("canonical regeneration positive control: generated Rust (`@generated`) keeps its own formatting", { skip: RUSTFMT ? false : "no rustfmt on this host" }, (t) => {
  const r = gate(rustRepo(t, `// @generated by a code generator\n${ORIGINAL}`, "fmt-pos"));
  assert.doesNotMatch(r.text, /rustfmt:2021/, r.text);
});
