import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { macCodesignArgs, parseSignManifest, signMacosEntries } from "./mac-sign-core.mjs";
import { hasNonDefaultSettings, matrixReceiptProblems, planMacSigning, validateMacSigningConfig } from "./mac-signing-matrix.mjs";

const PLIST = '<?xml version="1.0" encoding="UTF-8"?>\n<plist version="1.0"><dict/></plist>\n';
const MACHO = Buffer.from("cffaedfe00000000", "hex");

// The Pulse bundle shape: app + three helpers + a sandboxed Finder extension + a framework.
function pulseFixture(t) {
  const root = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rk-sign-matrix-")));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const put = (relative, bytes = MACHO) => { const file = path.join(root, relative); mkdirSync(path.dirname(file), { recursive: true }); writeFileSync(file, bytes); return file; };
  put("release/entitlements.plist", PLIST);
  put("mac/FinderExtension/PulseFinder.entitlements", PLIST);
  const app = path.join(root, "dist/Pulse.app");
  put("dist/Pulse.app/Contents/Info.plist", '<plist><dict><key>CFBundleExecutable</key><string>Pulse</string></dict></plist>');
  put("dist/Pulse.app/Contents/MacOS/Pulse");
  for (const helper of ["PulseHelper", "pulse-elevate", "smartctl"]) put(`dist/Pulse.app/Contents/Helpers/${helper}`);
  put("dist/Pulse.app/Contents/PlugIns/PulseFinder.appex/Contents/Info.plist", PLIST);
  put("dist/Pulse.app/Contents/PlugIns/PulseFinder.appex/Contents/MacOS/PulseFinder");
  put("dist/Pulse.app/Contents/Frameworks/Core.framework/Versions/A/Core");
  return { root, app, put };
}

const pulseSign = () => ({
  prePackageFiles: [{ file: "dist/Pulse.app", entitlements: "release/entitlements.plist", hardenedRuntime: true }],
  nestedCode: {
    "Contents/Helpers/PulseHelper": { hardenedRuntime: true },
    "Contents/Helpers/pulse-elevate": { hardenedRuntime: true },
    "Contents/Helpers/smartctl": { hardenedRuntime: true },
    "Contents/PlugIns/PulseFinder.appex": { hardenedRuntime: true, entitlements: "mac/FinderExtension/PulseFinder.entitlements" },
  },
});

test("string-only prePackageFiles validate unchanged and carry no settings", (t) => {
  const f = pulseFixture(t);
  const { entries, nestedCode } = validateMacSigningConfig({ prePackageFiles: ["dist/Pulse.app", "dist/tool"] }, { root: f.root });
  assert.deepEqual(entries, [{ file: "dist/Pulse.app", options: [] }, { file: "dist/tool", options: [] }]);
  assert.deepEqual(nestedCode, []);
  assert.equal(entries.some(hasNonDefaultSettings), false);
});

test("the Pulse matrix validates and resolves entitlement paths", (t) => {
  const f = pulseFixture(t);
  const { entries, nestedCode } = validateMacSigningConfig(pulseSign(), { root: f.root });
  assert.equal(entries[0].entitlements, path.join(f.root, "release/entitlements.plist"));
  assert.equal(nestedCode.length, 4);
  assert.equal(nestedCode.find((item) => item.key.endsWith(".appex")).entitlements, path.join(f.root, "mac/FinderExtension/PulseFinder.entitlements"));
});

const bad = (t, sign, pattern) => {
  const f = pulseFixture(t);
  assert.throws(() => validateMacSigningConfig(sign, { root: f.root }), (error) => { assert.equal(error.code, "MAC_SIGN_CONFIG_INVALID"); assert.match(error.message, pattern); return true; });
};

test("missing entitlement files fail early, for entries and nested code", (t) => {
  bad(t, { prePackageFiles: [{ file: "dist/Pulse.app", entitlements: "release/missing.plist" }] }, /prePackageFiles\[0\]\.entitlements file not found: release\/missing\.plist/);
  bad(t, { prePackageFiles: ["dist/Pulse.app"], nestedCode: { "Contents/Helpers/x": { entitlements: "nope.plist" } } }, /nestedCode\["Contents\/Helpers\/x"\]\.entitlements file not found/);
});

test("entitlements must be a plist inside the config directory", (t) => {
  const f = pulseFixture(t);
  f.put("release/not-a-plist.json", "{}");
  assert.throws(() => validateMacSigningConfig({ prePackageFiles: [{ file: "a.app", entitlements: "release/not-a-plist.json" }] }, { root: f.root }), /not a property list/);
  assert.throws(() => validateMacSigningConfig({ prePackageFiles: [{ file: "a.app", entitlements: "../outside.plist" }] }, { root: f.root }), /inside the release config directory/);
  assert.throws(() => validateMacSigningConfig({ prePackageFiles: [{ file: "a.app", entitlements: "/etc/x.plist" }] }, { root: f.root }), /relative to the release config directory/);
});

test("entry shape errors are reported together", (t) => {
  bad(t, { prePackageFiles: [42, { file: "" }, { file: "a", mystery: 1 }, "a", { file: "a" }] }, /prePackageFiles\[0\] must be a path string[\s\S]*file must be a non-empty[\s\S]*unknown key "mystery"[\s\S]*duplicate signing entry a/);
  bad(t, { prePackageFiles: [{ file: "a.app", hardenedRuntime: "yes" }] }, /hardenedRuntime must be a boolean/);
  bad(t, { prePackageFiles: [{ file: "a.app", hardenedRuntime: false }] }, /hardenedRuntime: false is not allowed[\s\S]*notarization/);
  bad(t, { prePackageFiles: [{ file: "a.app", options: ["library", "bogus", "library"] }] }, /unsupported codesign option "bogus"[\s\S]*duplicate option "library"/);
  bad(t, { prePackageFiles: [{ file: "a.app", options: "runtime" }] }, /options must be an array/);
  bad(t, { prePackageFiles: [] }, /non-empty array/);
});

test("nestedCode keys must be bundle-relative Contents/ paths and need exactly one .app entry", (t) => {
  for (const key of ["/abs/x", "../x", "Contents/../x", "Contents/x/", "Contents//x", "Resources/x", "Contents", "Contents\\x", ""]) {
    bad(t, { prePackageFiles: ["a.app"], nestedCode: { [key]: {} } }, /key must/);
  }
  bad(t, { prePackageFiles: ["tool"], nestedCode: { "Contents/Helpers/x": {} } }, /exactly one \.app entry[\s\S]*found 0/);
  bad(t, { prePackageFiles: ["a.app", "b.app"], nestedCode: { "Contents/Helpers/x": {} } }, /exactly one \.app entry[\s\S]*found 2/);
  bad(t, { prePackageFiles: ["a.app"], nestedCode: [] }, /must be an object/);
  bad(t, { prePackageFiles: ["a.app"], nestedCode: { "Contents/Helpers/x": { file: "y" } } }, /unknown key "file"/);
  bad(t, { prePackageFiles: ["a.app"], nestedCode: { "Contents/Helpers/x": "runtime" } }, /must be an object/);
});

test("plan signs nested code deepest first, matrix items interleaved, then the app entry", (t) => {
  const f = pulseFixture(t);
  const { entries, nestedCode } = validateMacSigningConfig(pulseSign(), { root: f.root });
  const plan = planMacSigning({ entries: entries.map((entry) => ({ ...entry, file: path.join(f.root, entry.file) })), nestedCode });
  const rel = (file) => path.relative(f.app, file);
  assert.deepEqual(plan.nested.map((item) => rel(item.file)), [
    "Contents/Frameworks/Core.framework/Versions/A/Core",
    "Contents/PlugIns/PulseFinder.appex/Contents/MacOS/PulseFinder",
    "Contents/Frameworks/Core.framework",
    "Contents/Helpers/pulse-elevate", // same depth: locale order, identical to the legacy discovery order
    "Contents/Helpers/PulseHelper",
    "Contents/Helpers/smartctl",
    "Contents/PlugIns/PulseFinder.appex",
  ]);
  const finder = plan.nested.find((item) => item.file.endsWith("PulseFinder.appex"));
  assert.equal(finder.entitlements, path.join(f.root, "mac/FinderExtension/PulseFinder.entitlements"));
  assert.equal(plan.nested.filter((item) => item.entitlements).length, 1);
  assert.deepEqual(plan.top.map((item) => rel(item.file)), [""]);
  assert.equal(plan.top[0].entitlements, path.join(f.root, "release/entitlements.plist"));
});

test("plan without nestedCode is the legacy discovery with default settings", (t) => {
  const f = pulseFixture(t);
  const plan = planMacSigning({ entries: [{ file: f.app, options: [] }] });
  assert.ok(plan.nested.length >= 6);
  assert.equal(plan.nested.some(hasNonDefaultSettings), false);
});

test("plan adds a nestedCode path discovery does not find, and orders it by depth", (t) => {
  const f = pulseFixture(t);
  f.put("dist/Pulse.app/Contents/Resources/a/b/c/d/script.sh", "#!/bin/sh\n");
  const sign = { prePackageFiles: ["dist/Pulse.app"], nestedCode: { "Contents/Resources/a/b/c/d/script.sh": { options: ["library"] } } };
  const { entries, nestedCode } = validateMacSigningConfig(sign, { root: f.root });
  const plan = planMacSigning({ entries: entries.map((entry) => ({ ...entry, file: path.join(f.root, entry.file) })), nestedCode });
  assert.equal(path.relative(f.app, plan.nested[0].file), "Contents/Resources/a/b/c/d/script.sh");
  assert.deepEqual(plan.nested[0].options, ["library"]);
});

test("plan fails before signing for missing, symlinked, escaping or main-executable nestedCode paths", (t) => {
  const f = pulseFixture(t);
  const run = (key) => {
    const { entries, nestedCode } = validateMacSigningConfig({ prePackageFiles: ["dist/Pulse.app"], nestedCode: { [key]: {} } }, { root: f.root });
    return () => planMacSigning({ entries: entries.map((entry) => ({ ...entry, file: path.join(f.root, entry.file) })), nestedCode });
  };
  assert.throws(run("Contents/Helpers/ghost"), /not found inside/);
  assert.throws(run("Contents/MacOS/Pulse"), /main executable is signed with the bundle/);
  symlinkSync(path.join(f.root, "release"), path.join(f.app, "Contents/Helpers/link"));
  assert.throws(run("Contents/Helpers/link"), /symlinks cannot be signed/);
  symlinkSync(path.join(f.root, "release"), path.join(f.app, "Contents/Resources-link"));
  assert.throws(run("Contents/Resources-link/entitlements.plist"), /resolves outside the bundle/);
});

test("fake signer receives codesign invocations inside-out with the matrix applied", (t) => {
  const f = pulseFixture(t);
  const { entries, nestedCode } = validateMacSigningConfig(pulseSign(), { root: f.root });
  const plan = planMacSigning({ entries: entries.map((entry) => ({ ...entry, file: path.join(f.root, entry.file) })), nestedCode });
  const calls = [];
  const identity = "Developer ID Application: Fake (TEAM123456)";
  const inspect = (cmd, args) => {
    calls.push([cmd, ...args]);
    return args[0] === "--verify" ? "valid on disk\nsatisfies its designated requirement" : `Authority=${identity}\nTimestamp=Oct 11 2026`;
  };
  const records = signMacosEntries([...plan.nested, ...plan.top], {
    identity, signOne: (args) => { calls.push(["codesign", ...args]); return { status: 0 }; }, inspect,
    evidence: () => ({ sha256: "0".repeat(64), sizeBytes: 0 }), hashFile: () => "e".repeat(64),
  });
  const signs = calls.filter((call) => call[1] === "--force");
  const order = signs.map((call) => path.relative(f.app, call.at(-1)));
  assert.equal(order.at(-1), "", "the app is signed last");
  assert.ok(order.indexOf("Contents/PlugIns/PulseFinder.appex/Contents/MacOS/PulseFinder") < order.indexOf("Contents/PlugIns/PulseFinder.appex"));
  assert.ok(order.indexOf("Contents/Frameworks/Core.framework/Versions/A/Core") < order.indexOf("Contents/Frameworks/Core.framework"));
  // Helpers: hardened runtime, no entitlements. Appex: its own entitlements. App: the app entitlements.
  const helper = signs.find((call) => call.at(-1).endsWith("Helpers/smartctl"));
  assert.deepEqual(helper, ["codesign", "--force", "--options", "runtime", "--timestamp", "--sign", identity, helper.at(-1)]);
  const appex = signs.find((call) => call.at(-1).endsWith("PulseFinder.appex"));
  assert.deepEqual(appex.slice(0, 7), ["codesign", "--force", "--options", "runtime", "--timestamp", "--sign", identity]);
  assert.deepEqual(appex.slice(7, 9), ["--entitlements", path.join(f.root, "mac/FinderExtension/PulseFinder.entitlements")]);
  const app = signs.at(-1);
  assert.deepEqual(app.slice(7, 9), ["--entitlements", path.join(f.root, "release/entitlements.plist")]);
  assert.equal(signs.filter((call) => call.includes("--entitlements")).length, 2, "only the app and the appex carry entitlements");
  // Each sign is followed by its verify + display before the next sign starts.
  assert.deepEqual(calls.slice(0, 3).map((call) => call[1]), ["--force", "--verify", "-dvv"]);
  // Receipt records and the plan agree.
  assert.deepEqual(matrixReceiptProblems(records, [...plan.nested, ...plan.top], () => "e".repeat(64)), []);
  assert.match(matrixReceiptProblems(records, [...plan.nested, ...plan.top], () => "f".repeat(64)).join("\n"), /entitlements digest does not match/);
});

test("a failing codesign or verification stops the sequence", () => {
  const entries = [{ file: "/a", options: [] }, { file: "/b", options: [] }];
  const signed = [];
  assert.throws(() => signMacosEntries(entries, { identity: "id", signOne: (args) => { signed.push(args.at(-1)); return { status: 1, stderr: "boom" }; }, inspect: () => "", evidence: () => ({}) }), /codesign failed: boom/);
  assert.deepEqual(signed, ["/a"]);
  assert.throws(() => signMacosEntries(entries, { identity: "id", signOne: () => ({ status: 0 }), inspect: (cmd, args) => (args[0] === "--verify" ? "invalid" : ""), evidence: () => ({}) }), /strict codesign verification failed for \/a/);
});

test("codesign argv: defaults are the historical form; options extend runtime", () => {
  assert.deepEqual(macCodesignArgs({ file: "/x", identity: "id" }), ["--force", "--options", "runtime", "--timestamp", "--sign", "id", "/x"]);
  assert.deepEqual(macCodesignArgs({ file: "/x", identity: "id", options: ["library", "runtime"], entitlements: "/e.plist" }),
    ["--force", "--options", "runtime,library", "--timestamp", "--sign", "id", "--entitlements", "/e.plist", "/x"]);
});

test("manifest parsing validates entries", () => {
  const ok = parseSignManifest(JSON.stringify({ schema: 1, entries: [{ file: "/a", entitlements: "/e.plist", options: ["kill"] }, { file: "/b" }] }));
  assert.deepEqual(ok, [{ file: "/a", entitlements: "/e.plist", hardenedRuntime: true, options: ["kill"] }, { file: "/b", hardenedRuntime: true, options: [] }]);
  assert.throws(() => parseSignManifest("{"), /not valid JSON/);
  assert.throws(() => parseSignManifest(JSON.stringify({ schema: 2, entries: [] })), /must be \{ schema: 1/);
  assert.throws(() => parseSignManifest(JSON.stringify({ schema: 1, entries: [{ file: "rel" }] })), /absolute path/);
  assert.throws(() => parseSignManifest(JSON.stringify({ schema: 1, entries: [{ file: "/a", options: ["--sign"] }] })), /codesign option names/);
});
