// node --test rightkit/scripts/generate-port-matrix.test.mjs
// Fixture RightKit root and fixture app workspaces are built in a temp dir; no real app repo is read.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { DEFAULT_ROOT } from "./generate-index.mjs";
import {
  CLASSES, DOMAINS, PORT_MATRIX_HTML, PORT_MATRIX_JSON, PORT_MATRIX_MD, PORT_MATRIX_OUTPUTS, STATES, applePinState, buildPortMatrixModel,
  checkPortMatrixConsistency, generatePortMatrix, parseAppDirArg, parsePreflightOutput, parseSwiftPackage, parseXcodegen, renderPortMatrix,
  resolveWorkspace, stampLine, summaryLines,
} from "./generate-port-matrix.mjs";

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), "generate-index.mjs");
const tmpDirs = [];
after(() => { for (const dir of tmpDirs) rmSync(dir, { recursive: true, force: true }); });

const NOW = "2026-10-24T12:00:00Z";
const write = (file, content) => {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, typeof content === "string" ? content : JSON.stringify(content, null, 2));
};
const crate = (name, version, extra = "") => `[package]\nname = "${name}"\nversion = "${version}"\n${extra}`;

// A RightKit root with seven crates, two npm packages, RightKitSwift, a mandatory set (M1 has two owners) and an ownership catalog;
// and a workspace with the apps listed in the mandatory set (one of them missing) plus a release consumer and a tool consumer.
function fixture() {
  const base = mkdtempSync(path.join(os.tmpdir(), "rk-matrix-"));
  tmpDirs.push(base);
  const root = path.join(base, "rightkit");
  const workspace = path.join(base, "apps");
  write(path.join(root, "crates/rightkit-alpha/Cargo.toml"), crate("rightkit-alpha", "0.2.0"));
  write(path.join(root, "crates/rightkit-beta/Cargo.toml"), crate("rightkit-beta", "0.3.0"));
  write(path.join(root, "crates/rightkit-gamma/Cargo.toml"), crate("rightkit-gamma", "1.0.0"));
  write(path.join(root, "crates/rightkit-delta/Cargo.toml"), crate("rightkit-delta", "0.5.0"));
  write(path.join(root, "crates/rightkit-ghost/Cargo.toml"), crate("rightkit-ghost", "0.1.0", "publish = false\n"));
  write(path.join(root, "crates/rightkit-legiononly/Cargo.toml"), crate("rightkit-legiononly", "0.1.0"));
  write(path.join(root, "crates/rightkit-unused/Cargo.toml"), crate("rightkit-unused", "0.1.0"));
  write(path.join(root, "packages/ui/package.json"), { name: "@rightkit/ui", version: "1.0.0" });
  write(path.join(root, "packages/web/package.json"), { name: "@rightkit/web", version: "0.6.0" });
  write(path.join(root, "swift/RightKitSwift/Package.swift"), [
    "// swift-tools-version: 6.1", "import PackageDescription", "let package = Package(", '    name: "RightKitSwift",', "    products: [",
    '        .library(name: "RightKitLicense", targets: ["RightKitLicense"]),', '        .library(name: "RightKitHub", targets: ["RightKitHub"]),', "    ],", "    targets: [])", ""].join("\n"));
  write(path.join(root, "swift/RightKitSwift/VERSION"), "0.9.0\n");
  write(path.join(root, "packages/release/rightkit-versions.json"), {
    schema: 2,
    npm: {},
    stagedNpm: { "@rightkit/ui": "1.0.0", "@rightkit/web": "0.6.0" },
    cargo: {},
    stagedCargo: { "rightkit-delta": "0.5.0" },
    registry: {
      cargo: { "rightkit-alpha": "0.2.0", "rightkit-beta": "0.3.0", "rightkit-gamma": "1.0.0", "rightkit-delta": "0.4.0", "rightkit-legiononly": "0.1.0", "rightkit-unused": "0.1.0" },
      npm: { "@rightkit/ui": "1.0.0", "@rightkit/web": "0.6.0" },
    },
    swift: { "rightkit-swift": { url: "https://example.invalid/old.git", version: "0.4.0", retired: "historical" } },
    stagedSwift: { "rightkit-swift": { url: "https://github.com/Orthic-Labs/rightkit", version: "0.8.0" } },
  });
  const capability = (pkg, name) => ({ package: pkg, capability: name, summary: name, detection: "declaration-only", fingerprints: [] });
  write(path.join(root, "packages/release/rightkit-ownership.json"), {
    schema: 1, version: "3.0.0", internalOwners: [],
    capabilities: [capability("rightkit-alpha", "alpha-cap"), capability("rightkit-delta", "delta-cap"), capability("@rightkit/ui", "ui-cap"), capability("rightkit-unused", "unused-cap")],
  });
  const owner = (name, ecosystem) => ({ name, ecosystem });
  write(path.join(root, "packages/release/ownership-mandatory-set.json"), {
    schema: 1, defaultProfile: "desktop", defaultApps: ["appone", "apptwo", "appthree", "appfour"], appDeclarationFile: "rightkit-mandatory.json",
    profiles: { desktop: {}, apple: { ratified: false } },
    rows: [
      { id: "M1", name: "Window host", profile: "desktop", owners: [owner("@rightkit/ui", "npm"), owner("rightkit-alpha", "cargo")] },
      { id: "M2", name: "Database", profile: "desktop", owners: [owner("rightkit-beta", "cargo")] },
      { id: "M3", name: "Engine", profile: "desktop", owners: [owner("rightkit-gamma", "cargo")] },
    ],
    pendingRows: [{ id: "M21", name: "Test mode", profile: "desktop", owners: [], pendingOwners: [owner("rightkit-delta", "cargo")] }],
    appAmendments: { apptwo: { notApplicable: { M3: "Ratified ruling: no engine child process." } } },
  });

  // appone: every pin state, an N/A ruling (declared, not ratified), exceptions in the legacy object shape.
  write(path.join(workspace, "appone/rightkit-mandatory.json"), { profile: "desktop", notApplicable: { M3: "No engine child process in this app.", M2: "short" } });
  write(path.join(workspace, "appone/Cargo.toml"), [
    '[package]\nname = "appone"\nversion = "1.0.0"', "[dependencies]", 'rightkit-alpha = "=0.2.0"', 'rightkit-beta = "0.3"', 'rightkit-delta = "=0.5.0"',
    'rightkit-ghost = { path = "../rightkit/crates/rightkit-ghost" }', ""].join("\n"));
  write(path.join(workspace, "appone/package.json"), { name: "appone", dependencies: { "@rightkit/ui": "0.9.0" } });
  write(path.join(workspace, "appone/.rightkit/ownership-exceptions.json"), {
    schema: 1, generated: "2026-10-01",
    exceptions: [
      { capability: "delta-cap", package: "rightkit-delta", path: "src/a.rs", issue: "rightkit#12", expires: "2099-01-01", owner: "appone" },
      { capability: "unused-cap", package: "rightkit-unused", path: "src/b.rs", issue: "rightkit#1", expires: "2020-01-01" },
      { package: "@rightkit/web", path: "src/c.rs", expires: "2099-12-31" },
      { fingerprint: "x.y", paths: ["a", "b"], capability: "no-such-cap", package: "rightkit-nonexistent", tracking: "cut over", date: "2026-10-01" },
      {},
      "garbage",
    ],
  });
  // apptwo: foundation row M1 with two owners where one is missing; M2 missing; M3 ruled and ratified; exceptions as a bare array.
  write(path.join(workspace, "apptwo/rightkit-mandatory.json"), { notApplicable: { M3: "Ratified ruling: no engine child process." } });
  write(path.join(workspace, "apptwo/Cargo.toml"), '[package]\nname = "apptwo"\nversion = "1.0.0"\n[dependencies]\nrightkit-alpha = "=0.2.0"\n');
  write(path.join(workspace, "apptwo/.rightkit/ownership-exceptions.json"), [{ capability: "alpha-cap", reason: "x" }, { package: "@rightkit/ui", capability: "ui-cap", issue: "" }]);
  // appfour: profile apple (unratified, no rows), Apple targets, RK-ready packages.
  write(path.join(workspace, "appfour/rightkit-mandatory.json"), { profile: "apple" });
  write(path.join(workspace, "appfour/ios/project.yml"), [
    "name: FourApp", "packages:", "  RightKitSwift:", "    url: https://github.com/Orthic-Labs/rightkit", '    exactVersion: "0.8.0"', "  Other:", "    url: https://example.invalid/other", "    from: 1.0.0",
    "targets:", "  FourApp:", "    dependencies:", "      - package: RightKitSwift", "        product: RightKitLicense", "      - package: RightKitSwift", "        product: RightKitStore",
    "      - package: Other", "        product: OtherProduct", ""].join("\n"));
  const swiftManifest = (name, packageCall, products = []) => `// swift-tools-version: 6.1\nimport PackageDescription\nlet package = Package(\n  name: "${name}",\n  dependencies: [\n    ${packageCall}\n  ],\n  targets: [.target(name: "T", dependencies: [${products.join(", ")}])]\n)\n`;
  write(path.join(workspace, "appfour/mac/Pill/Package.swift"), swiftManifest("Pill", '.package(url: "https://github.com/Orthic-Labs/rightkit", branch: "main")', ['.product(name: "RightKitAppleCredentials", package: "rightkit")']));
  write(path.join(workspace, "appfour/swift/Local/Package.swift"), swiftManifest("Local", '.package(path: "../../../rightkit/swift/RightKitSwift")', ['.product(name: "RightKitFS", package: "RightKitSwift")']));
  write(path.join(workspace, "appfour/tools/Range/Package.swift"), swiftManifest("Range", '.package(url: "https://github.com/bogusyogi/rightkit.git", from: "0.7.0")'));
  write(path.join(workspace, "appfour/tools/Old/Package.swift"), swiftManifest("Old", '.package(url: "https://github.com/Orthic-Labs/rightkit", exact: "0.7.0")'));
  write(path.join(workspace, "appfour/tools/New/Package.swift"), swiftManifest("New", '.package(url: "https://github.com/Orthic-Labs/rightkit", exact: "0.9.0")'));
  write(path.join(workspace, "appfour/ios-native/Tests/Smoke/Package.swift"), swiftManifest("Smoke", '.package(url: "https://github.com/Orthic-Labs/rightkit", exact: "0.1.0")'));
  write(path.join(workspace, "appfour/swift/NotRightKit/Package.swift"), swiftManifest("NotRightKit", '.package(path: "../../swift/RightKitHub")'));
  write(path.join(workspace, "appfour/rightkit-newthing/Cargo.toml"), crate("rightkit-newthing", "0.0.1", "publish = false\n"));
  write(path.join(workspace, "appfour/crates/rightkit-alpha/Cargo.toml"), crate("rightkit-alpha", "0.1.0"));
  write(path.join(workspace, "appfour/packages/web-ui/package.json"), { name: "@rightkit/web", version: "0.0.1", private: true });
  write(path.join(workspace, "appfour/swift/RightKitHub/Package.swift"), swiftManifest("RightKitHub", ""));
  write(path.join(workspace, "appfour/ios-native/Tests/RightKitSmoke/Package.swift"), swiftManifest("RightKitSmoke", ""));
  write(path.join(workspace, "appfour/node_modules/rightkit-x/package.json"), { name: "rightkit-x", version: "1.0.0" });
  write(path.join(workspace, "appfour/vendor/rightkit-v/Cargo.toml"), crate("rightkit-v", "1.0.0"));
  // sellright (release consumer) and legion (tool consumer).
  write(path.join(workspace, "sellright/package.json"), { name: "sellright", dependencies: { "@rightkit/web": "0.6.0" } });
  write(path.join(workspace, "legion/Cargo.toml"), '[package]\nname = "legion"\nversion = "0.1.0"\n[dependencies]\nrightkit-legiononly = "=0.1.0"\n');
  const triage = path.join(base, "rows.json");
  write(triage, [
    { app: "appone", verdict: "MOVE", package: "rightkit-alpha", path: "a" }, { app: "appone", verdict: "MOVE", package: "rightkit-alpha", path: "b" },
    { app: "apptwo", verdict: "MOVE", package: "rightkit-alpha", path: "c" }, { app: "appone", verdict: "MOVE", package: "rightkit-newcrate", path: "d" },
    { app: "appone", verdict: "KEEP", package: "rightkit-beta", path: "e" }, { app: "appone", verdict: "ADOPT", package: "rightkit-beta", path: "f" },
    { app: "appone", verdict: "MOVE" }, "not a row",
  ]);
  return { base, root, workspace, triage };
}

const modelOf = (fx, extra = {}) => buildPortMatrixModel({ root: fx.root, workspace: fx.workspace, now: NOW, triage: fx.triage, ...extra });
const cellOf = (model, pkg, app) => model.packages.find((p) => p.name === pkg).cells[app];
const appOf = (model, name) => model.apps.find((a) => a.name === name);
const stampFree = (text) => text.split("\n").filter((line) => !line.startsWith("Stamp: ") && !line.includes('id="stamp"')).join("\n");

test("every cell state appears, with its detail", () => {
  const model = modelOf(fixture());
  const seen = new Set();
  for (const pkg of model.packages) for (const cell of Object.values(pkg.cells)) seen.add(cell.state);
  assert.deepEqual([...seen].sort(), STATES.map((s) => s.state).sort(), "exactly the ten tokens, all of them used");

  assert.deepEqual(cellOf(model, "rightkit-alpha", "appone"), { state: "adopted", detail: "=0.2.0", rows: ["M1"] });
  assert.equal(cellOf(model, "rightkit-beta", "appone").state, "range", "0.3 admits 0.3.0");
  assert.equal(cellOf(model, "rightkit-beta", "appone").detail, "0.3");
  assert.deepEqual([cellOf(model, "@rightkit/ui", "appone").state, cellOf(model, "@rightkit/ui", "appone").detail], ["stale", "0.9.0\u21921.0.0"]);
  assert.equal(cellOf(model, "rightkit-delta", "appone").state, "ahead", "pin at the staged version, ahead of the published one");
  assert.ok(cellOf(model, "rightkit-delta", "appone").detail.startsWith("0.5.0 > 0.4.0"));
  assert.deepEqual([cellOf(model, "rightkit-ghost", "appone").state, cellOf(model, "rightkit-ghost", "appone").detail], ["local", "path"]);
  assert.equal(cellOf(model, "rightkit-beta", "apptwo").state, "missing", "foundation owner, no pin, no ruling");
  assert.equal(cellOf(model, "rightkit-beta", "apptwo").detail, "M2");
  assert.equal(cellOf(model, "rightkit-unused", "appone").state, "exception");
  assert.equal(cellOf(model, "rightkit-unused", "apptwo").state, "unused");
  assert.equal(cellOf(model, "rightkit-alpha", "appthree").state, "not present");
  assert.equal(cellOf(model, "rightkit-alpha", "sellright").state, "unused", "release consumers owe no foundation rows");
});

test("a foundation row with two owners is declared only when every owner is pinned, and names the missing one", () => {
  const model = modelOf(fixture());
  const two = appOf(model, "apptwo");
  const m1 = two.foundationRows.find((r) => r.id === "M1");
  assert.equal(m1.status, "open");
  assert.deepEqual(m1.missing, ["@rightkit/ui"]);
  assert.deepEqual(m1.owners.map((o) => [o.name, o.state]), [["@rightkit/ui", "missing"], ["rightkit-alpha", "adopted"]]);
  assert.match(cellOf(model, "rightkit-alpha", "apptwo").detail, /M1 incomplete: missing @rightkit\/ui/, "the pinned co-owner says what its row still misses");
  assert.equal(appOf(model, "appone").foundationRows.find((r) => r.id === "M1").status, "declared");
  assert.deepEqual(two.progress.foundation.openRows.map((r) => [r.id, r.missing]), [["M1", ["@rightkit/ui"]], ["M2", ["rightkit-beta"]]]);
});

test("exceptions: expired, without issue, legacy shape, bare array, capability-only match, unmatched", () => {
  const model = modelOf(fixture());
  const expired = cellOf(model, "rightkit-unused", "appone");
  assert.equal(expired.state, "exception");
  assert.match(expired.detail, /1 exception: rightkit#1, EXPIRED 2020-01-01/);
  assert.equal(expired.exceptions.expired, 1);
  const noIssue = cellOf(model, "@rightkit/web", "appone");
  assert.equal(noIssue.state, "exception");
  assert.match(noIssue.detail, /no issue, expires 2099-12-31/);
  assert.equal(noIssue.exceptions.withoutIssue, 1);
  const pinned = cellOf(model, "rightkit-delta", "appone");
  assert.match(pinned.detail, /1 exception: rightkit#12, expires 2099-01-01/, "a pinned cell keeps its pin state and adds the exception");
  assert.equal(pinned.state, "ahead");
  // appone: 5 object entries (the string is ignored); one expired; unmatched = the nonexistent package and the empty entry.
  assert.deepEqual(appOf(model, "appone").progress.exceptions, { total: 5, open: 4, expired: 1, withoutIssueOrExpiry: 3, unmatched: 2 });
  // apptwo: a capability-only entry reaches the capability's owner; an empty issue string counts as no issue.
  assert.match(cellOf(model, "rightkit-alpha", "apptwo").detail, /1 exception: no issue, no expiry/);
  assert.equal(cellOf(model, "@rightkit/ui", "apptwo").state, "exception");
  assert.match(cellOf(model, "@rightkit/ui", "apptwo").detail, /no issue, no expiry; required by M1/);
  assert.deepEqual(appOf(model, "apptwo").progress.exceptions, { total: 2, open: 2, expired: 0, withoutIssueOrExpiry: 2, unmatched: 0 });
});

test("N/A rulings show the row id, ratified or only declared; a ruling with a short reason is ignored", () => {
  const model = modelOf(fixture());
  assert.deepEqual([cellOf(model, "rightkit-gamma", "apptwo").state, cellOf(model, "rightkit-gamma", "apptwo").detail], ["n/a", "M3 ratified"]);
  assert.deepEqual([cellOf(model, "rightkit-gamma", "appone").state, cellOf(model, "rightkit-gamma", "appone").detail], ["n/a", "M3 declared"]);
  assert.deepEqual(appOf(model, "appone").declaration.ignored, ["M2"], "the reason 'short' is under 10 characters");
  assert.equal(appOf(model, "appone").foundationRows.find((r) => r.id === "M2").status, "declared", "so M2 still applies");
  assert.deepEqual(appOf(model, "apptwo").declaration.notApplicable.map((n) => [n.row, n.ratified]), [["M3", true]]);
  const f = appOf(model, "appone").progress.foundation;
  assert.deepEqual([f.rowsTotal, f.rowsDeclared, f.rowsRuled, f.rowsOpen], [3, 2, 1, 0]);
});

test("progress: foundation owners pinned current over required, add-ons reported apart", () => {
  const model = modelOf(fixture());
  const one = appOf(model, "appone").progress;
  // required: @rightkit/ui, rightkit-alpha, rightkit-beta (M3 is ruled); current: alpha (adopted), beta (range); ui is stale.
  assert.deepEqual([one.foundation.ownersRequired, one.foundation.ownersCurrent, one.foundation.ownersPinned, one.foundation.percent], [3, 2, 3, 67]);
  assert.deepEqual(one.addons, { total: 4, adopted: 1, current: 1, stale: 0, local: 0 }, "delta, unused, web and RightKitSwift are add-ons; the ghost is a tool; delta is ahead");
  const two = appOf(model, "apptwo").progress.foundation;
  assert.deepEqual([two.ownersRequired, two.ownersCurrent, two.percent, two.rowsDeclared, two.rowsRuled, two.rowsOpen], [3, 1, 33, 0, 1, 2]);
  const four = appOf(model, "appfour").progress.foundation;
  assert.deepEqual([four.profile, four.rowsTotal, four.ownersRequired, four.percent], ["apple", 0, 0, null], "an unratified profile owes no rows, so there is no figure");
  assert.equal(appOf(model, "appthree").progress, null);
  assert.equal(appOf(model, "sellright").progress.foundation.percent, null);
  assert.equal(appOf(model, "appone").progress.releasePin, null, "no @rightkit/release pin");
});

test("class rule: foundation owners, add-ons, and tools (private, or consumed only by non-app repos)", () => {
  const model = modelOf(fixture());
  const classOf = (name) => model.packages.find((p) => p.name === name).class;
  for (const name of ["rightkit-alpha", "rightkit-beta", "rightkit-gamma", "@rightkit/ui"]) assert.equal(classOf(name), "foundation", name);
  assert.equal(classOf("rightkit-delta"), "add-on");
  assert.equal(classOf("rightkit-unused"), "add-on", "published and unused is still an add-on");
  assert.equal(classOf("@rightkit/web"), "add-on");
  assert.equal(classOf("RightKitSwift"), "add-on");
  assert.equal(classOf("rightkit-ghost"), "tool", "private");
  assert.equal(classOf("rightkit-legiononly"), "tool", "only legion pins it");
  assert.deepEqual(model.packages.find((p) => p.name === "rightkit-alpha").mandatoryRows, ["M1"]);
  assert.deepEqual(model.packages.find((p) => p.name === "rightkit-delta").pendingRows, ["M21"], "pending rows are listed but make no foundation owner");
  assert.deepEqual(CLASSES.map((c) => c.class), ["foundation", "add-on", "tool"]);
});

test("Apple targets: tag, branch, path and range pins, products linked, test-only paths left out", () => {
  const model = modelOf(fixture());
  const targets = appOf(model, "appfour").appleTargets;
  const byFile = Object.fromEntries(targets.map((t) => [t.file, t]));
  assert.deepEqual(Object.keys(byFile).sort(), ["ios/project.yml", "mac/Pill/Package.swift", "swift/Local/Package.swift", "tools/New/Package.swift", "tools/Old/Package.swift", "tools/Range/Package.swift"]);
  const ios = byFile["ios/project.yml"];
  assert.deepEqual([ios.system, ios.pinKind, ios.pinVersion, ios.state, ios.products], ["xcodegen", "tag", "0.8.0", "adopted", ["RightKitLicense", "RightKitStore"]]);
  const pill = byFile["mac/Pill/Package.swift"];
  assert.deepEqual([pill.system, pill.pinKind, pill.pinRef, pill.state, pill.products], ["swiftpm", "branch", "main", "local", ["RightKitAppleCredentials"]]);
  const local = byFile["swift/Local/Package.swift"];
  assert.deepEqual([local.pinKind, local.state, local.products], ["path", "local", ["RightKitFS"]]);
  assert.deepEqual([byFile["tools/Range/Package.swift"].pinKind, byFile["tools/Range/Package.swift"].state], ["tag", "range"]);
  assert.equal(byFile["tools/Old/Package.swift"].state, "stale");
  assert.equal(byFile["tools/Old/Package.swift"].detail, "0.7.0\u21920.8.0");
  assert.equal(byFile["tools/New/Package.swift"].state, "ahead");
  // The RightKitSwift row: latest is the highest recorded version (the retired mirror tag is ignored), the cell takes the worst target.
  assert.equal(cellOf(model, "RightKitSwift", "appfour").state, "stale");
  assert.equal(model.packages.find((p) => p.name === "RightKitSwift").registry, "");
  assert.equal(model.packages.find((p) => p.name === "RightKitSwift").staged, "0.8.0");
  assert.deepEqual(model.packages.find((p) => p.name === "RightKitSwift").products, ["RightKitHub", "RightKitLicense"]);
});

test("Apple parsers: XcodeGen map and flow styles, Package.swift requirements, revision pins, comments", () => {
  const notch = parseXcodegen([
    "name: Notch", "packages:  # pinned", "  # RightKit's Swift package", "  rightkit:", "    url: https://github.com/bogusyogi/rightkit", '    exactVersion: "0.8.1"  # exact',
    "  Hub: {path: ../../swift/RightKitHub}", "targets:", "  T:", "    dependencies:", "      - package: rightkit", "        product: RightKitDB", "      - {package: rightkit, product: RightKitFS}"].join("\n"));
  assert.deepEqual(notch.packages.map((p) => [p.key, p.attrs.url ?? "", p.attrs.exactVersion ?? "", p.attrs.path ?? ""]), [["rightkit", "https://github.com/bogusyogi/rightkit", "0.8.1", ""], ["Hub", "", "", "../../swift/RightKitHub"]]);
  assert.deepEqual([...notch.products.get("rightkit")].sort(), ["RightKitDB", "RightKitFS"]);
  const swift = parseSwiftPackage([
    "import PackageDescription", "let package = Package(name: \"X\", dependencies: [",
    '  // .package(url: "https://github.com/Orthic-Labs/rightkit", exact: "9.9.9"),',
    '  .package(url: "https://github.com/Orthic-Labs/rightkit.git", revision: "0123456789abcdef"),',
    '  .package(url: "https://example.invalid/a", .upToNextMajor(from: "2.0.0")),',
    '  .package(url: "https://example.invalid/b", "1.0.0"..<"2.0.0"),', "], targets: [])"].join("\n"));
  assert.equal(swift.name, "X");
  assert.equal(swift.packages.length, 3, "the commented-out call is not read");
  assert.equal(swift.packages[0].identity, "rightkit");
  assert.equal(swift.packages[0].attrs.revision, "0123456789abcdef");
  assert.equal(swift.packages[1].attrs.from, "2.0.0");
  assert.equal(swift.packages[2].attrs.from, "1.0.0");
  const revision = { kind: "revision", ref: "0123456789abcdef", requirement: "revision 0123456789ab" };
  assert.deepEqual(applePinState(revision, "0.8.0"), { state: "local", detail: "revision 0123456789ab" });
  assert.equal(applePinState({ kind: "tag", version: "0.8.0", exactPin: true }, "0.8.0").state, "adopted");
  assert.equal(applePinState({ kind: "tag", version: "0.8.0", exactPin: false }, "1.2.0").state, "stale", "from 0.8.0 stops below 1.0.0");
  assert.equal(applePinState({ kind: "tag", version: "0.8.0", exactPin: true }, "").state, "ahead");
});

test("RK-ready packages: publish flag, duplicate or awaiting promotion, test-only and vendored paths left out", () => {
  const model = modelOf(fixture());
  const ready = appOf(model, "appfour").rkReady;
  assert.deepEqual(ready.map((r) => r.path), ["crates/rightkit-alpha", "packages/web-ui", "rightkit-newthing", "swift/RightKitHub"]);
  const [alpha, web, thing, hub] = ready;
  assert.deepEqual([alpha.kind, alpha.version, alpha.publish, alpha.status, alpha.statusText], ["cargo", "0.1.0", "publishable", "duplicate", "duplicate of published owner rightkit-alpha 0.2.0"]);
  assert.deepEqual([web.kind, web.publish, web.status], ["npm", "private", "duplicate"]);
  assert.deepEqual([thing.kind, thing.version, thing.publish, thing.status, thing.statusText], ["cargo", "0.0.1", "publish = false", "awaiting promotion", "awaiting promotion"]);
  assert.deepEqual([hub.kind, hub.status, hub.owner], ["swift", "duplicate", "RightKitSwift"]);
  assert.match(hub.statusText, /^duplicate of owner RightKitSwift product RightKitHub \(source 0\.9\.0, not published\)$/);
  assert.ok(!ready.some((r) => /Smoke|rightkit-x|rightkit-v/.test(r.path)), "Tests, node_modules and vendor are excluded");
  assert.deepEqual(appOf(model, "appone").rkReady, []);
});

test("to-move rows: MOVE counted per app and target package, KEEP and ADOPT left out, unknown targets need a new package", () => {
  const model = modelOf(fixture());
  assert.deepEqual(appOf(model, "appone").toMove.map((m) => [m.package, m.count, m.exists]), [["rightkit-alpha", 2, true], ["rightkit-newcrate", 1, false]]);
  assert.deepEqual(appOf(model, "apptwo").toMove.map((m) => [m.package, m.count]), [["rightkit-alpha", 1]]);
  assert.deepEqual(model.packages.find((p) => p.name === "rightkit-alpha").toMoveFrom, { appone: 2, apptwo: 1 });
  assert.deepEqual(model.packages.find((p) => p.name === "rightkit-beta").toMoveFrom, {}, "a KEEP row is not shown");
  assert.deepEqual(model.newPackagesNeeded, [{ package: "rightkit-newcrate", total: 1, byApp: { appone: 1 } }]);
  assert.deepEqual([model.triage.moveRows, model.triage.totalRows, model.triage.skipped], [4, 8, 2]);
  const md = renderPortMatrix(model)[PORT_MATRIX_MD];
  assert.match(md, /\| To move \| `rightkit-newcrate` \| - \| new \| - \| - \| 1 MOVE row; new package needed \|/);
  assert.match(md, /\| `rightkit-alpha` \| foundation \| M1 \| 0\.2\.0 \|.*\| appone 2, apptwo 1 \|/);
  assert.equal(buildPortMatrixModel({ root: fixture().root, workspace: fixture().workspace, now: NOW }).triage, null, "no --triage, no to-move rows");
});

test("a missing app directory is reported as not present and never crashes", () => {
  const model = modelOf(fixture());
  const three = appOf(model, "appthree");
  assert.deepEqual([three.present, three.head, three.progress, three.flags, three.appleTargets, three.rkReady], [false, null, null, null, [], []]);
  const out = renderPortMatrix(model);
  assert.match(out[PORT_MATRIX_MD], /\| appthree \| mandatory-set app \| - \| not present \|/);
  assert.match(out[PORT_MATRIX_MD], /apps: .*appthree not present/);
  assert.match(out[PORT_MATRIX_HTML], /not present: the app directory does not exist/);
  const nothing = buildPortMatrixModel({ root: fixture().root, workspace: path.join(os.tmpdir(), "rk-matrix-no-such-workspace") });
  assert.ok(nothing.apps.every((a) => !a.present), "no workspace at all: every app is not present");
  assert.ok(summaryLines(nothing).some((line) => /appone\s+not present/.test(line)));
});

test("workspace resolution: explicit, the default when it exists, else the parent of the RightKit root", () => {
  const fx = fixture();
  assert.equal(resolveWorkspace(fx.workspace, fx.root), fx.workspace);
  assert.equal(resolveWorkspace(null, fx.root, fx.workspace), fx.workspace, "the default exists");
  assert.equal(resolveWorkspace(null, fx.root, path.join(fx.base, "no-such-default")), fx.base, "the default is missing: the parent of the RightKit root");
  assert.equal(resolveWorkspace(undefined, path.join(fx.base, "rightkit", "."), "/Volumes/D/claude-missing"), fx.base);
  // Through the model: without a workspace the apps are the siblings of the root (appone is not one; the fixture keeps them in apps/).
  const sibling = path.join(fx.base, "appone");
  write(path.join(sibling, "Cargo.toml"), '[package]\nname = "appone"\nversion = "1.0.0"\n[dependencies]\nrightkit-alpha = "=0.2.0"\n');
  const model = buildPortMatrixModel({ root: fx.root });
  assert.equal(appOf(model, "appone").present, true);
  assert.equal(cellOf(model, "rightkit-alpha", "appone").state, "adopted");
});

test("--app-dir overrides the workspace directory of one app", () => {
  assert.deepEqual(parseAppDirArg("ember=/tmp/x/ember"), { ember: path.resolve("/tmp/x/ember") });
  assert.throws(() => parseAppDirArg("ember"), /--app-dir expects/);
  assert.throws(() => parseAppDirArg("=/x"), /--app-dir expects/);
  const fx = fixture();
  const elsewhere = path.join(fx.base, "elsewhere", "two");
  write(path.join(elsewhere, "Cargo.toml"), '[package]\nname = "two"\nversion = "1.0.0"\n[dependencies]\nrightkit-beta = "=0.3.0"\nrightkit-alpha = "=0.2.0"\n');
  const model = modelOf(fx, { appDirs: { apptwo: elsewhere, extra: path.join(fx.base, "elsewhere") } });
  assert.equal(appOf(model, "apptwo").dir, elsewhere);
  assert.equal(cellOf(model, "rightkit-beta", "apptwo").state, "adopted", "the pins come from the override directory");
  assert.equal(appOf(model, "extra").group, "other app", "an override for an unlisted app adds it");
});

test("the class and domain grouping: an unassigned package lands in Other; a domain option replaces the built-in map", () => {
  const fx = fixture();
  const model = modelOf(fx);
  assert.deepEqual(model.domains.map((d) => [d.id, d.packages]), [["apple", 1], ["other", 9]], "RightKitSwift has a built-in domain; the fixture's own packages have none");
  assert.ok(summaryLines(model).some((line) => /no domain assigned: .*rightkit-alpha/.test(line)));
  const grouped = modelOf(fx, { domains: [{ id: "core", name: "Core", packages: ["rightkit-alpha", "rightkit-beta"] }, { id: "empty", name: "Empty", packages: ["nothing"] }] });
  assert.deepEqual(grouped.domains.map((d) => [d.id, d.packages]), [["core", 2], ["other", 8]], "a domain with no package is not listed");
  assert.equal(grouped.packages.find((p) => p.name === "rightkit-alpha").domain, "core");
  assert.ok(DOMAINS.every((d) => d.id && d.name && d.packages.length), "built-in domains are all filled");
  assert.equal(new Set(DOMAINS.flatMap((d) => d.packages)).size, DOMAINS.flatMap((d) => d.packages).length, "no package is in two domains");
});

test("check counts: run with --with-check, recorded as not run otherwise, never a hard failure", () => {
  const fx = fixture();
  assert.deepEqual(appOf(modelOf(fx), "appone").progress.check, { status: "not run", reason: "--with-check not passed", fail: null, known: null, stale: null, byRule: {} });
  const ran = modelOf(fx, { withCheck: true, runCheck: (root, dir, ctx) => (ctx.app === "appone" ? { status: "ran", reason: "", fail: 2, known: 1, stale: 0, byRule: { "retired-alias": { fail: 2, known: 1 } } } : { status: "not run", reason: "stub", fail: null, known: null, stale: null, byRule: {} }) });
  assert.equal(appOf(ran, "appone").progress.check.fail, 2);
  assert.match(renderPortMatrix(ran)[PORT_MATRIX_MD], /\| appone \|.* \| FAIL 2, KNOWN 1 \|/);
  assert.match(renderPortMatrix(ran)[PORT_MATRIX_MD], /\| apptwo \|.* \| not run \(stub\) \|/);
  // The default runner needs scripts/preflight.mjs in the RightKit root; the fixture root has none.
  const real = modelOf(fx, { withCheck: true });
  assert.equal(appOf(real, "appone").progress.check.status, "not run");
  assert.equal(appOf(real, "appone").progress.check.reason, "scripts/preflight.mjs not found");
  const parsed = parsePreflightOutput("FAIL retired-alias (P11): a.md:1\n     fix: x\nFAIL retired-alias (P11): b.md:2\nKNOWN qa-platform: c\nSTALE baseline entry x\nGood line\n");
  assert.deepEqual(parsed, { fail: 2, known: 1, stale: 1, byRule: { "qa-platform": { fail: 0, known: 1 }, "retired-alias": { fail: 2, known: 0 } } });
});

test("output is byte-stable for identical input; only the stamp line carries the time", () => {
  const fx = fixture();
  const a = renderPortMatrix(modelOf(fx));
  const b = renderPortMatrix(modelOf(fx));
  assert.deepEqual(a, b, "same input and same time: identical bytes");
  const later = renderPortMatrix(modelOf(fx, { now: "2026-10-25T08:30:00Z" }));
  for (const rel of [PORT_MATRIX_MD, PORT_MATRIX_HTML]) {
    assert.notEqual(later[rel], a[rel], `${rel}: the stamp differs`);
    assert.equal(stampFree(later[rel]), stampFree(a[rel]), `${rel}: nothing else differs`);
  }
  const strip = (json) => { const doc = JSON.parse(json); delete doc.generatedAt; return JSON.stringify(doc); };
  assert.equal(strip(later[PORT_MATRIX_JSON]), strip(a[PORT_MATRIX_JSON]));
  for (const rel of [PORT_MATRIX_MD, PORT_MATRIX_HTML]) assert.equal((stampFree(a[rel]).match(/20\d\d-\d\d-\d\dT\d\d:\d\d/g) ?? []).length, 0, `${rel}: no timestamp outside the stamp line`);
  assert.match(stampLine(JSON.parse(a[PORT_MATRIX_JSON])), /^Stamp: generated 2026-10-24T12:00:00Z from RightKit not a git repo; apps: appfour not a git repo, appone not a git repo, appthree not present, apptwo not a git repo, sellright not a git repo, legion not a git repo\.$/);
});

test("the Markdown output: stamp, summary, one table per domain, Apple targets, commands, how to read", () => {
  const md = renderPortMatrix(modelOf(fixture()))[PORT_MATRIX_MD];
  assert.match(md, /^# RightKit port matrix\n\nStamp: generated 2026-10-24T12:00:00Z/);
  for (const heading of ["## Regenerate and check", "## How to read", "## Per-app summary", "## Matrix (", "## Apple targets", "## RK-ready packages and to-move rows", "### New package needed"]) {
    assert.ok(md.includes(heading), heading);
  }
  for (const state of STATES) assert.ok(md.includes(`\`${state.state}\``), `legend lists ${state.state}`);
  assert.ok(md.includes("node scripts/generate-index.mjs --check-adoption"), "drift-check command");
  assert.ok(md.includes("node scripts/generate-index.mjs --workspace"), "regenerate command");
  assert.ok(md.includes("node scripts/generate-index.mjs --check\n"), "local check command");
  assert.match(md, /\| appone \| mandatory-set app \| desktop \| 67% \(2\/3 owners current\) \| 3\/3 \(1 ruled\) \|/);
  assert.match(md, /\| apptwo \| mandatory-set app \| desktop \| 33% \(1\/3 owners current\) \|/);
  assert.match(md, /\| appfour \| mandatory-set app \| apple \| n\/a \(no foundation rows for this app\) \|/);
  assert.match(md, /- \*\*apptwo\*\*: M1 Window host \(missing @rightkit\/ui\); M2 Database \(missing rightkit-beta\)/);
  assert.match(md, /### Other \(no domain assigned\) \(9\)/);
  assert.match(md, /\| `@rightkit\/ui` \| foundation \| M1 \| 1\.0\.0 \| unused \| stale \(0\.9\.0\u21921\.0\.0\) \| not present \| exception \(1 exception: no issue, no expiry; required by M1\) \| unused \| unused \| - \|/);
  assert.ok(!/\r/.test(md));
});

test("the HTML output is self-contained: no network, no external assets, filters with labels, light and dark, sticky table", () => {
  const html = renderPortMatrix(modelOf(fixture()))[PORT_MATRIX_HTML];
  assert.ok(html.startsWith("<!doctype html>\n<html lang=\"en\">"));
  assert.ok(Buffer.byteLength(html) < 1.5 * 1024 * 1024);
  assert.ok(!/<script[^>]*\ssrc=|<link[\s>]|@import|url\(|<img|<iframe|\bfetch\(|XMLHttpRequest|WebSocket|localStorage|sendBeacon/.test(html));
  assert.deepEqual([...html.matchAll(/\b(?:src|href)="([^"]*)"/g)].map((m) => m[1]).sort(), ["PORT-MATRIX.md", "port-matrix.json"], "only links to the sibling outputs");
  assert.ok(!/https?:\/\//.test(html.replace(/<script>[\s\S]*<\/script>/, "").replace(/Apple[^<]*https?:\/\/[^<]*/g, "")), "no URLs in the page");
  assert.ok(html.includes("prefers-color-scheme:dark") && html.includes("color-scheme:light dark"));
  assert.ok(html.includes("position:sticky") && html.includes("left:0"));
  for (const id of ["f-app", "f-class", "f-domain", "f-state", "f-q"]) assert.match(html, new RegExp(`<label>[^<]*<(?:select|input) id="${id}"`), `${id} is labelled`);
  assert.match(html, /<table id="matrix"><caption class="vh">/);
  assert.match(html, /<th scope="row" class="pkg"><code>rightkit-alpha<\/code>/);
  assert.match(html, /<th scope="col" rowspan="2" class="pkg">Package<\/th>/);
  assert.match(html, /<div class="bar" role="img" aria-label="appone: 2 of 3 foundation owners pinned current, 67 percent"><span style="width:67%"><\/span><\/div>/);
  assert.match(html, /<details class="rk"><summary>RK-ready &amp; to-move \(4\)<\/summary>/);
  assert.match(html, /<p id="stamp">Stamp: generated 2026-10-24T12:00:00Z from RightKit not a git repo;/);
  assert.ok(html.includes('<th scope="col" class="apple" data-app="appfour" title="ios/project.yml">'), "Apple targets are sub-columns of their app");
  for (const state of STATES) assert.ok(html.includes(`<option value="${state.state}">`), `state filter has ${state.state}`);
  // Every body row has a cell for each header column: 4 leading, 6 apps, 6 Apple targets of appfour, "to move from".
  const head = html.match(/<table id="matrix">[\s\S]*?<\/thead>/)[0];
  const secondRow = head.split("</tr>")[1];
  assert.equal((secondRow.match(/<th /g) ?? []).length, 12, "6 app columns and 6 Apple sub-columns");
  const row = html.match(/<tr data-name="rightkit-alpha"[\s\S]*?<\/tr>/)[0];
  assert.equal((row.match(/<t[dh][ >]/g) ?? []).length, 4 + 12 + 1);
});

test("the filter script narrows rows by class, domain, state and search, and hides other apps' columns", () => {
  const html = renderPortMatrix(modelOf(fixture()))[PORT_MATRIX_HTML];
  const code = html.match(/<script>([\s\S]*)<\/script>/)[1];
  new Function(code); // syntax
  const el = (attrs = {}, scoped = {}) => {
    const node = { attrs, scoped, hidden: false, value: "", textContent: "", listeners: {}, classes: new Set(), style: { setProperty() {} } };
    node.getAttribute = (name) => (name in attrs ? attrs[name] : null);
    node.addEventListener = (type, fn) => { (node.listeners[type] ??= []).push(fn); };
    node.querySelectorAll = (selector) => node.scoped[selector] ?? [];
    node.classList = { toggle: (name, on) => { if (on) node.classes.add(name); else node.classes.delete(name); } };
    node.getBoundingClientRect = () => ({ height: 30 });
    return node;
  };
  const td = (app, state, extra = {}) => el({ "data-app": app, "data-state": state, ...extra });
  const rowA = el({ "data-name": "rightkit-fs", "data-class": "foundation", "data-domain": "storage" }, { "td[data-state]": [td("one", "adopted"), td("two", "stale")] });
  const rowB = el({ "data-name": "rightkit-db", "data-class": "add-on", "data-domain": "storage" }, { "td[data-state]": [td("one", "unused"), td("two", "exception", { "data-exc": "1" })] });
  const rowC = el({ "data-name": "rightkit-ai", "data-class": "add-on", "data-domain": "ai" }, { "td[data-state]": [td("one", "unused"), td("two", "unused")] });
  const groupA = el({ "data-domain": "storage" }, { "tr[data-name]": [rowA, rowB] });
  const groupB = el({ "data-domain": "ai" }, { "tr[data-name]": [rowC] });
  const headerApp = el({ "data-app": "one" });
  const all = [headerApp, ...rowA.scoped["td[data-state]"], ...rowB.scoped["td[data-state]"], ...rowC.scoped["td[data-state]"]];
  const controls = { "#f-app": el(), "#f-class": el(), "#f-domain": el(), "#f-state": el(), "#f-q": el(), "#count": el(), "#filters": el(), "#matrix": el(), "#matrix thead tr:first-child": el() };
  const cardOne = el({ "data-card": "one" }), cardTwo = el({ "data-card": "two" });
  const lists = { "#matrix tbody tr[data-name]": [rowA, rowB, rowC], "#matrix tbody[data-domain]": [groupA, groupB], "[data-app]": all, "[data-card]": [cardOne, cardTwo] };
  const document = { querySelector: (s) => controls[s] ?? null, querySelectorAll: (s) => lists[s] ?? [] };
  new Function("document", "setTimeout", code)(document, (fn) => fn());
  const change = () => controls["#filters"].listeners.change.forEach((fn) => fn());
  assert.equal(controls["#count"].textContent, "Showing 3 of 3 packages");
  controls["#f-class"].value = "add-on"; change();
  assert.deepEqual([rowA.hidden, rowB.hidden, rowC.hidden], [true, false, false]);
  controls["#f-domain"].value = "storage"; change();
  assert.deepEqual([rowA.hidden, rowB.hidden, rowC.hidden, groupB.hidden, groupA.hidden], [true, false, true, true, false]);
  controls["#f-class"].value = ""; controls["#f-domain"].value = ""; controls["#f-state"].value = "stale"; change();
  assert.deepEqual([rowA.hidden, rowB.hidden, rowC.hidden], [false, true, true]);
  assert.equal(rowA.scoped["td[data-state]"][0].classes.has("dim"), true, "cells of another state are dimmed");
  assert.equal(rowA.scoped["td[data-state]"][1].classes.has("dim"), false);
  controls["#f-state"].value = "exception"; change();
  assert.deepEqual([rowA.hidden, rowB.hidden], [true, false], "exception also matches an exception noted on a pinned cell");
  controls["#f-state"].value = ""; controls["#f-q"].value = " RIGHTKIT-A"; change();
  assert.deepEqual([rowA.hidden, rowB.hidden, rowC.hidden], [true, true, false]);
  assert.equal(controls["#count"].textContent, "Showing 1 of 3 packages");
  controls["#f-q"].value = ""; controls["#f-app"].value = "one"; change();
  assert.equal(headerApp.hidden, false);
  assert.deepEqual(rowA.scoped["td[data-state]"].map((c) => c.hidden), [false, true], "the other app's column is hidden");
  assert.deepEqual([cardOne.hidden, cardTwo.hidden], [false, true], "and so is its progress card");
  controls["#f-state"].value = "stale"; change();
  assert.equal(rowA.hidden, true, "state is matched only among visible columns");
});

test("generate-index writes the matrix next to the other docs; --check and --check-adoption compare them", () => {
  const fx = fixture();
  const args = ["--root", fx.root, "--workspace", fx.workspace, "--triage", fx.triage, "--now", NOW];
  const run = (...more) => spawnSync(process.execPath, [script, ...args, ...more], { encoding: "utf8" });
  const written = run();
  assert.equal(written.status, 0, written.stderr);
  for (const rel of PORT_MATRIX_OUTPUTS) assert.match(written.stdout, new RegExp(`${rel.replace(/[.]/g, "\\.")}: \\d+ lines`), rel);
  assert.match(written.stdout, /port matrix: 10 packages x 6 apps/);
  assert.match(written.stdout, /appone\s+ 67% \(2\/3 owners current\)/);
  for (const rel of PORT_MATRIX_OUTPUTS) assert.ok(readFileSync(path.join(fx.root, rel), "utf8").length > 100, rel);

  // --check: no app repos needed; the MD and HTML must be the rendering of the JSON.
  const noApps = spawnSync(process.execPath, [script, "--check", "--root", fx.root, "--workspace", path.join(fx.base, "nothing")], { encoding: "utf8" });
  assert.equal(noApps.status, 0, noApps.stderr);
  assert.deepEqual(checkPortMatrixConsistency(fx.root), []);
  const mdFile = path.join(fx.root, PORT_MATRIX_MD);
  writeFileSync(mdFile, `${readFileSync(mdFile, "utf8")}\nhand edit\n`);
  const edited = spawnSync(process.execPath, [script, "--check", "--root", fx.root], { encoding: "utf8" });
  assert.equal(edited.status, 1);
  assert.match(edited.stderr, /DRIFT docs\/PORT-MATRIX\.md: committed file is not the rendering of docs\/port-matrix\.json/);
  assert.ok(!edited.stderr.includes("DRIFT INDEX.md"));
  assert.equal(run().status, 0, "regenerating repairs it");
  rmSync(path.join(fx.root, PORT_MATRIX_JSON));
  const gone = spawnSync(process.execPath, [script, "--check", "--root", fx.root], { encoding: "utf8" });
  assert.equal(gone.status, 1);
  assert.match(gone.stderr, /DRIFT docs\/port-matrix\.json: missing/);
  assert.equal(run().status, 0);

  // --check-adoption: regenerated from the app repos with the stamp, HEADs, dirs and check counts carried from port-matrix.json.
  const drift = (...more) => spawnSync(process.execPath, [script, "--check-adoption", "--root", fx.root, "--workspace", fx.workspace, ...more], { encoding: "utf8" });
  const current = drift("--now", "2031-01-01T00:00:00Z", "--triage", fx.triage);
  assert.equal(current.status, 0, current.stderr);
  assert.equal(drift().status, 0, "the triage counts are carried from the committed JSON when --triage is not passed");
  write(path.join(fx.workspace, "appone/package.json"), { name: "appone", dependencies: { "@rightkit/ui": "1.0.0" } });
  const changed = drift();
  assert.equal(changed.status, 1);
  for (const rel of PORT_MATRIX_OUTPUTS) assert.match(changed.stderr, new RegExp(`DRIFT ${rel.replace(/[.]/g, "\\.")}`), rel);
  assert.ok(!changed.stderr.includes("DRIFT docs/ADOPTION.md"), "the pin is not in ADOPTION's app list");
});

test("a written matrix keeps every output the same when the stamp is the only thing that changed", () => {
  const fx = fixture();
  const run = (now) => spawnSync(process.execPath, [script, "--root", fx.root, "--workspace", fx.workspace, "--now", now], { encoding: "utf8" });
  assert.equal(run(NOW).status, 0);
  const first = Object.fromEntries(PORT_MATRIX_OUTPUTS.map((rel) => [rel, readFileSync(path.join(fx.root, rel), "utf8")]));
  assert.equal(run(NOW).status, 0);
  for (const rel of PORT_MATRIX_OUTPUTS) assert.equal(readFileSync(path.join(fx.root, rel), "utf8"), first[rel], `${rel} is identical on a second run`);
  assert.equal(run("2027-02-02T02:02:02Z").status, 0);
  for (const rel of [PORT_MATRIX_MD, PORT_MATRIX_HTML]) assert.equal(stampFree(readFileSync(path.join(fx.root, rel), "utf8")), stampFree(first[rel]), rel);
});

test("the generator rejects bad arguments", () => {
  const fx = fixture();
  const bad = (...args) => spawnSync(process.execPath, [script, "--root", fx.root, ...args], { encoding: "utf8" });
  assert.notEqual(bad("--app-dir", "nope").status, 0);
  assert.notEqual(bad("--now", "not a time").status, 0);
  assert.notEqual(bad("--triage", path.join(fx.base, "missing.json")).status, 0);
});

test("the committed port matrix of this repository is internally consistent", () => {
  assert.deepEqual(checkPortMatrixConsistency(DEFAULT_ROOT), []);
  const model = JSON.parse(readFileSync(path.join(DEFAULT_ROOT, PORT_MATRIX_JSON), "utf8"));
  assert.equal(model.schema, 1);
  assert.deepEqual(model.legend.map((l) => l.state), STATES.map((s) => s.state));
  assert.ok(model.packages.length > 90 && model.apps.length >= 12);
  for (const pkg of model.packages) for (const app of model.apps) assert.ok(STATES.some((s) => s.state === pkg.cells[app.name]?.state), `${pkg.name} x ${app.name}`);
  assert.ok(generatePortMatrix);
});

test("a foundation owner declared only as a dev dependency counts only when the row allows it", () => {
  const fx = fixture();
  write(path.join(fx.workspace, "apptwo/Cargo.toml"), '[package]\nname = "apptwo"\nversion = "1.0.0"\n[dependencies]\nrightkit-alpha = "=0.2.0"\n[dev-dependencies]\nrightkit-beta = "=0.3.0"\n');
  let m2 = appOf(modelOf(fx), "apptwo").foundationRows.find((r) => r.id === "M2");
  assert.equal(m2.status, "open", "a dev-only pin does not declare the row");
  assert.deepEqual(m2.missing, ["rightkit-beta"]);
  assert.match(cellOf(modelOf(fx), "rightkit-beta", "apptwo").detail, /declared only as a dev dependency/);
  const setFile = path.join(fx.root, "packages/release/ownership-mandatory-set.json");
  const set = JSON.parse(readFileSync(setFile, "utf8"));
  set.rows.find((r) => r.id === "M2").owners[0].allowDev = true;
  write(setFile, set);
  m2 = appOf(modelOf(fx), "apptwo").foundationRows.find((r) => r.id === "M2");
  assert.equal(m2.status, "declared", "allowDev counts the dev pin");
  assert.deepEqual(m2.missing, []);
});

test("an exception that expires today is still valid; tomorrow it is expired", () => {
  const fx = fixture();
  const file = path.join(fx.workspace, "appone/.rightkit/ownership-exceptions.json");
  const doc = JSON.parse(readFileSync(file, "utf8"));
  const key = Object.keys(doc).find((k) => Array.isArray(doc[k]));
  doc[key] = [{ package: "@rightkit/web", capability: "web-cap", path: "src/z.rs", issue: "rightkit#9", expires: "2026-10-24" }];
  write(file, doc);
  const today = cellOf(modelOf(fx), "@rightkit/web", "appone");
  assert.equal(today.exceptions.expired, 0, "expires on the current day is not expired");
  assert.doesNotMatch(today.detail, /expired/);
  const tomorrow = cellOf(modelOf(fx, { now: "2026-10-25T00:00:00Z" }), "@rightkit/web", "appone");
  assert.equal(tomorrow.exceptions.expired, 1);
});
