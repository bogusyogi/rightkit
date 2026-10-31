// RA-18 / L9-03: the host's own record of the scan tools the gate trusts (scan-tool-enrolment.mjs), the broker-pins.test.mjs pattern. The record is the
// operator's explicit enrolment, checked against the file's bytes at enrol time; nothing in the environment, the checked repository or PATH supplies a tool;
// the RightKit fallback is the running gate's own revision of ci/rk-pipeline.json; a tool nobody enrolled is not in the policy.
import "./test-support/broker-guard.mjs";
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { TRUSTED_SCAN_TOOLS, resolveTrustedScanTool, scanToolPolicyFromPipeline } from "./deterministic-scans.mjs";
import {
  SCAN_TOOLS_SCHEMA, SCAN_TOOL_NAMES, SCAN_TOOL_PLATFORMS, ScanToolEnrolmentError, enrollScanTool, loadScanToolPolicy, main, readEnrolledScanTools, readGateRevisionScanTools, scanToolsFile, validateScanTool,
} from "./scan-tool-enrolment.mjs";

const POSIX = process.platform === "win32" ? "POSIX file modes" : false;
const tmp = (t, prefix = "rk-scan-tools-") => { const d = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), prefix))); t.after(() => fs.rmSync(d, { recursive: true, force: true })); return d; };
const git = (cwd, ...args) => spawnSync("git", args, { cwd, encoding: "utf8" });
const sha = (bytes) => crypto.createHash("sha256").update(bytes).digest("hex");
const HOST = process.platform;

/** A real executable file standing in the analyzer's place, and its enrolment values. */
function tool(t, name = "shellcheck", body = `#!/bin/sh\necho "${name}"\n`) {
  const dir = tmp(t, "rk-scan-bin-"); const file = path.join(dir, name);
  fs.writeFileSync(file, body); fs.chmodSync(file, 0o755);
  return { path: fs.realpathSync(file), sha256: sha(body), version: "0.11.0" };
}

test("the trusted tool names are exactly the scan engine's (a tool one side adds and the other lacks would be enrolled for nothing or trusted from nowhere)", () => {
  assert.deepEqual([...SCAN_TOOL_NAMES].sort(), [...TRUSTED_SCAN_TOOLS].sort());
  assert.deepEqual([...SCAN_TOOL_PLATFORMS].sort(), ["darwin", "linux", "win32"]);
});

test("enroll writes one owner-only record per platform under the managed data root; a second enrollment of a tool is a rotation and must say so; tools accumulate", (t) => {
  const dataRoot = tmp(t); const sc = tool(t, "shellcheck"); const deny = tool(t, "cargo-deny"); const other = tool(t, "shellcheck", "#!/bin/sh\necho other\n");
  assert.equal(scanToolsFile({ dataRoot, platform: "linux" }), path.join(dataRoot, "scan-tools", "linux.json"));
  assert.equal(readEnrolledScanTools({ dataRoot, platform: HOST }), null, "nothing is enrolled until someone enrolls");
  const done = enrollScanTool({ dataRoot, platform: HOST, tool: "shellcheck", ...sc, now: () => new Date("2026-10-31T00:00:00Z") });
  assert.equal(done.rotated, false); assert.equal(done.record.schema, SCAN_TOOLS_SCHEMA); assert.equal(done.record.platform, HOST); assert.equal(done.record.enrolledAt, "2026-10-31T00:00:00.000Z");
  if (!POSIX) { assert.equal(fs.statSync(done.file).mode & 0o777, 0o600); assert.equal(fs.statSync(path.dirname(done.file)).mode & 0o777, 0o700); }
  enrollScanTool({ dataRoot, platform: HOST, tool: "cargo-deny", ...deny });
  const read = readEnrolledScanTools({ dataRoot, platform: HOST });
  assert.equal(read.source, "enrolled"); assert.deepEqual(read.tools, { shellcheck: sc, "cargo-deny": deny }, "the second tool joined the record, the first is unchanged");
  // No silent replacement, and a failed one changes nothing.
  assert.throws(() => enrollScanTool({ dataRoot, platform: HOST, tool: "shellcheck", ...other }), (e) => e instanceof ScanToolEnrolmentError && e.code === "RK_SCAN_TOOLS_EXISTS");
  assert.deepEqual(readEnrolledScanTools({ dataRoot, platform: HOST }).tools.shellcheck, sc);
  const rotated = enrollScanTool({ dataRoot, platform: HOST, tool: "shellcheck", ...other, rotate: true });
  assert.equal(rotated.rotated, true); assert.deepEqual(readEnrolledScanTools({ dataRoot, platform: HOST }).tools, { shellcheck: other, "cargo-deny": deny });
});

test("enrolment checks the claim against the file: a digest that is not the file's, a missing, non-regular or non-executable file, and every malformed value are refused and write nothing", { skip: POSIX }, (t) => {
  const dataRoot = tmp(t); const sc = tool(t, "shellcheck");
  const file = scanToolsFile({ dataRoot, platform: HOST });
  const refused = (over, code, why) => {
    assert.throws(() => enrollScanTool({ dataRoot, platform: HOST, tool: "shellcheck", ...sc, ...over }), (e) => e.code === code && why.test(e.message), JSON.stringify(over));
    assert.equal(fs.existsSync(file), false, "nothing was written");
  };
  refused({ sha256: "0".repeat(64) }, "RK_SCAN_TOOLS_DIGEST", /not the 0{64} being enrolled/);
  refused({ path: path.join(path.dirname(sc.path), "absent") }, "RK_SCAN_TOOLS_INVALID", /cannot be resolved/);
  refused({ path: path.dirname(sc.path) }, "RK_SCAN_TOOLS_INVALID", /not a regular file/);
  const plain = path.join(path.dirname(sc.path), "plain"); fs.writeFileSync(plain, "x"); fs.chmodSync(plain, 0o644);
  refused({ path: plain, sha256: sha("x") }, "RK_SCAN_TOOLS_INVALID", /not executable/);
  refused({ path: "relative/shellcheck" }, "RK_SCAN_TOOLS_INVALID", /absolute path without '\.\.'/);
  refused({ path: `${path.dirname(sc.path)}/../${path.basename(path.dirname(sc.path))}/shellcheck` }, "RK_SCAN_TOOLS_INVALID", /absolute path without '\.\.'/);
  refused({ sha256: sc.sha256.toUpperCase() }, "RK_SCAN_TOOLS_INVALID", /lowercase hex/);
  refused({ sha256: "abc" }, "RK_SCAN_TOOLS_INVALID", /lowercase hex/);
  refused({ version: " " }, "RK_SCAN_TOOLS_INVALID", /version is missing/);
  refused({ version: undefined }, "RK_SCAN_TOOLS_INVALID", /version is missing/);
  refused({ tool: "rustfmt" }, "RK_SCAN_TOOLS_INVALID", /not a trusted scan tool/);
  assert.throws(() => validateScanTool("shellcheck", null), /not an object/);
});

test("platforms: only darwin, win32 and linux have a record; another platform's tools cannot be enrolled from this host; the data root must be absolute", (t) => {
  const dataRoot = tmp(t); const sc = tool(t, "shellcheck");
  for (const platform of ["freebsd", "aix", ""]) assert.throws(() => scanToolsFile({ dataRoot, platform }), (e) => e.code === "RK_SCAN_TOOLS_PLATFORM" && /not a scan-tool host/.test(e.message), platform);
  assert.throws(() => scanToolsFile({ dataRoot: "relative", platform: "linux" }), (e) => e.code === "RK_SCAN_TOOLS_ROOT");
  const foreign = SCAN_TOOL_PLATFORMS.find((p) => p !== HOST);
  assert.throws(() => enrollScanTool({ dataRoot, platform: foreign, tool: "shellcheck", ...sc }), (e) => e.code === "RK_SCAN_TOOLS_PLATFORM" && /cannot be checked from here/.test(e.message));
  assert.equal(fs.existsSync(path.join(dataRoot, "scan-tools")), false);
  // A platform's absolute path rule is its own: a Windows drive path is not absolute on POSIX.
  assert.throws(() => validateScanTool("shellcheck", { path: "C:\\tools\\shellcheck.exe", sha256: "a".repeat(64), version: "0.11.0" }, { platform: "linux" }), /absolute path/);
  assert.deepEqual(validateScanTool("shellcheck", { path: "C:\\tools\\shellcheck.exe", sha256: "a".repeat(64), version: "0.11.0" }, { platform: "win32" }).path, "C:\\tools\\shellcheck.exe");
});

test("a record that is a link, writable by others, another platform's, not JSON, not this schema, naming an unknown tool or holding a malformed entry is refused, never read as missing", { skip: POSIX }, (t) => {
  const dataRoot = tmp(t); const sc = tool(t, "shellcheck");
  const { file } = enrollScanTool({ dataRoot, platform: HOST, tool: "shellcheck", ...sc });
  const refuse = (why) => assert.throws(() => readEnrolledScanTools({ dataRoot, platform: HOST }), (e) => e.code === "RK_SCAN_TOOLS_INVALID" && why.test(e.message));
  fs.chmodSync(file, 0o666); refuse(/writable by group or others/); fs.chmodSync(file, 0o600);
  fs.chmodSync(path.dirname(file), 0o777); refuse(/writable by group or others/); fs.chmodSync(path.dirname(file), 0o700);
  const text = fs.readFileSync(file, "utf8"); const record = JSON.parse(text);
  fs.writeFileSync(file, "not json"); refuse(/not JSON/);
  fs.writeFileSync(file, JSON.stringify({ ...record, schema: "other" })); refuse(/is not a rightkit\.scan-tools\/1 record/);
  const other = SCAN_TOOL_PLATFORMS.find((p) => p !== HOST);
  fs.writeFileSync(file, JSON.stringify({ ...record, platform: other })); refuse(new RegExp(`record of ${other}, not ${HOST}`));
  fs.writeFileSync(file, JSON.stringify({ ...record, tools: null })); refuse(/no tools object/);
  fs.writeFileSync(file, JSON.stringify({ ...record, tools: { rustfmt: record.tools.shellcheck } })); refuse(/not a trusted scan tool/);
  fs.writeFileSync(file, JSON.stringify({ ...record, tools: { shellcheck: { ...record.tools.shellcheck, sha256: "1" } } })); refuse(/sha256 is not a sha256/);
  fs.rmSync(file); const real = path.join(dataRoot, "real.json"); fs.writeFileSync(real, text, { mode: 0o600 }); fs.symlinkSync(real, file); refuse(/not a regular file/);
  fs.rmSync(file); fs.rmSync(path.join(dataRoot, "scan-tools"), { recursive: true });
  fs.mkdirSync(path.join(dataRoot, "elsewhere", "d"), { recursive: true }); fs.symlinkSync(path.join(dataRoot, "elsewhere", "d"), path.join(dataRoot, "scan-tools"));
  assert.equal(readEnrolledScanTools({ dataRoot, platform: HOST }), null, "no record behind a linked directory");
  fs.writeFileSync(path.join(dataRoot, "elsewhere", "d", `${HOST}.json`), text, { mode: 0o600 });
  assert.throws(() => readEnrolledScanTools({ dataRoot, platform: HOST }), /not a real directory/, "a record behind a linked directory is refused");
});

/** A fake RightKit checkout whose gate lives in it, with ci/rk-pipeline.json committed. */
function rightkitRepo(t, tools) {
  const root = tmp(t);
  fs.mkdirSync(path.join(root, "ci")); fs.mkdirSync(path.join(root, "packages", "release", "bin"), { recursive: true });
  fs.writeFileSync(path.join(root, "ci", "rk-pipeline.json"), JSON.stringify({ schema: "rk-pipeline.v1", ...(tools ? { tools } : {}) }));
  const gateFile = path.join(root, "packages", "release", "bin", "rightkit-gate.mjs"); fs.writeFileSync(gateFile, "// gate\n");
  git(root, "init", "-q", "-b", "main"); git(root, "add", "-A"); git(root, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "c");
  return { root, gateFile };
}
const entry = (platform, e) => ({ [platform]: e });
const load = (args) => loadScanToolPolicy({ fromPipeline: scanToolPolicyFromPipeline, platform: HOST, ...args });

test("fallback: RightKit's own tools come from the running gate's revision of ci/rk-pipeline.json, never the working tree, and only in the RightKit checkout; the enrolled record beats it per tool", (t) => {
  const dataRoot = tmp(t); const sc = tool(t, "shellcheck"); const enrolledSc = tool(t, "shellcheck", "#!/bin/sh\necho enrolled\n");
  const pipelineTools = { shellcheck: entry(HOST, sc), actionlint: "1.7.12", "cargo-deny": { version: "0.20.2", ...entry(HOST, { path: tool(t, "cargo-deny").path, sha256: tool(t, "cargo-deny").sha256 }) } };
  const { root, gateFile } = rightkitRepo(t, pipelineTools);
  const fromGate = load({ dataRoot, gateFile, checkout: root });
  assert.equal(fromGate.sources.shellcheck, "rk-pipeline"); assert.deepEqual(fromGate.policy.shellcheck, sc); assert.match(fromGate.revision, /^[0-9a-f]{40}$/);
  assert.equal(fromGate.policy["cargo-deny"].version, "0.20.2", "a tool-level version applies to its platform entry");
  assert.equal(Object.hasOwn(fromGate.policy, "actionlint"), false, "a bare version string enrols no executable");
  // The working tree changes nothing: the policy is the gate's committed revision.
  fs.writeFileSync(path.join(root, "ci", "rk-pipeline.json"), JSON.stringify({ schema: "rk-pipeline.v1", tools: { shellcheck: entry(HOST, { ...sc, path: "/evil/shellcheck" }) } }));
  assert.deepEqual(load({ dataRoot, gateFile, checkout: root }).policy.shellcheck, sc, "an edit of ci/rk-pipeline.json in the working tree enrols nothing");
  // Another repository (an app) never gets the fallback, whatever its own ci/rk-pipeline.json says; a gate outside any checkout has none.
  const app = rightkitRepo(t, { shellcheck: entry(HOST, { ...sc, path: "/evil/shellcheck" }) });
  assert.deepEqual(load({ dataRoot, gateFile, checkout: app.root }).policy, {});
  const loose = tmp(t); fs.writeFileSync(path.join(loose, "rightkit-gate.mjs"), "//\n");
  assert.deepEqual(load({ dataRoot, gateFile: path.join(loose, "rightkit-gate.mjs"), checkout: root }).policy, {});
  assert.equal(readGateRevisionScanTools({ gateFile, checkout: app.root }), null);
  // The enrolled record wins for the tool it names and leaves the fallback's others standing.
  enrollScanTool({ dataRoot, platform: HOST, tool: "shellcheck", ...enrolledSc });
  const merged = load({ dataRoot, gateFile, checkout: root });
  assert.equal(merged.sources.shellcheck, "enrolled"); assert.deepEqual(merged.policy.shellcheck, enrolledSc);
  assert.equal(merged.sources["cargo-deny"], "rk-pipeline", "the fallback still fills the tools the enrolled record does not name");
  assert.equal(merged.enrolledFile, scanToolsFile({ dataRoot, platform: HOST }));
});

test("linux carries no fallback unless ci/rk-pipeline.json carries tools.<name>.linux; the file's darwin and win32 entries never answer for it; an enrolled linux record decides alone", { skip: HOST !== "linux" && "the enrol operation checks the file's bytes on its own platform" }, (t) => {
  const dataRoot = tmp(t); const sc = tool(t, "shellcheck");
  const { root, gateFile } = rightkitRepo(t, { shellcheck: { darwin: { path: "/Volumes/D/tools/shellcheck", sha256: "a".repeat(64), version: "0.11.0" }, win32: { path: "D:\\tools\\shellcheck.exe", sha256: "b".repeat(64), version: "0.11.0" } } });
  assert.deepEqual(load({ dataRoot, platform: "linux", gateFile, checkout: root }).policy, {}, "no linux entry: nothing is trusted, TOOL_UNTRUSTED where a scan needs a tool");
  enrollScanTool({ dataRoot, platform: "linux", tool: "shellcheck", ...sc });
  const alone = load({ dataRoot, platform: "linux", gateFile, checkout: root });
  assert.deepEqual(alone.policy, { shellcheck: sc }); assert.deepEqual(alone.sources, { shellcheck: "enrolled" });
  // The loader's answer is what the scan engine judges: the enrolled file resolves as trusted, and a tool nobody enrolled does not.
  assert.equal(resolveTrustedScanTool("shellcheck", { policy: alone.policy, platform: "linux" }).ok, true);
  assert.equal(resolveTrustedScanTool("cargo-deny", { policy: alone.policy, platform: "linux" }).code, "TOOL_UNTRUSTED");
  // An unsupported platform has nothing to read: an empty policy, never a refusal of the run.
  assert.deepEqual(load({ dataRoot, platform: "freebsd" }), { policy: {}, sources: {} });
});

test("the loader reads nothing from the environment, the checked repository or PATH: an environment variable naming a tool changes no policy", (t) => {
  const dataRoot = tmp(t); const sc = tool(t, "shellcheck");
  enrollScanTool({ dataRoot, platform: HOST, tool: "shellcheck", ...sc });
  const before = load({ dataRoot });
  const names = ["RIGHTKIT_SCAN_TOOLS", "RIGHTKIT_SHELLCHECK", "SHELLCHECK_PATH", "RUSTRIGHT_SCAN_TOOL_POLICY"];
  const saved = Object.fromEntries(names.map((n) => [n, process.env[n]]));
  t.after(() => { for (const n of names) { if (saved[n] === undefined) delete process.env[n]; else process.env[n] = saved[n]; } });
  for (const n of names) process.env[n] = JSON.stringify({ shellcheck: { path: "/evil", sha256: "0".repeat(64), version: "9" } });
  const savedPath = process.env.PATH; t.after(() => { process.env.PATH = savedPath; });
  process.env.PATH = `${path.dirname(tool(t, "shellcheck").path)}${path.delimiter}${savedPath}`;
  assert.deepEqual(load({ dataRoot }), before);
  assert.throws(() => loadScanToolPolicy({ dataRoot, platform: HOST }), /pipeline policy reader is required/);
});

test("the enrol command: explicit values only, verifies the digest, refuses to replace without --rotate, prints what it wrote; show prints the record", { skip: HOST === "win32" && "POSIX paths" }, (t) => {
  const dataRoot = tmp(t); const sc = tool(t, "shellcheck"); const out = []; const err = [];
  const run = (...argv) => main(argv, { out: (m) => out.push(m), err: (m) => err.push(m) });
  const enroll = (over = {}) => ["enroll", "--platform", HOST, "--data-root", dataRoot, "--tool", over.tool ?? "shellcheck", "--path", over.path ?? sc.path, "--sha256", over.sha256 ?? sc.sha256, "--version", over.version ?? sc.version];
  assert.equal(run("show", "--platform", HOST, "--data-root", dataRoot), 1); assert.match(out.pop(), /no enrollment/);
  assert.equal(run(...enroll({ sha256: "f".repeat(64) })), 1); assert.match(err.pop(), /RK_SCAN_TOOLS_DIGEST/);
  assert.equal(run(...enroll()), 0); assert.match(out.pop(), new RegExp(`enrolled shellcheck 0\\.11\\.0 for ${HOST} \\(sha256 ${sc.sha256}\\) in .*${HOST}\\.json$`));
  assert.equal(run(...enroll()), 1); assert.match(err.pop(), /RK_SCAN_TOOLS_EXISTS/);
  assert.equal(run(...enroll(), "--rotate"), 0); assert.match(out.pop(), /\(rotation\)/);
  assert.equal(run("show", "--platform", HOST, "--data-root", dataRoot), 0); assert.deepEqual(JSON.parse(out.pop()).tools.shellcheck, sc);
  assert.equal(run("enroll", "--platform", HOST, "--data-root", dataRoot, "--tool", "shellcheck"), 1); assert.match(err.pop(), /RK_SCAN_TOOLS_INVALID/);
  assert.equal(run(...enroll({ tool: "rustfmt" })), 1); assert.match(err.pop(), /not a trusted scan tool/);
  assert.equal(run("enroll", "--platform", "freebsd", "--data-root", dataRoot), 1); assert.match(err.pop(), /RK_SCAN_TOOLS_PLATFORM/);
  assert.equal(run("nonsense"), 2); assert.match(err.pop(), /usage: scan-tool-enrolment\.mjs/);
  assert.equal(run("show", "--platform", HOST), 1); assert.match(err.pop(), /RK_SCAN_TOOLS_ROOT/);
});

test("RightKit's committed ci/rk-pipeline.json tools, read from this checkout's gate revision, form a policy the scan engine can read (darwin and win32 hold shellcheck; linux none, by design)", (t) => {
  const here = path.dirname(fileURLToPath(import.meta.url));
  const top = git(here, "rev-parse", "--show-toplevel");
  if (top.status !== 0 || git(here, "show", "HEAD:ci/rk-pipeline.json").status !== 0) { t.skip("no git history of ci/rk-pipeline.json here (an export)"); return; }
  const read = readGateRevisionScanTools({ gateFile: path.join(here, "bin", "rightkit-gate.mjs"), checkout: top.stdout.trim() });
  assert.ok(read && typeof read.tools === "object", "the gate's revision carries a tools object");
  for (const platform of SCAN_TOOL_PLATFORMS) {
    const policy = scanToolPolicyFromPipeline(read.tools, platform);
    for (const [name, e] of Object.entries(policy)) assert.ok(SCAN_TOOL_NAMES.includes(name) && typeof e.path === "string" && /^[0-9a-f]{64}$/.test(e.sha256) && typeof e.version === "string", `${platform} ${name}`);
  }
});
