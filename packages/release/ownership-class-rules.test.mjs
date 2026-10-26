import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RULES, runRules } from "./app-check.mjs";
import { isRealDate, mandatoryFoundation, OWNERSHIP_CLASS_RULES, rightkitIssue, validateLedger, validateLedgerEntry } from "./ownership-class-rules.mjs";
import { loadData } from "./ownership-mandatory-set.mjs";

// Plan items 0.5 (mandatory-set enforcement) and 0.6 (the ownership exception ledger), run as app-check rules.

const data = loadData();
const NOW = new Date("2026-10-26T12:00:00Z");
const tree = (t, files) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-class-test-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  for (const [file, body] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    fs.writeFileSync(path.join(root, file), typeof body === "string" ? body : JSON.stringify(body, null, 2));
  }
  return { root, files: Object.keys(files) };
};
const run = (only, t, files) => runRules({ ...tree(t, files), only: [only], now: NOW }).failures;

// Every desktop foundation owner, at its required version, except `drop`.
const owners = () => [...new Map(data.rows.filter((r) => r.profile === "desktop").flatMap((r) => r.owners).map((o) => [`${o.ecosystem}:${o.name}`, o])).values()];
function app({ drop = [], dev = [], declaration = { profile: "desktop" } } = {}) {
  const cargo = owners().filter((o) => o.ecosystem === "cargo" && !drop.includes(o.name));
  const npm = owners().filter((o) => o.ecosystem === "npm" && !drop.includes(o.name));
  const files = {
    "src-tauri/Cargo.toml": `[package]\nname = "app"\nversion = "0.1.0"\n\n[dependencies]\n${cargo.filter((o) => !dev.includes(o.name)).map((o) => `${o.name} = "${o.min}"`).join("\n")}\n\n[dev-dependencies]\n${cargo.filter((o) => dev.includes(o.name)).map((o) => `${o.name} = "${o.min}"`).join("\n")}\n`,
    "package.json": { name: "app", dependencies: Object.fromEntries(npm.map((o) => [o.name, o.min])) },
  };
  if (declaration !== null) files["rightkit-mandatory.json"] = declaration;
  return files;
}

const entry = (over = {}) => ({
  capability: "app-shell-ui", package: "@rightkit/react-shell", path: "src/Dock.tsx", reason: "no controlled resize handle yet", owner: "app",
  approvedBy: "Adrian 2026-10-20", date: "2026-10-20", expires: "2026-11-30", issue: "bogusyogi/rightkit#1107", ...over,
});

test("the two rules run in the app check, last, and the shared runner passes the clock to them", () => {
  assert.deepEqual(RULES.slice(-2).map((r) => [r.id, r.gap]), [["mandatory-foundation", "OW-05"], ["ownership-exceptions", "OW-06"]]);
  assert.deepEqual(OWNERSHIP_CLASS_RULES.map((r) => r.id), ["mandatory-foundation", "ownership-exceptions"]);
});

test("mandatory-foundation: a repository that is not an app is not checked", (t) => {
  assert.deepEqual(run("mandatory-foundation", t, { "Cargo.toml": "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n" }), []);
});

test("mandatory-foundation: an app that uses every foundation owner of its profile passes", (t) => {
  assert.deepEqual(run("mandatory-foundation", t, app()), []);
});

test("mandatory-foundation: a foundation owner the app does not use fails, with an identity for the app's ratchet", (t) => {
  const found = run("mandatory-foundation", t, app({ drop: ["rightkit-db", "@rightkit/logs"] }));
  assert.deepEqual(found.map((f) => f.identity).sort(), ["mandatory-foundation|rightkit-mandatory.json|@rightkit/logs", "mandatory-foundation|rightkit-mandatory.json|rightkit-db"]);
  const db = found.find((f) => f.subject === "rightkit-db");
  assert.match(db.problem, /profile "desktop" requires the foundation owner rightkit-db \(cargo, row M9\) and the app does not use it/);
  assert.match(db.fix, /notApplicable in rightkit-mandatory.json/);
});

test("mandatory-foundation: a dev-only declaration does not count as use", (t) => {
  const found = run("mandatory-foundation", t, app({ dev: ["rightkit-crash"] }));
  assert.deepEqual(found.map((f) => f.subject), ["rightkit-crash"]);
  assert.match(found[0].problem, /declared only as a dev dependency/);
});

test("mandatory-foundation: a row declared not applicable with a reason is not required", (t) => {
  const declaration = { profile: "desktop", notApplicable: { M9: "No SQLite app state: preferences are JSON through rightkit-app-settings." } };
  assert.deepEqual(run("mandatory-foundation", t, app({ drop: ["rightkit-db"], declaration })), []);
});

test("mandatory-foundation: only the declaration marks an app; a release config alone is not checked here", (t) => {
  const files = { ...app({ drop: ["rightkit-http"], declaration: null }), "right-release.config.json": { app: "x" } };
  assert.deepEqual(run("mandatory-foundation", t, files), []);
});

test("mandatory-foundation: an unratified or unknown profile fails; an unparseable declaration fails", (t) => {
  assert.deepEqual(run("mandatory-foundation", t, app({ declaration: { profile: "apple" } })).map((f) => f.subject), ["profile:apple"]);
  assert.deepEqual(run("mandatory-foundation", t, app({ declaration: { profile: "kiosk" } })).map((f) => f.subject), ["profile:kiosk"]);
  const broken = run("mandatory-foundation", t, { ...app(), "rightkit-mandatory.json": "{ not json" });
  assert.deepEqual(broken.map((f) => f.subject), ["declaration"]);
});

test("mandatory-foundation: an app whose check cannot load the evaluator fails rather than passing", (t) => {
  const { root } = tree(t, app());
  assert.throws(() => mandatoryFoundation({ root }, { evaluator: null }), /could not be loaded/);
});

test("ledger entry: the complete entry passes; each missing field is named", () => {
  assert.deepEqual(validateLedgerEntry(entry(), { today: "2026-10-26" }), { problems: [], expired: false });
  const bare = validateLedgerEntry({ capability: "x", package: "y", reason: "z", owner: "app", date: "2026-10-19" }, { today: "2026-10-26" });
  assert.deepEqual(bare.problems, ["missing approvedBy", "expires must be a real date, YYYY-MM-DD (UTC): every exception expires", "missing issue: the RightKit issue that moves this code into RightKit"]);
  assert.equal(bare.expired, false);
});

test("ledger entry: schema, dates and the RightKit issue link are validated", () => {
  const problems = (over) => validateLedgerEntry(entry(over), { today: "2026-10-26" }).problems;
  assert.match(problems({ expiry: "2026-12-01" }).join(), /unknown key\(s\) expiry/);
  assert.match(problems({ crate: "x" }).join(), /both package and crate/);
  assert.match(problems({ approvedBy: " " }).join(), /missing approvedBy/);
  assert.match(problems({ date: "2026-02-30" }).join(), /date must be a real date/);
  assert.match(problems({ date: "2026-10-27" }).join(), /later than today/);
  assert.match(problems({ expires: "2026-10-01" }).join(), /expires 2026-10-01 is before date 2026-10-20/);
  assert.match(problems({ issue: "bogusyogi/genright#292" }).join(), /not a RightKit issue/);
  assert.match(problems({ issue: "#1107" }).join(), /not a RightKit issue/);
  assert.deepEqual(problems({ issue: "https://github.com/bogusyogi/rightkit/issues/1107" }), []);
  assert.deepEqual(problems({ issue: "Orthic-Labs/rightkit#893", package: undefined, crate: "rightkit-ownership" }), []);
});

test("ledger entry: an entry past its expires date is expired; the expiry day itself is still valid", () => {
  assert.equal(validateLedgerEntry(entry({ expires: "2026-10-26" }), { today: "2026-10-26" }).expired, false);
  assert.deepEqual(validateLedgerEntry(entry({ expires: "2026-10-25" }), { today: "2026-10-26" }), { problems: [], expired: true });
});

test("ledger helpers: real dates and RightKit issue forms", () => {
  assert.equal(isRealDate("2026-02-28"), true);
  assert.equal(isRealDate("2026-02-29"), false);
  assert.equal(isRealDate("2026-1-01"), false);
  assert.equal(rightkitIssue("bogusyogi/rightkit#12"), "bogusyogi/rightkit#12");
  assert.equal(rightkitIssue("https://github.com/bogusyogi/rightkit/issues/12"), "bogusyogi/rightkit#12");
  assert.equal(rightkitIssue("https://github.com/bogusyogi/rightkit/pull/12"), null);
  assert.equal(rightkitIssue("bogusyogi/rightkit-exports#12"), null);
});

test("ledger document: top-level schema", () => {
  assert.deepEqual(validateLedger({ exceptions: [] }), []);
  assert.deepEqual(validateLedger({ schema: 1, generated: "2026-10-24", exceptions: [] }), []);
  assert.deepEqual(validateLedger({ schema: 2, exceptions: [], extra: true }).map((f) => f.problem), ["unknown top-level key(s) extra (allowed: schema, generated, exceptions)", "schema 2 is not 1"]);
  assert.deepEqual(validateLedger({}).map((f) => f.problem), ["exceptions must be an array"]);
  assert.deepEqual(validateLedger([]).map((f) => f.subject), ["document"]);
});

test("ownership-exceptions: every committed ledger is checked; schema findings ratchet, an expired entry never does", (t) => {
  const files = {
    ".rightkit/ownership-exceptions.json": { exceptions: [entry(), entry({ approvedBy: undefined, path: "src/Rail.tsx" })] },
    "apps/web/.rightkit/ownership-exceptions.json": { schema: 1, exceptions: [entry({ expires: "2026-10-01", date: "2026-09-01" })] },
    "fixtures/not-a-ledger.json": "{}",
  };
  const found = run("ownership-exceptions", t, files);
  assert.equal(found.length, 2);
  const schema = found.find((f) => f.identity);
  assert.equal(schema.identity, "ownership-exceptions|.rightkit/ownership-exceptions.json|#1 app-shell-ui/@rightkit/react-shell src/Rail.tsx");
  assert.match(schema.problem, /entry #1 .*missing approvedBy/);
  const expired = found.find((f) => !f.identity);
  assert.match(expired.problem, /^apps\/web\/\.rightkit\/ownership-exceptions\.json: entry #0 .* expired on 2026-10-01 \(today 2026-10-26, UTC\)/);
  assert.match(expired.fix, /bogusyogi\/rightkit#1107/);
});

test("ownership-exceptions: an unparseable ledger fails; an empty ledger and a tree without one pass", (t) => {
  assert.deepEqual(run("ownership-exceptions", t, { ".rightkit/ownership-exceptions.json": "{ nope" }).map((f) => f.subject), ["document"]);
  assert.deepEqual(run("ownership-exceptions", t, { ".rightkit/ownership-exceptions.json": { exceptions: [] } }), []);
  assert.deepEqual(run("ownership-exceptions", t, { "README.md": "x" }), []);
});

test("the desktop template passes both rules as committed", (t) => {
  const template = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "desktop-app-template");
  if (!fs.existsSync(template)) return t.skip("no RightKit checkout beside the package");
  const files = [".rightkit/ownership-exceptions.json", "rightkit-mandatory.json"];
  const result = runRules({ root: template, files, only: ["mandatory-foundation", "ownership-exceptions"], now: NOW });
  assert.deepEqual(result.failures, []);
});
