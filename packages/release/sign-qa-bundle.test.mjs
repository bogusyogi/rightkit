import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { QaBundleSigningError, adhocCodesignArgs, signQaBundleAdhoc } from "./sign-qa-bundle.mjs";

const HOME = "/Users/qa";
const ROOT = "/Volumes/D/scratch/qa";
const APP = `${ROOT}/Demo.app`;
const FW = `${APP}/Contents/Frameworks/Core.framework`;

// In-memory macOS tree: path -> "dir" | "file" | { link: target }.
function fakeFs(extra = {}) {
  const agent = `${APP}/Contents/Library/LoginItems/Agent.app`;
  const svc = `${APP}/Contents/PlugIns/Svc.xpc`;
  const nodes = new Map(Object.entries({
    "/": "dir", "/Volumes": "dir", "/Volumes/D": "dir", "/Volumes/D/scratch": "dir", [ROOT]: "dir",
    "/Applications": "dir", "/Applications/Real.app": "dir", [HOME]: "dir", [`${HOME}/Applications`]: "dir", [`${HOME}/Applications/Mine.app`]: "dir",
    "/Users": "dir", "/elsewhere": "dir", "/elsewhere/Out.app": "dir", [`${ROOT}/NotDir.app`]: "file", [`${ROOT}/plain`]: "dir",
    [APP]: "dir", [`${APP}/Contents`]: "dir", [`${APP}/Contents/Info.plist`]: "file",
    [`${APP}/Contents/MacOS`]: "dir", [`${APP}/Contents/MacOS/Demo`]: "file", [`${APP}/Contents/MacOS/helper`]: "file",
    [`${APP}/Contents/Frameworks`]: "dir", [`${APP}/Contents/Frameworks/libextra.dylib`]: "file",
    [FW]: "dir", [`${FW}/Versions`]: "dir", [`${FW}/Versions/A`]: "dir", [`${FW}/Versions/A/Core`]: "file",
    [`${FW}/Versions/A/libdeep.dylib`]: "file", [`${FW}/Versions/Current`]: { link: `${FW}/Versions/A` },
    [`${APP}/Contents/PlugIns`]: "dir", [svc]: "dir", [`${svc}/Contents`]: "dir", [`${svc}/Contents/Info.plist`]: "file",
    [`${svc}/Contents/MacOS`]: "dir", [`${svc}/Contents/MacOS/Svc`]: "file",
    [`${APP}/Contents/Library`]: "dir", [`${APP}/Contents/Library/LoginItems`]: "dir", [agent]: "dir",
    [`${agent}/Contents`]: "dir", [`${agent}/Contents/Info.plist`]: "file", [`${agent}/Contents/MacOS`]: "dir", [`${agent}/Contents/MacOS/Agent`]: "file",
    ...extra,
  }));
  const resolve = (file) => {
    let cur = "";
    for (const part of file.split("/").filter(Boolean)) {
      cur = `${cur}/${part}`;
      const node = nodes.get(cur);
      if (typeof node === "object") cur = resolve(node.link);
    }
    const out = cur || "/";
    if (!nodes.has(out)) throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" });
    return out;
  };
  const stat = (file) => {
    const node = nodes.get(file);
    if (node === undefined) throw Object.assign(new Error(`ENOENT ${file}`), { code: "ENOENT" });
    return { isDirectory: () => node === "dir", isFile: () => node === "file", isSymbolicLink: () => typeof node === "object" };
  };
  const written = new Map();
  const removed = [];
  return {
    written, removed,
    lstatSync: (file) => {
      const cut = file.lastIndexOf("/");
      return stat(cut > 0 ? `${resolve(file.slice(0, cut))}${file.slice(cut)}` : file);
    },
    existsSync: (file) => nodes.has(file) || written.has(file),
    realpathSync: resolve,
    readdirSync: (dir) => [...nodes.keys()]
      .filter((key) => key !== dir && key.startsWith(`${dir}/`) && !key.slice(dir.length + 1).includes("/"))
      .map((key) => ({ name: key.slice(dir.length + 1), ...stat(key) })),
    mkdtempSync: (prefix) => { nodes.set(`${prefix}XYZ`, "dir"); return `${prefix}XYZ`; },
    writeFileSync: (file, data) => { written.set(file, String(data)); },
    rmSync: (file) => { removed.push(file); },
  };
}

function fakeRun(entitlements = {}) {
  const calls = [];
  const run = (command, args) => {
    calls.push({ command, args });
    if (command === "/usr/bin/plutil" && args[1] === "json") {
      const file = args[4];
      if (file.endsWith("/Contents/Info.plist")) {
        const bundle = file.slice(0, -"/Contents/Info.plist".length).split("/").at(-1);
        return { status: 0, stdout: JSON.stringify({ CFBundleExecutable: bundle.replace(/\.(app|xpc)$/, "") }), stderr: "" };
      }
      return { status: 0, stdout: JSON.stringify(entitlements), stderr: "" };
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  return { run, calls };
}
const forceCalls = (calls) => calls.filter((c) => c.command === "/usr/bin/codesign" && c.args[0] === "--force");
const base = (extra = {}) => ({ app: APP, qaScratchRoot: ROOT, platform: "darwin", home: HOME, ...extra });

test("signs nested code inside-out with the outer bundle last, never --deep, every identity '-'", () => {
  const fs = fakeFs();
  const { run, calls } = fakeRun();
  const receipt = signQaBundleAdhoc(base({ run, io: fs }));
  const signs = forceCalls(calls);
  const order = signs.map((c) => c.args.at(-1));
  assert.equal(order.at(-1), APP);
  const depth = (file) => file.split("/").length;
  const nested = order.slice(0, -1);
  assert.deepEqual(nested.map(depth), [...nested.map(depth)].sort((a, b) => b - a));
  assert.deepEqual(new Set(nested), new Set([
    `${APP}/Contents/MacOS/helper`, `${APP}/Contents/Frameworks/libextra.dylib`, FW, `${FW}/Versions/A/libdeep.dylib`,
    `${APP}/Contents/PlugIns/Svc.xpc`, `${APP}/Contents/Library/LoginItems/Agent.app`,
  ]));
  assert.ok(order.indexOf(`${FW}/Versions/A/libdeep.dylib`) < order.indexOf(FW), "dylib before its framework");
  assert.ok(!order.includes(`${APP}/Contents/MacOS/Demo`), "main executable is covered by the bundle signature");
  for (const { args } of signs) {
    assert.deepEqual(args.slice(0, 3), ["--force", "--sign", "-"]);
    assert.ok(!args.includes("--deep"));
    assert.equal(args.filter((a) => a === "--sign").length, 1);
  }
  assert.ok(!JSON.stringify(calls).includes("--deep"));
  assert.deepEqual(calls.filter((c) => c.command === "/usr/bin/codesign").at(-1).args, ["--verify", "--strict", APP]);
  assert.deepEqual(receipt.signed.map((item) => item.path), order);
  assert.equal(receipt.signed.at(-1).kind, "bundle");
});

test("the signing agent is never touched; codesign is spawned directly via the injected run", () => {
  const source = readFileSync(new URL("./sign-qa-bundle.mjs", import.meta.url), "utf8").replace(/^\s*\/\/.*$/gm, "");
  assert.doesNotMatch(source, /requestSigningAgent|macos-signing-session|signing-agent|runMacCodesignCommands|runMacGuiSigning/);
  const { run, calls } = fakeRun();
  signQaBundleAdhoc(base({ run, io: fakeFs() }));
  assert.ok(calls.every((c) => c.command === "/usr/bin/codesign" || c.command === "/usr/bin/plutil"));
});

test("receipt: adhoc, releaseArtifact false, qaOnly true; optional evidence file", () => {
  const fs = fakeFs();
  const { run } = fakeRun();
  const receipt = signQaBundleAdhoc(base({ run, io: fs, out: "/Volumes/D/scratch/evidence.json" }));
  assert.equal(receipt.signature, "adhoc");
  assert.equal(receipt.releaseArtifact, false);
  assert.equal(receipt.qaOnly, true);
  assert.equal(receipt.notarized, false);
  assert.equal(receipt.usedSigningAgent, false);
  assert.deepEqual(JSON.parse(fs.written.get("/Volumes/D/scratch/evidence.json")), receipt);
});

test("entitlements are reduced to ad hoc-safe keys and applied to the outer bundle only", () => {
  const fs = fakeFs();
  const { run, calls } = fakeRun({
    "com.apple.security.cs.allow-jit": true, "com.apple.security.device.audio-input": true,
    "com.apple.application-identifier": "T.x", "keychain-access-groups": ["T.x"], "com.apple.developer.icloud-services": ["CloudKit"],
  });
  const receipt = signQaBundleAdhoc(base({ run, io: fs, entitlements: "/Volumes/D/scratch/app.entitlements" }));
  assert.deepEqual(receipt.entitlementKeys, ["com.apple.security.cs.allow-jit", "com.apple.security.device.audio-input"]);
  const withEnt = forceCalls(calls).filter((c) => c.args.includes("--entitlements"));
  assert.equal(withEnt.length, 1);
  assert.equal(withEnt[0].args.at(-1), APP);
  const [file] = [...fs.written.keys()];
  assert.ok(file.startsWith(`${ROOT}/.sign-qa-`));
  assert.deepEqual(JSON.parse(fs.written.get(file)), { "com.apple.security.cs.allow-jit": true, "com.apple.security.device.audio-input": true });
  assert.ok(fs.removed.some((dir) => dir.startsWith(`${ROOT}/.sign-qa-`)), "scratch plist removed");
});

test("refusals happen before any command runs", () => {
  const cases = [
    ["non-darwin", base({ platform: "win32" }), "QA_SIGN_DARWIN_ONLY"],
    ["linux", base({ platform: "linux" }), "QA_SIGN_DARWIN_ONLY"],
    ["relative app", base({ app: "Demo.app" }), "QA_SIGN_PATH_NOT_ABSOLUTE"],
    ["relative root", base({ qaScratchRoot: "qa" }), "QA_SIGN_PATH_NOT_ABSOLUTE"],
    ["dot-dot", base({ app: `${ROOT}/../qa/Demo.app` }), "QA_SIGN_PATH_NOT_NORMAL"],
    ["/Applications", base({ app: "/Applications/Real.app", qaScratchRoot: "/Applications" }), "QA_SIGN_APPLICATIONS_REFUSED"],
    ["~/Applications", base({ app: `${HOME}/Applications/Mine.app`, qaScratchRoot: `${HOME}/Applications` }), "QA_SIGN_APPLICATIONS_REFUSED"],
    ["outside the QA root", base({ app: "/elsewhere/Out.app" }), "QA_SIGN_OUTSIDE_QA_ROOT"],
    ["not a .app", base({ app: `${ROOT}/plain` }), "QA_SIGN_NOT_APP"],
    ["a .app that is a file", base({ app: `${ROOT}/NotDir.app` }), "QA_SIGN_NOT_APP"],
    ["root is /", base({ qaScratchRoot: "/" }), "QA_SIGN_ROOT_REFUSED"],
    ["root is home", base({ qaScratchRoot: HOME }), "QA_SIGN_ROOT_REFUSED"],
  ];
  for (const [label, options, code] of cases) {
    const { run, calls } = fakeRun();
    assert.throws(() => signQaBundleAdhoc({ ...options, run, io: fakeFs() }), (error) => error instanceof QaBundleSigningError && error.code === code, label);
    assert.equal(calls.length, 0, `${label}: no command may run`);
  }
});

test("links that escape the QA root, the install locations or the bundle are refused", () => {
  let { run, calls } = fakeRun();
  assert.throws(() => signQaBundleAdhoc({ ...base({ app: `${ROOT}/Linked.app` }), run, io: fakeFs({ [`${ROOT}/Linked.app`]: { link: "/elsewhere/Out.app" } }) }), { code: "QA_SIGN_LINK_REFUSED" });
  assert.throws(() => signQaBundleAdhoc({ ...base({ app: `${ROOT}/escape/Real.app` }), run, io: fakeFs({ [`${ROOT}/escape`]: { link: "/Applications" } }) }), { code: "QA_SIGN_APPLICATIONS_REFUSED" });
  assert.throws(() => signQaBundleAdhoc({ ...base({ app: `${ROOT}/rootlink/Out.app` }), run, io: fakeFs({ [`${ROOT}/rootlink`]: { link: "/elsewhere" } }) }), { code: "QA_SIGN_OUTSIDE_QA_ROOT" });
  ({ run, calls } = fakeRun());
  assert.throws(() => signQaBundleAdhoc({ ...base(), run, io: fakeFs({ [`${APP}/Contents/Frameworks/evil.dylib`]: { link: "/elsewhere/Out.app" } }) }), { code: "QA_SIGN_LINK_REFUSED" });
  assert.equal(forceCalls(calls).length, 0, "nothing signed when an inner link escapes");
});

test("a failing codesign stops the run and no verification is claimed", () => {
  const real = fakeRun();
  const run = (command, args, options) => (command === "/usr/bin/codesign" && args[0] === "--force" && args.at(-1) === APP ? { status: 1, stderr: "boom" } : real.run(command, args, options));
  assert.throws(() => signQaBundleAdhoc(base({ run, io: fakeFs() })), { code: "QA_SIGN_CODESIGN_FAILED" });
  assert.ok(!real.calls.some((c) => c.args[0] === "--verify"));
});

test("argv builder cannot emit --deep or a real identity", () => {
  assert.deepEqual(adhocCodesignArgs("/x/y.app"), ["--force", "--sign", "-", "/x/y.app"]);
  assert.deepEqual(adhocCodesignArgs("/x/y.app", "/x/e.plist"), ["--force", "--sign", "-", "--entitlements", "/x/e.plist", "/x/y.app"]);
});
