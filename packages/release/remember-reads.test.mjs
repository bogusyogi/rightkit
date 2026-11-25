// GC-A27 NN08: nothing is started twice in one run. Three read-only commands the gate asked two to four times a run (acceptance cases on 0.6.63,
// 2026-11-24; rightkit#1514) are answered from memory the second time: same command, arguments, folder and environment.
import assert from "node:assert/strict";
import { test } from "node:test";
import { isRememberedRead, rememberReads } from "./deterministic-scans.mjs";

const SHA = "a".repeat(40);
test("only reads whose answer cannot change in a run are remembered", () => {
  assert.equal(isRememberedRead("git", ["show", `${SHA}:package.json`]), true);
  assert.equal(isRememberedRead("C:\\Program Files\\Git\\cmd\\git.exe", ["-c", "core.fsmonitor=false", "rev-parse", "--show-toplevel"]), true);
  assert.equal(isRememberedRead("pnpm.cmd", ["--version"]), true);
  // a moving name, a write, or anything else is always asked again
  assert.equal(isRememberedRead("git", ["show", "HEAD:package.json"]), false);
  assert.equal(isRememberedRead("git", ["rev-parse", "HEAD"]), false);
  assert.equal(isRememberedRead("git", ["status", "--porcelain"]), false);
  assert.equal(isRememberedRead("pnpm", ["install"]), false);
  assert.equal(isRememberedRead("cargo", ["build"]), false);
  // a tool's own version and pnpm's resolved configuration are remembered too (keyed by folder and environment)
  for (const [c, a] of [["cargo", ["--version"]], ["actionlint", ["-version"]], ["shellcheck", ["--version"]], ["rustc", ["-V"]], ["pnpm", ["config", "list", "--json"]], ["pnpm", ["config", "get", "userconfig"]], ["cargo-audit", ["audit", "--version"]]]) assert.equal(isRememberedRead(c, a), true, `${c} ${a.join(" ")}`);
  assert.equal(isRememberedRead("pnpm", ["config", "set", "x", "y"]), false, "a write is never remembered");
});
test("the same read in the same folder is started once; another folder, another environment or a failure is started again", () => {
  const calls = []; let fail = false;
  const spawn = (c, a, o) => { calls.push([c, a.join(" "), o.cwd ?? ""]); return fail ? { status: 1, stdout: "", stderr: "no" } : { status: 0, stdout: `out ${calls.length}`, stderr: "" }; };
  const run = rememberReads(spawn, { store: new Map() });
  const first = run("git", ["show", `${SHA}:package.json`], { cwd: "D:\\repo" });
  assert.equal(run("git", ["show", `${SHA}:package.json`], { cwd: "D:\\repo" }).stdout, first.stdout);
  assert.equal(calls.length, 1, "the second identical read starts nothing");
  run("git", ["show", `${SHA}:package.json`], { cwd: "D:\\other" }); assert.equal(calls.length, 1, "an object named by its hash is the same object in any folder");
  run("actionlint", ["-version"], { cwd: "D:\\repo" }); run("actionlint", ["-version"], { cwd: "D:\\other" }); assert.equal(calls.length, 2, "a tool's version is its binary's, whatever the folder");
  run("git", ["rev-parse", "--show-toplevel"], { cwd: "D:\\repo" }); run("git", ["rev-parse", "--show-toplevel"], { cwd: "D:\\other" }); assert.equal(calls.length, 4, "the top of a checkout is the folder's");
  run("git", ["status"], { cwd: "D:\\repo" }); run("git", ["status"], { cwd: "D:\\repo" }); assert.equal(calls.length, 6, "anything else always runs");
  fail = true; run("git", ["rev-parse", "--show-toplevel"], { cwd: "D:\\x" }); run("git", ["rev-parse", "--show-toplevel"], { cwd: "D:\\x" });
  assert.equal(calls.length, 8, "a failed read is not remembered");
});

test("the asynchronous path answers a remembered read from the same memory", async () => {
  const { defaultSpawnAsync } = await import("./deterministic-scans.mjs");
  const node = process.execPath; const cwd = process.cwd();
  const a = await defaultSpawnAsync(node, ["--version"], { cwd }); const b = await defaultSpawnAsync(node, ["--version"], { cwd });
  assert.equal(a.status, 0); assert.deepEqual(b, { status: 0, signal: null, stdout: a.stdout, stderr: a.stderr }, "the second answer is the remembered one");
});

test("the gate runs with defaultSpawn itself, so the pnpm answers kept across runs (NN25, which test spawn === defaultSpawn) stay on", async () => {
  const src = (await import("node:fs")).readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  assert.match(src, /spawn: defaultSpawn, exists: fs\.existsSync/); assert.doesNotMatch(src, /spawn: rememberReads\(defaultSpawn\)/);
});
test("a rustup proxy's or pnpm's version keeps its folder in the key; a fixed tool's does not", () => {
  const calls = []; const run = rememberReads((c, a, o) => { calls.push(o.cwd); return { status: 0, stdout: "v", stderr: "" }; }, { store: new Map() });
  run("cargo", ["--version"], { cwd: "D:\a" }); run("cargo", ["--version"], { cwd: "D:\b" }); assert.equal(calls.length, 2, "rustup may pick another toolchain in another folder");
  run("pnpm", ["--version"], { cwd: "D:\a" }); run("pnpm", ["--version"], { cwd: "D:\b" }); assert.equal(calls.length, 4);
  run("shellcheck", ["--version"], { cwd: "D:\a" }); run("shellcheck", ["--version"], { cwd: "D:\b" }); assert.equal(calls.length, 5);
});

test("NN08: a snapshot run does not ask the snapshot for its top level, and the selected commit is not asked for again", async () => {
  const { ignoredPathSkip } = await import("./deterministic-scans.mjs");
  const asked = [];
  const ctx = { contentRoot: "D:\\snap", trackedFiles: ["a.rs"], deps: { env: {}, spawn: async (c, a) => { asked.push(a.join(" ")); return { status: 1, stdout: "" }; } } };
  await ignoredPathSkip(ctx).catch(() => null);
  assert.equal(asked.includes("rev-parse --show-toplevel"), false, "the gate already knows the content root is a snapshot");
  const src = (await import("node:fs")).readFileSync(new URL("./bin/rightkit-gate.mjs", import.meta.url), "utf8");
  assert.match(src, /snapshotBCommit\?\.commit \?\? \(\/\^\[0-9a-f\]\{40\}\$\/\.test\(named \?\? ""\) \? named :/);
});

test("NN08: a path missing at a full commit id is asked once; a cancel signal still takes a remembered answer", async () => {
  const { absentAtCommit } = await import("./probe-memo.mjs");
  assert.equal(absentAtCommit("git", ["show", "9da554".padEnd(40, "0") + ":package.json"]), true);
  assert.equal(absentAtCommit("git", ["show", "HEAD:package.json"]), false);
  const src = (await import("node:fs")).readFileSync(new URL("./deterministic-scans.mjs", import.meta.url), "utf8");
  assert.match(src, /!options\.onSpawn && !options\.signal\?\.aborted\)/);
});

test("NN08: the advisory database's HEAD question names its checkout with -C", async () => {
  const { ensureAdvisoryDb } = await import("./advisory-db.mjs");
  const commit = "a".repeat(40); const asked = [];
  const git = (args) => { asked.push(args); return { status: 0, stdout: commit + "\n" }; };
  const fsImpl = { existsSync: () => true };
  const root = (await import("node:os")).tmpdir();
  try { ensureAdvisoryDb({ cacheRoot: root, pin: { url: "https://github.com/rustsec/advisory-db", commit }, git, fsImpl }); } catch {}
  const q = asked.find((a) => a.includes("rev-parse"));
  assert.ok(q, "HEAD was asked"); assert.equal(q[0], "-C"); assert.deepEqual(q.slice(2), ["rev-parse", "HEAD"]);
});

test("NN08: the async path keeps a missing path at a full commit id; a second ask starts nothing", async () => {
  const src = (await import("node:fs")).readFileSync(new URL("./deterministic-scans.mjs", import.meta.url), "utf8");
  assert.match(src, /r\.status === 128 && absentAtCommit\(command, args \?\? \[\]\)/);
});

test("NN08: pnpm --version keys on pnpm's own variables, not per-run ones", async () => {
  const { PNPM_VERSION_ENV } = await import("./deterministic-scans.mjs");
  for (const k of ["PATH", "Path", "PNPM_HOME", "COREPACK_HOME", "npm_config_userconfig", "USERPROFILE", "APPDATA"]) assert.ok(PNPM_VERSION_ENV.test(k), k);
  for (const k of ["RK_LEASE", "RUSTRIGHT_QUIET_LEASE", "RK_GATE_SPAWN_LEDGER", "RIGHTKIT_SCAN_TOKEN"]) assert.equal(PNPM_VERSION_ENV.test(k), false, k);
});

test("NN08: the probe shares the Class A digest's pnpm version where no pin exists (one start), and not where one does (two)", async () => {
  const { pnpmVersionAnswer, pnpmVersionAnswerAsync } = await import("./snapshot-dependency-reuse.mjs");
  const os = await import("node:os"); const fs = await import("node:fs"); const path = await import("node:path");
  const pnpmOnPath = (() => { for (const d of String(process.env.PATH ?? process.env.Path ?? "").split(path.delimiter)) for (const n of ["pnpm.cmd", "pnpm"]) if (d && fs.existsSync(path.join(d, n))) return true; return false; })();
  if (!pnpmOnPath) return; // the key needs pnpm's program; without it nothing is remembered and the rule cannot be shown here
  const probeOnly = { npm_config_ignore_pnpmfile: "true", npm_config_ignore_scripts: "true", COREPACK_ENABLE_AUTO_PIN: "0", RIGHTKIT_CARGO_SCAN: "probe" };
  const env = { ...process.env, RK_TEST_SALT: String(Math.random()) };
  // The gate's answer server, stood in by a map (resident-answers.mjs: answer-get / answer-put).
  const kept = new Map(); const ask = (req) => (req.op === "answer-get" ? (kept.has(req.key) ? { value: kept.get(req.key) } : null) : (kept.set(req.key, req.value), { ok: true }));
  const run = (dirA, dirB) => { kept.clear(); let starts = 0; const v = `9.${Math.floor(Math.random() * 1e6)}.0`;
    pnpmVersionAnswer({ cwd: dirA, env, remember: true, ask, run: () => { starts += 1; return v; } });
    return pnpmVersionAnswerAsync({ cwd: dirB, env: { ...env, ...probeOnly }, remember: true, ask, run: async () => { starts += 1; return v; } }).then(() => starts); };
  const plainA = fs.mkdtempSync(path.join(os.tmpdir(), "rk-nopin-a-")); const plainB = fs.mkdtempSync(path.join(os.tmpdir(), "rk-nopin-b-"));
  assert.equal(await run(plainA, plainB), 1, "no pin: one start");
  const pinned = fs.mkdtempSync(path.join(os.tmpdir(), "rk-pin-")); fs.writeFileSync(path.join(pinned, "package.json"), JSON.stringify({ packageManager: "pnpm@9.15.0" }));
  const plainC = fs.mkdtempSync(path.join(os.tmpdir(), "rk-nopin-c-"));
  assert.equal(await run(pinned, plainC), 2, "a pin: separate answers");
});

test("NN06: the acceptance run does not start case 2 within 45 minutes of UTC midnight", async () => {
  const { minutesToUtcMidnight, MIDNIGHT_MARGIN_MIN } = await import("./acceptance-run.mjs");
  assert.equal(MIDNIGHT_MARGIN_MIN, 45);
  assert.equal(minutesToUtcMidnight(new Date("2026-11-24T23:59:00Z")), 1);
  assert.equal(minutesToUtcMidnight(new Date("2026-11-25T00:03:00Z")), 1437);
});
