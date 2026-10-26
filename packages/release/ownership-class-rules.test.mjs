import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { RULES, runRules } from "./app-check.mjs";
import { isRealDate, mandatoryFoundation, OWNERSHIP_CLASS_RULES, ownershipExceptions, ratifiedNotApplicable, releaseAppMarkers, rightkitIssue, rulingFor, validateLedger, validateLedgerEntry } from "./ownership-class-rules.mjs";
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

// #1197 weakness B (rightkit#1140): any not-applicable reason of 10+ characters used to excuse its row. Only a ruling ratified in
// appAmendments.<app>, for the app the declaration names and its profile, with the reason verbatim, excuses a row now.
const EMBER_M9 = data.appAmendments.ember.notApplicable.M9;
test("mandatory-foundation: original negative: an unratified not-applicable reason no longer excuses its row", (t) => {
  // ViewRight's shape (rightkit#1140): a plausible reason, no ruling for the app.
  const declaration = { profile: "desktop", notApplicable: { M9: "No SQLite app state: preferences are JSON through rightkit-app-settings." } };
  const found = run("mandatory-foundation", t, app({ drop: ["rightkit-db"], declaration }));
  assert.deepEqual(found.map((f) => f.subject).sort(), ["notApplicable:M9", "rightkit-db"]);
  assert.match(found.find((f) => f.subject === "notApplicable:M9").problem, /is not a ratified ruling: rightkit-mandatory\.json names no "app"; the row still applies/);
});

test("mandatory-foundation: borrowing another app's ruling, or rewording a ruling, does not excuse the row", (t) => {
  const borrowed = run("mandatory-foundation", t, app({ drop: ["rightkit-db"], declaration: { app: "viewright", profile: "desktop", notApplicable: { M9: EMBER_M9 } } }));
  assert.deepEqual(borrowed.map((f) => f.subject).sort(), ["notApplicable:M9", "rightkit-db"]);
  assert.match(borrowed.find((f) => f.subject === "notApplicable:M9").problem, /appAmendments has no entry for "viewright"/);
  const reworded = run("mandatory-foundation", t, app({ drop: ["rightkit-db"], declaration: { app: "ember", profile: "desktop", notApplicable: { M9: `${EMBER_M9} Also no cache.` } } }));
  assert.match(reworded.find((f) => f.subject === "notApplicable:M9").problem, /has no ruling for M9 with this reason verbatim/);
  const unruledRow = run("mandatory-foundation", t, app({ drop: ["rightkit-http"], declaration: { app: "ember", profile: "desktop", notApplicable: { M17: "Ember downloads nothing at all, ever." } } }));
  assert.deepEqual(unruledRow.map((f) => f.subject).sort(), ["notApplicable:M17", "rightkit-http"]);
});

test("mandatory-foundation: repaired and positive control: a ratified ruling, named by its app and copied verbatim, excuses its row", (t) => {
  assert.deepEqual(run("mandatory-foundation", t, app({ drop: ["rightkit-db"], declaration: { app: "ember", profile: "desktop", notApplicable: { M9: EMBER_M9 } } })), []);
  // Ember's committed declaration (Orthic-Labs/ember rightkit-mandatory.json): eight rulings, each verbatim from appAmendments.ember.
  const rows = ["M3", "M4", "M6", "M7", "M9", "M12", "M13", "M22"];
  const split = ratifiedNotApplicable(Object.fromEntries(rows.map((r) => [r, data.appAmendments.ember.notApplicable[r]])), { app: "ember", profile: "desktop", data });
  assert.deepEqual([Object.keys(split.ratified), Object.keys(split.unratified)], [rows, []]);
});

// #1197 weakness A: only a root rightkit-mandatory.json made a tree an app, so an app that never committed one was never checked.
test("mandatory-foundation: original negative: a desktop app without a declaration fails; the missing declaration is never an exemption", (t) => {
  const root = run("mandatory-foundation", t, { ...app({ drop: ["rightkit-http"], declaration: null }), "right-release.config.json": { app: "x" } });
  assert.deepEqual(root.map((f) => f.identity), ["mandatory-foundation|rightkit-mandatory.json|declaration"]);
  assert.match(root[0].problem, /^right-release\.config\.json ships a desktop app, and the repository has no rightkit-mandatory\.json: an app without its declaration is never exempt/);
  // CutRight / HeardRight shape: the app lives in a subdirectory beside its src-tauri.
  const nested = run("mandatory-foundation", t, { ...app({ declaration: null }), "apps/cut/right-release.config.mjs": "export default {};\n", "apps/cut/src-tauri/tauri.conf.json": { identifier: "x" } });
  assert.deepEqual(nested.map((f) => f.subject), ["declaration"]);
  assert.match(nested[0].problem, /^apps\/cut ships a desktop app/);
  // Without the evaluator the missing declaration still fails: it needs no TOML parser.
  const { root: dir, files } = tree(t, { "right-release.config.mjs": "export default {};\n" });
  assert.deepEqual(mandatoryFoundation({ root: dir, files }, { evaluator: null }).map((f) => f.subject), ["declaration"]);
});

test("mandatory-foundation: repaired: the same app with its declaration and every owner passes, at the root or in its own directory", (t) => {
  assert.deepEqual(run("mandatory-foundation", t, { ...app({ declaration: { app: "x", profile: "desktop" } }), "right-release.config.json": { app: "x" } }), []);
  const nested = Object.fromEntries(Object.entries(app({ declaration: { profile: "desktop" } })).map(([file, body]) => [`apps/cut/${file}`, body]));
  assert.deepEqual(run("mandatory-foundation", t, { ...nested, "apps/cut/right-release.config.mjs": "export default {};\n", "apps/cut/src-tauri/tauri.conf.json": {} }), []);
  const nestedMissing = Object.fromEntries(Object.entries(app({ drop: ["rightkit-http"], declaration: { profile: "desktop" } })).map(([file, body]) => [`apps/cut/${file}`, body]));
  // From the outer root a nested app's owners are not evaluated (that needs the TOML parser; its own root's check does it), but its
  // rulings are, with no parser at all.
  assert.deepEqual(run("mandatory-foundation", t, { ...nestedMissing, "apps/cut/right-release.config.mjs": "", "apps/cut/src-tauri/tauri.conf.json": {} }), []);
  const { root: outer, files: outerFiles } = tree(t, { "apps/cut/rightkit-mandatory.json": { profile: "desktop", notApplicable: { M9: "No SQLite app state in this app at all." } }, "apps/cut/right-release.config.mjs": "", "apps/cut/src-tauri/tauri.conf.json": {} });
  assert.deepEqual(mandatoryFoundation({ root: outer, files: outerFiles }, { evaluator: null }).map((f) => f.subject), ["notApplicable:M9"]);
  assert.deepEqual(run("mandatory-foundation", t, nestedMissing.constructor === Object ? Object.fromEntries(Object.entries(nestedMissing).map(([f, b]) => [f.slice("apps/cut/".length), b])) : {}).map((f) => f.subject), ["rightkit-http"], "checked as its own root, the owners are evaluated");
});

test("mandatory-foundation: positive control: a library, a fixture Tauri app and a nested release config without src-tauri are not apps", (t) => {
  assert.deepEqual(run("mandatory-foundation", t, { "Cargo.toml": "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n", "tests/fixtures/app/src-tauri/tauri.conf.json": {}, "tools/right-release.config.json": {}, "node_modules/x/right-release.config.json": {} }), []);
  assert.deepEqual(releaseAppMarkers(["right-release.config.mjs", "a/right-release.config.json", "a/src-tauri/tauri.conf.json", "b/right-release.config.json", "vendor/c/right-release.config.json", "vendor/c/src-tauri/tauri.conf.json"]), ["a/right-release.config.json", "right-release.config.mjs"]);
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

// Rulings that cover every fixture entry, so the schema tests see only schema findings.
const RULED = { appAmendments: { app: { profile: "desktop", exceptions: [{ capability: "app-shell-ui", package: "@rightkit/react-shell", issue: "bogusyogi/rightkit#1107", expires: "2026-12-31" }] } } };
test("ownership-exceptions: every committed ledger is checked; schema findings ratchet, an expired entry never does", (t) => {
  const run = (_rule, t2, files) => runRules({ ...tree(t2, { ...files, "rightkit-mandatory.json": { app: "app", profile: "desktop" } }), only: ["ownership-exceptions"], now: NOW, rules: [{ id: "ownership-exceptions", gap: "OW-06", run: (ctx) => ownershipExceptions(ctx, { data: RULED }) }] }).failures;
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

// #1197 weakness B, the ledger half: approvedBy is text the app writes, so an entry was "approved" by its own say-so.
test("ownership-exceptions: original negative: a complete, approved-looking entry that no ratified ruling covers fails", (t) => {
  // Ember's ledger shape (Orthic-Labs/ember .rightkit/ownership-exceptions.json), with approvedBy filled in.
  const found = run("ownership-exceptions", t, { "rightkit-mandatory.json": { app: "ember", profile: "desktop" }, ".rightkit/ownership-exceptions.json": { exceptions: [entry()] } });
  assert.equal(found.length, 1);
  assert.equal(found[0].identity, "ownership-exceptions|.rightkit/ownership-exceptions.json|#0 app-shell-ui/@rightkit/react-shell src/Dock.tsx ruling");
  assert.match(found[0].problem, /matches no ratified ruling: appAmendments\.ember\.exceptions in ownership-mandatory-set\.json has none/);
  const anonymous = run("ownership-exceptions", t, { ".rightkit/ownership-exceptions.json": { exceptions: [entry()] } });
  assert.match(anonymous[0].problem, /rightkit-mandatory\.json names no "app", so no ruling can apply/);
});

test("ownership-exceptions: repaired and positive control: a ruling for the app covers its entry; a wider entry is not covered", (t) => {
  const ruled = (files) => runRules({ ...tree(t, { ...files, "rightkit-mandatory.json": { app: "app", profile: "desktop" } }), only: ["ownership-exceptions"], now: NOW, rules: [{ id: "ownership-exceptions", gap: "OW-06", run: (ctx) => ownershipExceptions(ctx, { data: RULED }) }] }).failures;
  assert.deepEqual(ruled({ ".rightkit/ownership-exceptions.json": { exceptions: [entry()] } }), []);
  const rulings = RULED.appAmendments.app.exceptions;
  assert.ok(rulingFor(entry({ issue: "https://github.com/BOGUSYOGI/rightkit/issues/1107" }), rulings), "the issue may be written either way");
  assert.equal(rulingFor(entry({ expires: "2027-01-31" }), rulings), null, "an entry may not outlive its ruling");
  assert.equal(rulingFor(entry({ issue: "bogusyogi/rightkit#1108" }), rulings), null, "another issue is another ruling");
  assert.equal(rulingFor(entry({ package: "@rightkit/settings-ui" }), rulings), null, "another package is not covered");
  assert.equal(rulingFor(entry(), [{ ...rulings[0], path: "src/Other.tsx" }]), null, "a ruling that names a path covers that path only");
  assert.equal(ruled({ ".rightkit/ownership-exceptions.json": { exceptions: [entry({ capability: "settings" })] } }).length, 1);
});

test("ownership-exceptions: an unparseable ledger fails; an empty ledger and a tree without one pass", (t) => {
  assert.deepEqual(run("ownership-exceptions", t, { ".rightkit/ownership-exceptions.json": "{ nope" }).map((f) => f.subject), ["document"]);
  assert.deepEqual(run("ownership-exceptions", t, { ".rightkit/ownership-exceptions.json": { exceptions: [] } }), []);
  assert.deepEqual(run("ownership-exceptions", t, { "README.md": "x" }), []);
});

test("the desktop template passes both rules as committed, except its unratified M22 row", (t) => {
  const template = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "desktop-app-template");
  if (!fs.existsSync(template)) return t.skip("no RightKit checkout beside the package");
  const files = [".rightkit/ownership-exceptions.json", "rightkit-mandatory.json"];
  const result = runRules({ root: template, files, only: ["mandatory-foundation", "ownership-exceptions"], now: NOW });
  // The template's M22 not-applicable row is not ratified yet (rightkit#994), so M22 applies: two findings when checked as its own root.
  assert.deepEqual(result.failures.map((f) => f.identity), ["mandatory-foundation|rightkit-mandatory.json|notApplicable:M22", "mandatory-foundation|rightkit-mandatory.json|rightkit-agent-tools"]);
});
