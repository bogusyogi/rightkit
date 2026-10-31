import assert from "node:assert/strict";
import test from "node:test";
import { packageLockProblems, parsePackageSwift, parsePbxproj, parseResolved, requirementProblem, xcodeProjectLockProblems } from "./swift-lock.mjs";

const mem = (files) => ({ read: (f) => { if (!(f in files)) throw new Error(`ENOENT ${f}`); return files[f]; }, exists: (f) => f in files });
const RK = "https://github.com/orthiclabs/rightkit-swift.git";
const pkg = (dep) => `// swift-tools-version:5.9\nimport PackageDescription\nlet package = Package(name: "A", dependencies: [\n  ${dep}\n], targets: [])\n`;
const v2 = (pins) => JSON.stringify({ version: 2, pins: pins.map(([loc, version, extra = {}]) => ({ identity: loc.split("/").pop().replace(/\.git$/, "").toLowerCase(), kind: "remoteSourceControl", location: loc, state: { revision: "a".repeat(40), ...(version ? { version } : {}), ...extra } })) });
const run = (files, dir = ".") => packageLockProblems({ dir, ...mem(files) });

test("matching exact pin passes", () => {
  assert.deepEqual(run({ "Package.swift": pkg(`.package(url: "${RK}", exact: "0.8.1"),`), "Package.resolved": v2([[RK, "0.8.1"]]) }), []);
});

test("stale exact: Package.swift 0.8.1, Package.resolved 0.8.0 names file, identity, requirement and pin", () => {
  const out = run({ "Package.swift": pkg(`.package(url: "${RK}", exact: "0.8.1"),`), "Package.resolved": v2([[RK, "0.8.0"]]) });
  assert.equal(out.length, 1);
  assert.match(out[0], /^Package\.swift:4: package rightkit-swift requires exact 0\.8\.1 but Package\.resolved pins version 0\.8\.0, not 0\.8\.1$/);
});

test("ranges: from / upToNextMajor / upToNextMinor / half-open and closed, satisfied and not", () => {
  const cases = [
    [`.package(url: "${RK}", from: "1.2.0")`, "1.9.0", true], [`.package(url: "${RK}", from: "1.2.0")`, "2.0.0", false], [`.package(url: "${RK}", from: "1.2.0")`, "1.1.9", false],
    [`.package(url: "${RK}", .upToNextMajor(from: "0.8.0"))`, "0.9.3", true], [`.package(url: "${RK}", .upToNextMajor(from: "0.8.0"))`, "1.0.0", false],
    [`.package(url: "${RK}", .upToNextMinor(from: "0.8.0"))`, "0.8.9", true], [`.package(url: "${RK}", .upToNextMinor(from: "0.8.0"))`, "0.9.0", false],
    [`.package(url: "${RK}", "1.0.0"..<"2.0.0")`, "1.5.0", true], [`.package(url: "${RK}", "1.0.0"..<"2.0.0")`, "2.0.0", false],
    [`.package(url: "${RK}", "1.0.0"..."2.0.0")`, "2.0.0", true], [`.package(url: "${RK}", .exact("1.0.0"))`, "1.0.1", false],
    [`.package(url: "${RK}", from: "1.0.0-beta.1")`, "1.0.0-beta.2", true],
  ];
  for (const [dep, pin, ok] of cases) {
    const out = run({ "Package.swift": pkg(dep), "Package.resolved": v2([[RK, pin]]) });
    assert.equal(out.length === 0, ok, `${dep} with ${pin}: ${out.join("; ")}`);
  }
});

test("missing pin, missing Package.resolved, no remote deps", () => {
  const dep = pkg(`.package(url: "${RK}", exact: "0.8.1"),`);
  assert.match(run({ "Package.swift": dep, "Package.resolved": v2([["https://github.com/x/other", "1.0.0"]]) })[0], /package rightkit-swift requires exact 0\.8\.1 but Package\.resolved has no pin for it/);
  assert.match(run({ "Package.swift": dep })[0], /Package\.swift: 1 remote dependency \(rightkit-swift\) and no Package\.resolved/);
  assert.deepEqual(run({ "Package.swift": pkg(`.package(path: "../rk"),`) }), [], "a path dependency needs no pin");
});

test("a pin's location that differs is a finding; scp, .git and case forms are the same location", () => {
  const dep = pkg(`.package(url: "${RK}", exact: "0.8.1"),`);
  assert.match(run({ "Package.swift": dep, "Package.resolved": v2([["https://github.com/evil/rightkit-swift", "0.8.1"]]) })[0], /pins https:\/\/github\.com\/evil\/rightkit-swift/);
  assert.deepEqual(run({ "Package.swift": dep, "Package.resolved": v2([["git@github.com:OrthicLabs/rightkit-swift", "0.8.1"]]) }), []);
});

test("branch and revision requirements", () => {
  const rev = "b".repeat(40);
  const br = pkg(`.package(url: "${RK}", branch: "main"),`); const rv = pkg(`.package(url: "${RK}", revision: "${rev}"),`);
  assert.deepEqual(run({ "Package.swift": br, "Package.resolved": v2([[RK, null, { branch: "main" }]]) }), []);
  assert.match(run({ "Package.swift": br, "Package.resolved": v2([[RK, "1.0.0"]]) })[0], /not branch main/);
  assert.deepEqual(run({ "Package.swift": rv, "Package.resolved": v2([[RK, null, { revision: rev }]]) }), []);
  assert.match(run({ "Package.swift": rv, "Package.resolved": v2([[RK, "1.0.0"]]) })[0], /not revision b+/);
});

test("Package.resolved v1 and v3 are read", () => {
  const v1 = JSON.stringify({ version: 1, object: { pins: [{ package: "RightKitSwift", repositoryURL: RK, state: { branch: null, revision: "c".repeat(40), version: "0.8.0" } }] } });
  const dep = pkg(`.package(url: "${RK}", exact: "0.8.1"),`);
  assert.match(run({ "Package.swift": dep, "Package.resolved": v1 })[0], /pins version 0\.8\.0/);
  assert.deepEqual(run({ "Package.swift": dep, "Package.resolved": v1.replace("0.8.0", "0.8.1") }), []);
  assert.deepEqual(run({ "Package.swift": dep, "Package.resolved": JSON.stringify({ ...JSON.parse(v2([[RK, "0.8.1"]])), version: 3, originHash: "x" }) }), []);
  assert.match(run({ "Package.swift": dep, "Package.resolved": '{"version":9}' })[0], /unsupported Package\.resolved version 9/);
  assert.match(run({ "Package.swift": dep, "Package.resolved": "{" })[0], /Package\.resolved: not valid JSON/);
});

test("an unparseable declaration is a finding naming the line, never a silent pass", () => {
  const src = `import PackageDescription\nlet v = "1.0.0"\nlet package = Package(name: "A", dependencies: [\n  .package(url: "${RK}", from: v),\n  .package(url: base + "x", exact: "1.0.0"),\n  .package(id: "scope.name", from: "1.0.0"),\n  .package(url: "${RK}", exact: "1.0.0", more),\n], targets: [])\n`;
  const out = run({ "Package.swift": src, "Package.resolved": v2([[RK, "1.0.0"]]) });
  assert.equal(out.length, 4, out.join("\n"));
  assert.match(out[0], /^Package\.swift:4: .*cannot be read literally/);
  assert.match(out[1], /^Package\.swift:5: /); assert.match(out[2], /^Package\.swift:6: /); assert.match(out[3], /^Package\.swift:7: /);
  const cond = `let package = Package(name: "A", dependencies: [\n#if os(macOS)\n  .package(url: "${RK}", exact: "1.0.0"),\n#endif\n], targets: [])\n`;
  assert.match(run({ "Package.swift": cond, "Package.resolved": v2([]) })[0], /^Package\.swift:3: .*#if region/);
});

test("comments are ignored and nested strings do not confuse the reader", () => {
  const src = `// .package(url: "https://x/y", exact: "9.9.9"),\n/* .package(url: "https://x/z", from: "1.0.0") */\nlet package = Package(name: "A", dependencies: [\n  .package(url: "${RK}", exact: "0.8.1"), // https://not/a/comment\n], targets: [])\n`;
  const parsed = parsePackageSwift(src);
  assert.deepEqual(parsed.problems, []);
  assert.equal(parsed.deps.length, 1);
  assert.equal(parsed.deps[0].line, 4);
});

test("pbxproj: XCRemoteSwiftPackageReference requirements against the workspace Package.resolved", () => {
  const pbx = (req) => `/* Begin XCRemoteSwiftPackageReference section */\n\t\tAAA /* XCRemoteSwiftPackageReference "rightkit-swift" */ = {\n\t\t\tisa = XCRemoteSwiftPackageReference;\n\t\t\trepositoryURL = "${RK}";\n\t\t\trequirement = {\n${req}\n\t\t\t};\n\t\t};\n/* End XCRemoteSwiftPackageReference section */\n`;
  const P = "App.xcodeproj"; const R = `${P}/project.xcworkspace/xcshareddata/swiftpm/Package.resolved`;
  const files = (req, pins) => ({ [`${P}/project.pbxproj`]: pbx(req), ...(pins ? { [R]: v2(pins) } : {}) });
  const run2 = (f) => xcodeProjectLockProblems({ project: P, ...mem(f) });
  assert.deepEqual(run2(files("\t\t\t\tkind = exactVersion;\n\t\t\t\tversion = 0.8.1;", [[RK, "0.8.1"]])), []);
  assert.match(run2(files("\t\t\t\tkind = exactVersion;\n\t\t\t\tversion = 0.8.1;", [[RK, "0.8.0"]]))[0], /App\.xcodeproj\/project\.pbxproj:\d+: package rightkit-swift requires exact 0\.8\.1 but .*Package\.resolved pins version 0\.8\.0/);
  assert.deepEqual(run2(files("\t\t\t\tkind = upToNextMajorVersion;\n\t\t\t\tminimumVersion = 0.8.0;", [[RK, "0.9.0"]])), []);
  assert.match(run2(files("\t\t\t\tkind = upToNextMinorVersion;\n\t\t\t\tminimumVersion = 0.8.0;", [[RK, "0.9.0"]]))[0], /up to next minor/);
  assert.deepEqual(run2(files("\t\t\t\tkind = versionRange;\n\t\t\t\tmaximumVersion = 2.0.0;\n\t\t\t\tminimumVersion = 1.0.0;", [[RK, "1.4.0"]])), []);
  assert.deepEqual(run2(files('\t\t\t\tbranch = main;\n\t\t\t\tkind = branch;', [[RK, null, { branch: "main" }]])), []);
  assert.match(run2(files("\t\t\t\tkind = exactVersion;\n\t\t\t\tversion = 0.8.1;", null))[0], /1 remote Swift package \(rightkit-swift\) and no .*Package\.resolved/);
  assert.match(run2(files("\t\t\t\tkind = surprise;", [[RK, "1.0.0"]]))[0], /cannot be read literally \(kind surprise\)/);
  assert.deepEqual(parsePbxproj("nothing here").deps, []);
});

test("parse helpers: requirementProblem and parseResolved reject nonsense", () => {
  assert.match(requirementProblem({ type: "exact", version: "1.0.0" }, { version: null, branch: "main" }), /not a semantic version/);
  assert.match(parseResolved('{"version":2}').problems[0], /no pins array/);
});
