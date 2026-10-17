import assert from "node:assert/strict";
import test from "node:test";
import { installTauriDevId, resetTauriDevId, resolveTauriDevIdDescriptor } from "./install-tauri-devid.mjs";

const DEST = "/Applications/Demo.app";
const REQ = 'identity "Developer ID Application: Orthic Labs (6KLGD3LLKF)"';
const config = (extra = {}) => ({ development: { targets: { mac: { install: { kind: "tauri-devid", productName: "Demo", destination: DEST, bundleId: "com.example.demo", ...extra } } } } });

// In-memory fake of every external call. `files` maps path -> contents (a directory is just an entry).
function harness({ files = {}, installedRequirement = REQ, signedRequirement = REQ, verifyRequirement = signedRequirement, verifyStatus = 0, signResult = {}, installResult } = {}) {
  const fs = new Map(Object.entries({ "/work/Demo.app": "built", ...files }));
  const calls = [];
  const deps = {
    exists: (p) => fs.has(p),
    makeTemp: () => { fs.set("/tmp/stage", "dir"); return "/tmp/stage"; },
    copyApp: (from, to) => { calls.push(`copy ${from} -> ${to}`); fs.set(to, fs.get(from)); fs.set(`${to}/Contents/MacOS/demo`, "signed-bin"); },
    sign: async ({ bundle }) => { calls.push(`sign ${bundle}`); return { identity: "Developer ID Application: Orthic Labs (6KLGD3LLKF)", teamIdentifier: "6KLGD3LLKF", designatedRequirement: signedRequirement, ...signResult }; },
    assertRequirement: async ({ installed, expected }) => {
      calls.push(`assert-dr ${installed}`);
      if (installedRequirement !== expected) { const e = new Error("changed"); e.code = "DEV_MAC_SIGNING_REQUIREMENT_CHANGED"; throw e; }
    },
    installApp: async (opts) => {
      calls.push(`install ${opts.app} -> ${opts.dest}/${opts.appName} dev=${opts.dev}`);
      const hadOld = fs.has(DEST);
      if (hadOld) fs.set(`${DEST}.previous`, fs.get(DEST));
      fs.set(DEST, "new");
      fs.set(`${DEST}/Contents/MacOS/demo`, "signed-bin");
      return installResult ?? { ok: true, installed: DEST, previous: hadOld ? `${DEST}.previous` : null };
    },
    run: (command, args) => {
      calls.push(`run ${command} ${args.join(" ")}`);
      if (args.includes("-r-")) return { status: 0, stdout: `designated => ${verifyRequirement}`, stderr: "" };
      return { status: verifyStatus, stdout: "", stderr: verifyStatus ? "invalid" : "" };
    },
    readFile: (p) => { const v = fs.get(p); if (v === undefined) throw new Error(`ENOENT ${p}`); return Buffer.from(v); },
    rename: (from, to) => { calls.push(`rename ${from} -> ${to}`); if (!fs.has(from)) throw new Error(`ENOENT ${from}`); fs.set(to, fs.get(from)); fs.delete(from); },
    remove: (p) => { for (const key of [...fs.keys()]) if (key === p || key.startsWith(`${p}/`)) fs.delete(key); },
    log: () => {},
  };
  return { fs, calls, deps };
}

const opts = (h, extra = {}) => ({ appRoot: "/work", config: config(), built: "/work/Demo.app", deps: h.deps, env: {}, ...extra });

test("signs the staging copy, then asserts the requirement, installs, and verifies, in that order", async () => {
  const h = harness({ files: { [DEST]: "old" } });
  const result = await installTauriDevId(opts(h));
  const steps = h.calls.map((line) => line.split(" ")[0] + (line.startsWith("run") ? ` ${line.split(" ")[1]}` : ""));
  assert.deepEqual(steps, ["copy", "sign", "assert-dr", "install", "run codesign", "run codesign"]);
  assert.match(h.calls[1], /^sign \/tmp\/stage\/Demo\.app$/);
  assert.match(h.calls[3], /install \/tmp\/stage\/Demo\.app -> \/Applications\/Demo\.app dev=true/);
  assert.equal(result.installed, DEST);
  assert.equal(result.previous, `${DEST}.previous`);
  assert.equal(result.designatedRequirement, REQ);
  assert.equal(h.fs.has("/tmp/stage"), false, "staging is removed");
  assert.equal(h.fs.get(`${DEST}.previous`), "old");
});

test("a first install has no requirement to match and no previous", async () => {
  const h = harness();
  const result = await installTauriDevId(opts(h));
  assert.equal(h.calls.some((line) => line.startsWith("assert-dr")), false);
  assert.equal(result.previous, null);
});

test("a designated requirement mismatch refuses before anything is installed", async () => {
  const h = harness({ files: { [DEST]: "old" }, installedRequirement: 'identity "Apple Development: Someone"' });
  await assert.rejects(installTauriDevId(opts(h)), (error) => error.code === "DEV_MAC_SIGNING_REQUIREMENT_CHANGED");
  assert.equal(h.calls.some((line) => line.startsWith("install")), false);
  assert.equal(h.fs.get(DEST), "old");
  assert.equal(h.fs.has("/tmp/stage"), false);
});

test("ad hoc or unstable signing is refused", async () => {
  for (const signResult of [{ adhoc: true }, { designatedRequirement: null }]) {
    const h = harness({ signResult });
    await assert.rejects(installTauriDevId(opts(h)), (error) => error.code === "TAURI_DEVID_SIGNING_UNSTABLE");
    assert.equal(h.calls.some((line) => line.startsWith("install")), false);
  }
});

test("a failed post-install verification restores .previous", async () => {
  const h = harness({ files: { [DEST]: "old" }, verifyStatus: 1 });
  await assert.rejects(installTauriDevId(opts(h)), (error) => error.code === "TAURI_DEVID_VERIFY_FAILED" && error.rolledBack === "restored-previous");
  assert.equal(h.fs.get(DEST), "old");
  assert.equal(h.fs.has(`${DEST}.previous`), false);
});

test("a failed verification of a first install removes it", async () => {
  const h = harness({ verifyRequirement: "cdhash H\"abc\"" });
  await assert.rejects(installTauriDevId(opts(h)), (error) => error.rolledBack === "removed-first-install");
  assert.equal(h.fs.has(DEST), false);
});

test("verifyBinaries must match the signed staging copy", async () => {
  const h = harness({ files: { [DEST]: "old" } });
  const original = h.deps.installApp;
  h.deps.installApp = async (o) => { const r = await original(o); h.fs.set(`${DEST}/Contents/MacOS/demo`, "tampered"); return r; };
  await assert.rejects(installTauriDevId(opts(h, { config: config({ verifyBinaries: ["Contents/MacOS/demo"] }) })), (error) => error.code === "TAURI_DEVID_VERIFY_FAILED");
  assert.equal(h.fs.get(DEST), "old");
});

test("descriptor validation", () => {
  assert.throws(() => resolveTauriDevIdDescriptor(config({ destination: "Demo.app" })), /absolute/);
  assert.throws(() => resolveTauriDevIdDescriptor(config({ signing: "ci-adhoc" })), /ad hoc/);
  assert.throws(() => resolveTauriDevIdDescriptor(config({ verifyBinaries: ["../x"] })), /verifyBinaries/);
  assert.throws(() => resolveTauriDevIdDescriptor({ development: { targets: { mac: { install: { kind: "tauri" } } } } }), /tauri-devid/);
});

test("reset restores .previous as the install", async () => {
  const h = harness({ files: { [DEST]: "bad", [`${DEST}.previous`]: "good" } });
  const result = await resetTauriDevId({ appRoot: "/work", config: config(), deps: h.deps });
  assert.equal(result.mode, "restore");
  assert.equal(h.fs.get(DEST), "good");
  assert.equal(h.fs.has(`${DEST}.previous`), false);
  assert.equal(h.fs.has(`${DEST}.reset`), false);
});

test("reset without a .previous fails and leaves the install alone", async () => {
  const h = harness({ files: { [DEST]: "current" } });
  await assert.rejects(resetTauriDevId({ appRoot: "/work", config: config(), deps: h.deps }), (error) => error.code === "TAURI_DEVID_NO_PREVIOUS");
  assert.equal(h.fs.get(DEST), "current");
});

test("reset only removes when mode is remove, and resets privacy only on request", async () => {
  const h = harness({ files: { [DEST]: "current", [`${DEST}.previous`]: "old" } });
  await assert.rejects(resetTauriDevId({ appRoot: "/work", config: config(), mode: "delete", deps: h.deps }), (error) => error.code === "TAURI_DEVID_RESET_MODE_INVALID");
  assert.equal(h.fs.has(DEST), true);
  await resetTauriDevId({ appRoot: "/work", config: config(), mode: "remove", deps: h.deps });
  assert.equal(h.fs.has(DEST), false);
  assert.equal(h.fs.has(`${DEST}.previous`), false);
  assert.equal(h.calls.some((line) => line.includes("tccutil")), false);
  await resetTauriDevId({ appRoot: "/work", config: config(), mode: "remove", resetPrivacy: true, deps: h.deps });
  assert.ok(h.calls.includes("run tccutil reset All com.example.demo"));
});

// ---- identity assertions on the installed copy (bogusyogi/rightkit#345) ----
const NL = String.fromCharCode(10);
const TEAM = "6KLGD3LLKF";
const dvvReport = (team, identifier = "x") => `Identifier=${identifier}${NL}Authority=Developer ID Application: Orthic Labs (${TEAM})${NL}TeamIdentifier=${team}${NL}`;
// codesign -dvv reports on stderr. `teams` maps an installed path to its TeamIdentifier (default: the app's team).
function withDvv(h, teams = {}) {
  const original = h.deps.run;
  h.deps.run = (command, args, ...rest) => {
    if (args[0] === "-dvv") { h.calls.push(`run codesign -dvv ${args[1]}`); return { status: 0, stdout: "", stderr: dvvReport(teams[args[1]] ?? TEAM) }; }
    return original(command, args, ...rest);
  };
  h.deps.listFiles = () => ["Contents/MacOS/demo", "Contents/Frameworks/libonnxruntime.1.2.dylib", "Contents/Resources/readme.txt"];
  h.deps.isMachO = (file) => !file.endsWith(".txt");
  return h;
}
const assertCfg = () => config({ assertTeamSignedNested: true, assert: [{ path: "Contents/Frameworks/libonnxruntime*.dylib", teamIdentifier: TEAM }, { path: ".", identifier: "x", authority: `Developer ID Application: Orthic Labs (${TEAM})` }] });

test("declared identity assertions pass on the installed copy, read with codesign -dvv", async () => {
  const h = withDvv(harness({ files: { [DEST]: "old" } }));
  const result = await installTauriDevId(opts(h, { config: assertCfg() }));
  assert.equal(result.installed, DEST);
  assert.ok(h.calls.includes(`run codesign -dvv ${DEST}`));
  assert.ok(h.calls.includes(`run codesign -dvv ${DEST}/Contents/Frameworks/libonnxruntime.1.2.dylib`));
  assert.equal(h.calls.some((line) => line.includes("readme.txt")), false, "non-Mach-O files are not inspected");
  assert.equal(h.fs.get(`${DEST}.previous`), "old");
});

test("a nested Mach-O signed by another team fails the install and restores .previous", async () => {
  const h = withDvv(harness({ files: { [DEST]: "old" } }), { [`${DEST}/Contents/Frameworks/libonnxruntime.1.2.dylib`]: "UBF8T346G9" });
  await assert.rejects(installTauriDevId(opts(h, { config: assertCfg() })), (error) => error.code === "TAURI_DEVID_VERIFY_FAILED" && error.rolledBack === "restored-previous" && /libonnxruntime\.1\.2\.dylib/.test(error.message));
  assert.equal(h.fs.get(DEST), "old");
  assert.equal(h.fs.has(`${DEST}.previous`), false);
});

test("a failed assertion on a first install removes it", async () => {
  const h = withDvv(harness(), { [DEST]: "OTHERTEAM" });
  await assert.rejects(installTauriDevId(opts(h, { config: config({ assert: [{ path: ".", teamIdentifier: TEAM }] }) })), (error) => error.code === "TAURI_DEVID_VERIFY_FAILED" && error.rolledBack === "removed-first-install");
  assert.equal(h.fs.has(DEST), false);
});

test("without declared assertions the assertion step is never called", async () => {
  const h = harness({ files: { [DEST]: "old" } });
  let called = 0;
  h.deps.assertIdentities = () => { called += 1; };
  await installTauriDevId(opts(h));
  assert.equal(called, 0);
  assert.equal(h.calls.some((line) => line.includes("-dvv")), false);
});

test("the descriptor accepts and validates the per-binary signing keys", () => {
  const ok = config({ identifiers: { "Contents/MacOS/demo-engine": "app.demo.engine" }, identifierTemplate: "<bundleId>.runtime.<relpath>", entitlements: { "Contents/MacOS/demo-engine": "src-tauri/entitlements.engine.plist" }, timestamp: true, assert: [{ path: "Contents/MacOS/demo-engine", identifier: "app.demo.engine" }], assertTeamSignedNested: true });
  assert.equal(resolveTauriDevIdDescriptor(ok).identifiers["Contents/MacOS/demo-engine"], "app.demo.engine");
  for (const extra of [{ identifiers: { "../x": "a" } }, { identifiers: { x: "-a" } }, { identifierTemplate: "<bundleId>" }, { timestamp: 1 }, { assert: [{ path: "." }] }, { assertTeamSignedNested: "yes" }]) {
    assert.throws(() => resolveTauriDevIdDescriptor(config(extra)), (error) => error.code === "TAURI_DEVID_CONFIG_INVALID", JSON.stringify(extra));
  }
});

test("the sign step receives the file listing hooks and the full config", async () => {
  const h = harness();
  let seen;
  h.deps.sign = async (options) => { seen = options; return { identity: "i", teamIdentifier: TEAM, designatedRequirement: REQ }; };
  h.deps.listFiles = () => [];
  await installTauriDevId(opts(h, { config: config({ timestamp: true }) }));
  assert.equal(seen.descriptor.timestamp, true);
  assert.equal(typeof seen.listFiles, "function");
});
