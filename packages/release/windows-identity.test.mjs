// The Windows identity producer: the planner (windows-identity-plan.mjs) with fixtures, and the executor (windows-identity.mjs) with
// doubles for makepri/makeappx/the signer/the SVG renderer. No Windows SDK, no signing, no credentials are touched.
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { buildWindowsIdentity, main, parseArgs } from "./windows-identity.mjs";
import { declaresWindowsIdentity, expectedPackageFiles, expectedPriUris, identityOutputProblems, planWindowsIdentity, stampManifest, toMsixVersion, windowsIdentitySteps } from "./windows-identity-plan.mjs";

const HR = {
  version: "0.1.33",
  windows: { identity: {
    manifest: "src-tauri/windows/identity/AppxManifest.xml", out: "src-tauri/windows/identity/dist", package: "HeardRight.Identity.msix",
    assets: {
      files: [{ from: "src-tauri/icons/StoreLogo.png", to: "HeardRightStoreLogo.png" }, { from: "src-tauri/icons/Square44x44Logo.png", to: "HeardRightSquare44x44Logo.png" }],
      scaled: { svg: "src-tauri/icons/heardright-icon.svg", sizes: [16, 48], name: "HeardRightSquare44x44Logo.targetsize-{size}_altform-unplated.png" },
    },
  } },
};
const MANIFEST = '<Package><Identity Name="DamnedVentures.HeardRight" Publisher="CN=Damned Ventures LLC, O=Damned Ventures LLC" Version="0.1.0.0" ProcessorArchitecture="neutral" /></Package>';
const clone = (v) => JSON.parse(JSON.stringify(v));

test("planner: the HeardRight declaration validates, derives the MSIX version from the config, and lists the package contents and PRI URIs", () => {
  const { problems, plan } = planWindowsIdentity(HR);
  assert.deepEqual(problems, []);
  assert.equal(plan.msixVersion, "0.1.33.0");
  assert.deepEqual(expectedPackageFiles(plan), ["AppxManifest.xml", "resources.pri", "Assets/HeardRightStoreLogo.png", "Assets/HeardRightSquare44x44Logo.png", "Assets/HeardRightSquare44x44Logo.targetsize-16_altform-unplated.png", "Assets/HeardRightSquare44x44Logo.targetsize-48_altform-unplated.png"]);
  assert.deepEqual(expectedPriUris(plan, "DamnedVentures.HeardRight"), ["ms-resource://DamnedVentures.HeardRight/Files/Assets/HeardRightStoreLogo.png", "ms-resource://DamnedVentures.HeardRight/Files/Assets/HeardRightSquare44x44Logo.png"]);
  assert.deepEqual(windowsIdentitySteps(plan).slice(-3), ["verify-pri", "sign", "cleanup"]);
  const noScaled = clone(HR); delete noScaled.windows.identity.assets.scaled;
  assert.ok(!windowsIdentitySteps(planWindowsIdentity(noScaled).plan).includes("render-scaled"));
});

test("planner: refusals name the key (escape, unknown keys, bad package, prerelease version, repeated or colliding assets)", () => {
  const bad = (mutate, re) => { const c = clone(HR); mutate(c); const r = planWindowsIdentity(c); assert.equal(r.plan, null); assert.match(r.problems.join("\n"), re); };
  bad((c) => { c.windows.identity.out = "../dist"; }, /out must be a relative path inside the app/);
  bad((c) => { c.windows.identity.manifest = "C:/x.xml"; }, /manifest must be a relative path/);
  bad((c) => { c.windows.identity.package = "a/b.msix"; }, /package must be a flat \.msix/);
  bad((c) => { c.windows.identity.sign = true; }, /unknown key sign/);
  bad((c) => { c.windows.other = {}; }, /windows has unknown key other/);
  bad((c) => { c.version = "1.2.3-beta.1"; }, /not 3 or 4 numeric segments/);
  bad((c) => { c.windows.identity.assets.files.push({ from: "x.png", to: "heardrightstorelogo.png" }); }, /repeats/);
  bad((c) => { c.windows.identity.assets.scaled.name = "x.png"; }, /assets\.scaled needs/);
  bad((c) => { c.windows.identity.assets.scaled.sizes = [0]; }, /assets\.scaled needs/);
  bad((c) => { c.windows.identity.assets.files = []; }, /at least one/);
  assert.deepEqual(planWindowsIdentity({}).problems, ["the config declares no windows.identity"]);
  assert.equal(toMsixVersion("1.2.3.4"), "1.2.3.4");
  assert.throws(() => toMsixVersion("70000.0.0"), /numeric segments/);
});

test("planner: the manifest's Identity/@Version alone is stamped; a manifest without Identity, Version or Name is refused", () => {
  const s = stampManifest(MANIFEST, "0.1.33.0");
  assert.equal(s.name, "DamnedVentures.HeardRight");
  assert.match(s.xml, /Version="0\.1\.33\.0"/);
  assert.match(s.xml, /Publisher="CN=Damned Ventures LLC, O=Damned Ventures LLC"/);
  assert.throws(() => stampManifest("<Package/>", "1.0.0.0"), /no <Identity/);
  assert.throws(() => stampManifest('<Identity Name="a" />', "1.0.0.0"), /no Version/);
  assert.throws(() => stampManifest('<Identity Version="1" />', "1.0.0.0"), /no Name/);
});

test("planner: config text detection (json and mjs forms) and the write attribution", () => {
  assert.equal(declaresWindowsIdentity(JSON.stringify(HR)), true);
  assert.equal(declaresWindowsIdentity("export default { windows: {\n  identity: { out: 'x' } } }"), true);
  assert.equal(declaresWindowsIdentity('{"web":{"build":[]}}'), false);
  const e = (p, h = "a") => ({ path: p, type: "file", size: 1, sha256: h, execBit: false });
  const out = "app/dist"; const declared = ["app/dist"];
  assert.deepEqual(identityOutputProblems({ before: [], after: [e("app/dist/P.msix"), e("app/dist/external-assets/Assets/a.png")], out, declared, packageFile: "P.msix" }), []);
  assert.match(identityOutputProblems({ before: [], after: [e("app/dist/P.msix"), e("app/other")], out, declared, packageFile: "P.msix" })[0], /app\/other was written .* outside its out app\/dist/);
  assert.match(identityOutputProblems({ before: [e("keep/x")], after: [e("app/dist/P.msix")], out, declared, packageFile: "P.msix" })[0], /keep\/x was removed/);
  assert.match(identityOutputProblems({ before: [], after: [e("app/dist/P.msix")], out: "app/dist", declared: ["elsewhere"], packageFile: "P.msix" })[0], /lies under no declared compile input/);
  assert.match(identityOutputProblems({ before: [], after: [], out, declared, packageFile: "P.msix" })[0], /was not produced/);
});

// ---- executor ---------------------------------------------------------------------------------------------------------------------------

function app(t) {
  const dir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-wid-")));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const w = (rel, body) => { fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true }); fs.writeFileSync(path.join(dir, rel), body); };
  w("src-tauri/windows/identity/AppxManifest.xml", MANIFEST);
  w("src-tauri/icons/StoreLogo.png", "store"); w("src-tauri/icons/Square44x44Logo.png", "sq44"); w("src-tauri/icons/heardright-icon.svg", "<svg/>");
  return dir;
}
/** makepri/makeappx doubles that write what the real tools write; `dumpUris` is what the PRI dump will contain. */
function tools(appRoot, { dumpUris = null, packFails = false, dropFromUnpack = null } = {}) {
  const calls = [];
  const fakeTool = (p) => ({ toolPath: p });
  const flag = (a, f) => a[a.indexOf(f) + 1];
  const run = (cmd, a) => {
    calls.push([path.basename(cmd), a[0]]);
    const tool = path.basename(cmd);
    if (tool === "makepri") {
      if (a[0] === "createconfig") fs.writeFileSync(flag(a, "/cf"), "<resources/>");
      if (a[0] === "new") fs.writeFileSync(flag(a, "/of"), "pri");
      if (a[0] === "dump") fs.writeFileSync(flag(a, "/of"), (dumpUris ?? ["ms-resource://DamnedVentures.HeardRight/Files/Assets/HeardRightStoreLogo.png", "ms-resource://DamnedVentures.HeardRight/Files/Assets/HeardRightSquare44x44Logo.png"]).join("\n"));
    } else if (a[0] === "pack") {
      if (packFails) return { status: 3, stderr: "bad manifest" };
      fs.writeFileSync(flag(a, "/p"), JSON.stringify({ dir: flag(a, "/d") }));
    } else if (a[0] === "unpack") {
      const src = JSON.parse(fs.readFileSync(flag(a, "/p"), "utf8")).dir; const dest = flag(a, "/d");
      fs.cpSync(src, dest, { recursive: true });
      if (dropFromUnpack) fs.rmSync(path.join(dest, dropFromUnpack));
    }
    return { status: 0, stdout: "", stderr: "" };
  };
  return { calls, run, findTool: (n) => fakeTool(path.join(appRoot, n)), env: { MAKEAPPX_PATH: path.join(appRoot, "makeappx"), MAKEPRI_PATH: path.join(appRoot, "makepri") } };
}
const withTools = (appRoot) => { fs.writeFileSync(path.join(appRoot, "makeappx"), ""); fs.writeFileSync(path.join(appRoot, "makepri"), ""); };
const listOut = (appRoot) => fs.readdirSync(path.join(appRoot, HR.windows.identity.out), { recursive: true }).map((x) => String(x).split(path.sep).join("/")).sort();

test("executor: on Windows it stages, renders, builds the PRI, packs with /nv, round-trips, checks the PRI, signs the package, and leaves only the package and external-assets", (t) => {
  const root = app(t); withTools(root);
  const { plan } = planWindowsIdentity(HR);
  const x = tools(root); const signed = []; const rendered = [];
  const r = buildWindowsIdentity({ plan, appRoot: root }, { platform: "win32", ...x, render: (svg, size) => { rendered.push(size); return Buffer.from(`png${size}`); }, sign: (f) => signed.push(path.relative(root, f).split(path.sep).join("/")), log: () => {} });
  assert.equal(r.skipped, null);
  assert.deepEqual(x.calls.map((c) => c.join(" ")), ["makepri createconfig", "makepri new", "makeappx pack", "makeappx unpack", "makepri dump"]);
  assert.deepEqual(rendered, [16, 48]);
  assert.deepEqual(signed, ["src-tauri/windows/identity/dist/HeardRight.Identity.msix"], "the package is signed once, after the round-trip and PRI checks");
  assert.deepEqual(listOut(root), ["HeardRight.Identity.msix", "external-assets", "external-assets/Assets", "external-assets/Assets/HeardRightSquare44x44Logo.png", "external-assets/Assets/HeardRightSquare44x44Logo.targetsize-16_altform-unplated.png", "external-assets/Assets/HeardRightSquare44x44Logo.targetsize-48_altform-unplated.png", "external-assets/Assets/HeardRightStoreLogo.png"]);
  assert.deepEqual(r.files.slice(0, 1), ["src-tauri/windows/identity/dist/HeardRight.Identity.msix"]);
  assert.ok(!fs.existsSync(path.join(root, "src-tauri/windows/identity/dist/.identity-work")), "scratch removed");
  assert.equal(x.calls.some((c) => c[1] === "pack"), true);
});

test("executor: a non-Windows host or an existing package builds nothing; every failure stops before signing and cleans its scratch", (t) => {
  const root = app(t); withTools(root);
  const { plan } = planWindowsIdentity(HR);
  const base = (over, extra = {}) => ({ platform: "win32", ...tools(root, extra), render: () => Buffer.from("p"), sign: () => assert.fail("must not sign"), log: () => {}, ...over });
  assert.equal(buildWindowsIdentity({ plan, appRoot: root }, base({ platform: "darwin" })).skipped, "not-windows");
  assert.ok(!fs.existsSync(path.join(root, plan.out)));
  assert.throws(() => buildWindowsIdentity({ plan, appRoot: root }, base({}, { packFails: true })), /makeappx pack failed .* bad manifest/);
  assert.ok(!fs.existsSync(path.join(root, plan.out, ".identity-work")));
  assert.throws(() => buildWindowsIdentity({ plan, appRoot: root }, base({}, { dumpUris: ["ms-resource://DamnedVentures.HeardRight/Files/Assets/HeardRightStoreLogo.png"] })), /does not index: .*Square44x44Logo\.png/);
  assert.throws(() => buildWindowsIdentity({ plan, appRoot: root }, base({}, { dropFromUnpack: "resources.pri" })), /round-trip unpack .* missing: resources\.pri/);
  assert.throws(() => buildWindowsIdentity({ plan, appRoot: root }, base({ env: {}, findTool: () => null })), /makeappx\.exe not found/);
  fs.rmSync(path.join(root, "src-tauri/icons/StoreLogo.png"));
  assert.throws(() => buildWindowsIdentity({ plan, appRoot: root }, base({})), /missing asset src-tauri\/icons\/StoreLogo\.png/);
  fs.mkdirSync(path.join(root, plan.out), { recursive: true }); fs.writeFileSync(path.join(root, plan.out, plan.package), "x");
  assert.equal(buildWindowsIdentity({ plan, appRoot: root }, base({})).skipped, "present");
});

test("main: the door is checked before the config is loaded; a non-Windows host reports a skip; arguments are strict", async (t) => {
  const root = app(t);
  const cfg = path.join(root, "right-release.config.mjs");
  let loaded = 0;
  const loadConfig = async () => { loaded += 1; return { config: HR }; };
  const git = (status) => () => ({ status, stdout: "", stderr: "" });
  await assert.rejects(main(["--config", cfg], { loadConfig, spawnSyncFn: git(128) }), /windows-identity runs only inside a git repository, under a door run/);
  assert.equal(loaded, 0, "the config (code) is never imported without a door run");
  assert.deepEqual(parseArgs(["--config", "a", "--json"]), { config: "a", json: true, dryRun: false });
  assert.throws(() => parseArgs([]), /--config/);
  assert.throws(() => parseArgs(["--config", "a", "--x"]), /unknown/);
});
