import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { checkApp, checkApps, compareVersions, loadData, lowerBound, main, renderText } from "./ownership-mandatory-set.mjs";

const data = loadData();
const tmp = () => mkdtempSync(path.join(tmpdir(), "rk-mandatory-"));
const put = (root, rel, content) => { mkdirSync(path.dirname(path.join(root, rel)), { recursive: true }); writeFileSync(path.join(root, rel), typeof content === "string" ? content : JSON.stringify(content, null, 2)); };
const owners = (rows = data.rows) => [...new Map(rows.flatMap((r) => r.owners).map((o) => [`${o.ecosystem}:${o.name}`, o])).values()];
const minOf = (name) => owners().find((o) => o.name === name).min;

/** Known-good fixture: every desktop owner declared from the registry at exactly the required version. */
function goodApp(root, { drop = [], version = {}, extraCargo = "", extraPackage = {}, devPackage = {} } = {}) {
  const cargo = owners().filter((o) => o.ecosystem === "cargo" && !drop.includes(o.name)).map((o) => `${o.name} = "=${version[o.name] ?? o.min}"`).join("\n");
  const dependencies = Object.fromEntries(owners().filter((o) => o.ecosystem === "npm" && !drop.includes(o.name)).map((o) => [o.name, version[o.name] ?? o.min]));
  put(root, "src-tauri/Cargo.toml", `[package]\nname = "fixture"\nversion = "0.0.0"\n\n[dependencies]\n${cargo}\n${extraCargo}\n`);
  put(root, "package.json", { name: "fixture", dependencies: { ...dependencies, ...extraPackage }, devDependencies: devPackage });
}
const byOwner = (report, name) => report.owners.find((o) => o.owner === name);

test("version helpers read lower bounds", () => {
  assert.deepEqual(lowerBound("=0.3.0"), { major: 0, minor: 3, patch: 0, pre: null });
  assert.equal(lowerBound("^1.2").minor, 2);
  assert.equal(lowerBound("*"), null);
  assert.ok(compareVersions(lowerBound("0.2.153"), lowerBound("0.2.9")) > 0);
  assert.ok(compareVersions(lowerBound("1.0.0-rc.1"), lowerBound("1.0.0")) < 0);
});

test("data file: rows M1-M23 are covered, M21/M23 are owner-pending, Apple ships no rows", () => {
  const ids = [...data.rows, ...data.pendingRows].map((r) => r.id).sort((a, b) => Number(a.slice(1)) - Number(b.slice(1)));
  assert.deepEqual(ids, Array.from({ length: 23 }, (_, i) => `M${i + 1}`));
  assert.deepEqual(data.pendingRows.map((r) => r.id), ["M21", "M23"]);
  assert.ok(data.pendingRows.every((r) => r.owners.length === 0));
  assert.equal(data.rows.filter((r) => r.profile === "apple").length, 0);
  assert.equal(data.profiles.apple.ratified, false);
  assert.ok(!owners().some((o) => o.name === "@rightkit/tauri-shell"), "amendment owner stays behind its flag");
});

test("known-good: all owners at required version pass, pending rows never fail", () => {
  const root = tmp();
  try {
    goodApp(root);
    const report = checkApp(root, { data });
    assert.equal(report.violations, 0, JSON.stringify(report.owners.filter((o) => o.status !== "ok")));
    assert.deepEqual(report.pending.map((p) => p.row), ["M21", "M23"]);
    assert.equal(checkApps([root], { enforce: true }).failed, false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("known-bad: missing, below-required, path/patch overrides and dev-only are each reported", () => {
  const root = tmp();
  try {
    goodApp(root, {
      drop: ["rightkit-crash", "@rightkit/legal"],
      version: { "rightkit-db": "0.3.2", "@rightkit/app-update": "0.2.4" },
      extraCargo: "[patch.crates-io]\nrightkit-fs = { path = \"../rk-snapshot/crates/rightkit-fs\" }\n",
      extraPackage: { "@rightkit/theme": "file:../rk-snapshot/theme" },
    });
    put(root, "tools/Cargo.toml", "[package]\nname=\"t\"\nversion=\"0.0.0\"\n[dev-dependencies]\nrightkit-crash = \"0.2.1\"\n[dependencies]\nrightkit-http = { path = \"../rk-snapshot/crates/rightkit-http\" }\n");
    const report = checkApp(root, { data });
    assert.equal(byOwner(report, "rightkit-crash").status, "missing");
    assert.match(byOwner(report, "rightkit-crash").evidence[0], /dev-dependencies/);
    assert.equal(byOwner(report, "@rightkit/legal").status, "missing");
    assert.equal(byOwner(report, "rightkit-db").status, "below-required");
    assert.equal(byOwner(report, "@rightkit/app-update").status, "below-required");
    assert.equal(byOwner(report, "rightkit-fs").status, "not-on-registry");
    assert.match(byOwner(report, "rightkit-fs").evidence[0], /patch\.crates-io/);
    assert.equal(byOwner(report, "@rightkit/theme").status, "not-on-registry");
    assert.equal(byOwner(report, "rightkit-http").status, "not-on-registry");
    assert.equal(report.violations, 7);
    assert.equal(checkApps([root], { enforce: true }).failed, true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("QA/release owners may be dev-dependencies; other owners may not", () => {
  const root = tmp();
  try {
    goodApp(root, { drop: ["@rightkit/release", "rightkit-http"], devPackage: { "@rightkit/release": minOf("@rightkit/release") } });
    put(root, "dev/Cargo.toml", "[package]\nname=\"d\"\nversion=\"0.0.0\"\n[dev-dependencies]\nrightkit-http = \"0.3.0\"\n");
    const report = checkApp(root, { data });
    assert.equal(byOwner(report, "@rightkit/release").status, "ok");
    assert.equal(byOwner(report, "rightkit-http").status, "missing");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("workspace.dependencies resolve for members; rk-snapshot is not scanned", () => {
  const root = tmp();
  try {
    goodApp(root, { drop: ["rightkit-fs"] });
    put(root, "Cargo.toml", `[workspace]\nmembers=["crates/a"]\n[workspace.dependencies]\nrightkit-fs = "=${minOf("rightkit-fs")}"\n`);
    put(root, "crates/a/Cargo.toml", "[package]\nname=\"a\"\nversion=\"0.0.0\"\n[dependencies]\nrightkit-fs = { workspace = true }\n");
    put(root, "rk-snapshot/crates/rightkit-crash/Cargo.toml", "[package]\nname=\"rightkit-crash\"\nversion=\"0.2.1\"\n[dependencies]\nrightkit-fs=\"0.0.1\"\n");
    assert.equal(byOwner(checkApp(root, { data }), "rightkit-fs").status, "ok");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("app-side N/A needs a reason", () => {
  const root = tmp();
  try {
    goodApp(root, { drop: ["rightkit-crash"] });
    put(root, "rightkit-mandatory.json", { notApplicable: { M16: "CLI-only helper app has no crash-reporting process boundary", M18: "n/a" } });
    const report = checkApp(root, { data });
    assert.equal(byOwner(report, "rightkit-crash"), undefined);
    assert.deepEqual(report.notApplicable.map((n) => n.row), ["M16"]);
    assert.ok(report.problems.some((p) => p.startsWith("M18")));
    assert.equal(byOwner(report, "@rightkit/app-update").status, "ok");
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("amendment v2.1 adds @rightkit/tauri-shell to M1 only when requested; an unratified or unknown profile fails", () => {
  const root = tmp();
  try {
    goodApp(root);
    const amended = checkApp(root, { data, amendments: ["v2.1"] });
    assert.equal(amended.violations, 1);
    assert.equal(byOwner(amended, "@rightkit/tauri-shell").status, "missing");
    assert.equal(checkApp(root, { data }).violations, 0);
    const apple = checkApp(root, { data, profile: "apple" });
    assert.deepEqual([apple.owners.length, apple.violations], [1, 1]);
    assert.equal(apple.owners[0].status, "unratified");
    const unknown = checkApp(root, { data, profile: "nope" });
    assert.equal(unknown.violations, 1);
    assert.equal(unknown.owners[0].status, "unknown-profile");
    put(root, "rightkit-mandatory.json", { profile: "apple" });
    const selected = checkApps([root], { data, enforce: true });
    assert.equal(selected.failed, true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("CLI: report mode exits 0 on a bad app, --enforce exits 1, missing app root is unscanned", async () => {
  const workspace = tmp();
  try {
    goodApp(path.join(workspace, "good"));
    goodApp(path.join(workspace, "bad"), { drop: ["rightkit-agent-tools"] });
    const run = async (args) => { let out = ""; const code = await main(["--workspace", workspace, ...args], { stdout: { write: (s) => { out += s; } }, stderr: { write() {} } }); return { code, out }; };
    assert.equal((await run(["--app", "good", "--app", "bad"])).code, 0);
    assert.equal((await run(["--app", "good", "--enforce"])).code, 0);
    const bad = await run(["--app", "good", "--app", "bad", "--enforce"]);
    assert.equal(bad.code, 1);
    assert.match(bad.out, /missing\s+rightkit-agent-tools/);
    assert.equal((await run(["--app", "nope", "--enforce"])).code, 1);
    assert.equal(JSON.parse((await run(["--app", "bad", "--json"])).out).apps[0].violations, 1);
    assert.match(renderText(checkApps([path.join(workspace, "bad")], {})), /1\/1 apps fail/);
  } finally { rmSync(workspace, { recursive: true, force: true }); }
});

test("CLI: an unknown --amendment exits 2 like an unknown --profile, even with --enforce", async () => {
  const workspace = tmp();
  try {
    goodApp(path.join(workspace, "good"));
    const errors = [];
    const run = (args) => main(["--workspace", workspace, ...args], { stdout: { write() {} }, stderr: { write: (s) => { errors.push(s); } } });
    assert.equal(await run(["--app", "good", "--amendment", "v2.2", "--enforce"]), 2);
    assert.match(errors.join(""), /unknown amendment "v2.2"/);
    assert.equal(await run(["--app", "good", "--amendment", "2.2"]), 2);
    assert.equal(await run(["--app", "good", "--profile", "nope", "--enforce"]), 2);
    assert.equal(await run(["--app", "good", "--amendment", "v2.1"]), 0);
  } finally { rmSync(workspace, { recursive: true, force: true }); }
});

test("lowerBound proven-minimum range semantics (M23): upper-only proves nothing, AND takes max, union takes min", () => {
  assert.equal(lowerBound("<9"), null);
  assert.equal(lowerBound("<9.0.0"), null);
  assert.equal(lowerBound("<=9.0.0"), null);
  assert.equal(lowerBound("<9 <10"), null);
  assert.deepEqual(lowerBound(">=0.2.5 <0.4"), { major: 0, minor: 2, patch: 5, pre: null });
  assert.deepEqual(lowerBound(">=0.2.5, <0.4"), { major: 0, minor: 2, patch: 5, pre: null });
  assert.deepEqual(lowerBound(">=1.0.0 >=2.0.0 <3"), { major: 2, minor: 0, patch: 0, pre: null });
  assert.deepEqual(lowerBound(">1.2.3"), { major: 1, minor: 2, patch: 3, pre: null });
  assert.deepEqual(lowerBound("~1.2.3"), { major: 1, minor: 2, patch: 3, pre: null });
  assert.deepEqual(lowerBound("1.x"), { major: 1, minor: 0, patch: 0, pre: null });
  assert.deepEqual(lowerBound("1.2.*"), { major: 1, minor: 2, patch: 0, pre: null });
  assert.deepEqual(lowerBound("1.2.3 - 2.0.0"), { major: 1, minor: 2, patch: 3, pre: null });
  assert.deepEqual(lowerBound(">=3.0.0 || >=1.0.0 <2.0.0"), { major: 1, minor: 0, patch: 0, pre: null });
  assert.equal(lowerBound("<9 || >=10"), null);
  assert.equal(lowerBound("* || >=1.0.0"), null);
  assert.equal(lowerBound("latest"), null);
  assert.equal(lowerBound(""), null);
  assert.equal(lowerBound(null), null);
});

test("judge: declared <9 against required min 9 is below-required (not provable), >=9 <10 passes (M23)", () => {
  const data9 = {
    defaultProfile: "desktop",
    appDeclarationFile: "rightkit-mandatory.json",
    profiles: { desktop: {} },
    rows: [{ id: "M18", name: "fixture row", profile: "desktop", owners: [{ name: "@rightkit/app-update", ecosystem: "npm", min: "9" }] }],
    pendingRows: [],
    amendments: {},
  };
  const badRoot = tmp();
  const okRoot = tmp();
  try {
    put(badRoot, "package.json", { name: "fixture", dependencies: { "@rightkit/app-update": "<9" } });
    const bad = byOwner(checkApp(badRoot, { data: data9 }), "@rightkit/app-update");
    assert.equal(bad.status, "below-required");
    assert.equal(bad.declared, null);
    assert.match(bad.evidence.join("\n"), /not provable/);
    put(okRoot, "package.json", { name: "fixture", dependencies: { "@rightkit/app-update": ">=9 <10" } });
    const ok = byOwner(checkApp(okRoot, { data: data9 }), "@rightkit/app-update");
    assert.equal(ok.status, "ok");
    assert.equal(ok.declared, "9.0.0");
  } finally {
    rmSync(badRoot, { recursive: true, force: true });
    rmSync(okRoot, { recursive: true, force: true });
  }
});


test("SemVer prerelease identifiers use numeric precedence & stable releases win", () => {
  for (const [low, high] of [["1.0.0-beta.2", "1.0.0-beta.10"], ["1.0.0-2", "1.0.0-10"], ["1.0.0-2", "1.0.0-alpha"], ["1.0.0-beta", "1.0.0-beta.1"], ["1.0.0-rc.1", "1.0.0"]]) {
    assert.ok(compareVersions(lowerBound(low), lowerBound(high)) < 0, `${low} < ${high}`);
  }
  assert.equal(lowerBound("1.0.0-beta.01"), null);
});

test("nonapplicable target & optional/peer declarations never claim active production adoption", () => {
  const root = tmp();
  try {
    const fixtureData = { ...data, rows: [{ id: "M1", profile: "desktop", owners: [{ name: "rightkit-fs", ecosystem: "cargo", min: "1.0.0" }, { name: "@rightkit/legal", ecosystem: "npm", min: "1.0.0" }] }], pendingRows: [] };
    put(root, "Cargo.toml", '[package]\nname="fixture"\nversion="0.0.0"\n[target.\'cfg(target_os = "none")\'.dependencies]\nrightkit-fs = { version="1.0.0", optional=true }');
    put(root, "package.json", { peerDependencies: { "@rightkit/legal": "1.0.0" } });
    const report = checkApp(root, { data: fixtureData });
    assert.equal(report.violations, 0); // Existing declaration enforcement contract remains intact.
    for (const owner of report.owners) {
      assert.equal(owner.declarationStatus, "ok");
      assert.equal(owner.evidenceKind, "declaration");
      assert.equal(owner.qualification, "unverified");
      assert.equal(owner.activeTarget, null);
      assert.equal(owner.lockResolution, null);
      assert.equal(owner.productionUse, null);
    }
    assert.match(byOwner(report, "rightkit-fs").declarations[0].target, /none/);
    assert.equal(byOwner(report, "@rightkit/legal").declarations[0].peer, true);
  } finally { rmSync(root, { recursive: true, force: true }); }
});


test("mandatory prerelease minimum cannot be qualified by beta.2 against beta.10", () => {
  const root = tmp();
  try {
    const fixtureData = { ...data, rows: [{ id: "M1", profile: "desktop", owners: [{ name: "rightkit-fs", ecosystem: "cargo", min: "1.0.0-beta.10" }] }], pendingRows: [] };
    put(root, "Cargo.toml", '[dependencies]\nrightkit-fs="^1.0.0-beta.2"');
    assert.equal(byOwner(checkApp(root, { data: fixtureData }), "rightkit-fs").status, "below-required");
    put(root, "Cargo.toml", '[dependencies]\nrightkit-fs="^1.0.0-beta.10"');
    assert.equal(byOwner(checkApp(root, { data: fixtureData }), "rightkit-fs").status, "ok");
  } finally { rmSync(root, { recursive: true, force: true }); }
});
