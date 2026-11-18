import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { gitFixture } from "./release-qa-fixtures.mjs";
import { pureQuestion } from "./probe-memo.mjs";

test("only questions whose answer cannot change during a run are remembered", () => {
  const c = ["-c", "safe.directory=D:/x", "-c", "core.fsmonitor=false"]; const sha = "3a017e99e2f69ab3c8cae9ca03a285b8de8cb5bf";
  // The real repeats of 2026-11-17 (Dell, S1 check ledger).
  for (const args of [[...c, "rev-parse", "--show-toplevel"], [...c, "rev-parse", "--absolute-git-dir"], [...c, "rev-parse", "--show-object-format"], ["rev-parse", "--path-format=absolute", "--git-common-dir"],
    [...c, "rev-parse", "--verify", "--quiet", `${sha}^{commit}`], [...c, "rev-parse", "--verify", "--quiet", `${sha}^{tree}`], ["config", "--get", "remote.origin.url"], ["ls-tree", "-r", "--name-only", "-z", sha]]) assert.equal(pureQuestion("git", args), true, args.join(" "));
  assert.equal(pureQuestion("C:\\Program Files\\Git\\cmd\\git.exe", ["rev-parse", "--show-toplevel"]), true);
  assert.equal(pureQuestion("C:\\WINDOWS\\system32\\where.exe", ["rustright"]), true); assert.equal(pureQuestion("/usr/bin/which", ["rustright"]), true);
  // Anything that reads the working tree, a moving name, or changes something is asked every time.
  for (const args of [["status", "--porcelain"], ["rev-parse", "HEAD"], ["rev-parse", "--verify", "--quiet", "main^{commit}"], ["rev-parse", "--verify", "--quiet", "HEAD^{tree}"], ["ls-tree", "-r", "--name-only", "-z", "HEAD"],
    ["config", "--get", "user.name"], ["config", "remote.origin.url", "x"], ["diff", "--quiet"], ["add", "-A"], ["rev-parse", "--show-toplevel", "--show-prefix"], ["ls-files"]]) assert.equal(pureQuestion("git", args), false, args.join(" "));
  // A blob of a commit named by its full id, and a broker job's status by id (kept only when final, tested below).
  assert.equal(pureQuestion("git", ["show", `${sha}:rk/Cargo.toml`]), true);
  assert.equal(pureQuestion("C:\\Program Files\\Git\\cmd\\git.exe", ["show", `${sha}:app/pnpm-lock.yaml`]), true);
  for (const args of [["show", "HEAD:rk/Cargo.toml"], ["show", sha], ["show", `${sha}:a`, "--stat"], ["show", `main:${sha}`]]) assert.equal(pureQuestion("git", args), false, args.join(" "));
  assert.equal(pureQuestion("D:\\.rightkit-managed\\agent-bin\\rustright.cmd", ["status", "ccc54925-c162-420c-917c-816b4577656d"]), true);
  assert.equal(pureQuestion("rustright", ["status"]), false); assert.equal(pureQuestion("rustright", ["cargo", "build"]), false);
  assert.equal(pureQuestion("cargo", ["--version"]), false); assert.equal(pureQuestion("where", ["a", "b"]), false); assert.equal(pureQuestion("git", ["rev-parse", 1]), false);
});

test("in one process an identical pure question starts one child; other commands and failed questions start one each time", (t) => {
  const repo = gitFixture();
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-memo-")); const ledger = path.join(dir, "ledger.jsonl");
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const script = `
    import { installSpawnLedger } from ${JSON.stringify(new URL("./spawn-ledger.mjs", import.meta.url).href)};
    import { installProbeMemo } from ${JSON.stringify(new URL("./probe-memo.mjs", import.meta.url).href)};
    installSpawnLedger(); const stats = installProbeMemo();
    const cp = await import("node:child_process");
    const cwd = ${JSON.stringify(repo)}; const out = [];
    for (let i = 0; i < 3; i += 1) out.push(cp.spawnSync("git", ["rev-parse", "--show-toplevel"], { cwd, encoding: "utf8" }).stdout.trim());
    for (let i = 0; i < 2; i += 1) out.push(String(cp.execFileSync("git", ["rev-parse", "--show-toplevel"], { cwd })).trim());
    for (let i = 0; i < 2; i += 1) cp.spawnSync("git", ["status", "--porcelain"], { cwd, encoding: "utf8" });
    const missing = "0".repeat(40) + "^{commit}"; const fails = [];
    for (let i = 0; i < 2; i += 1) fails.push(cp.spawnSync("git", ["rev-parse", "--verify", "--quiet", missing], { cwd, encoding: "utf8" }).status);
    // A remembered answer is a copy: changing what one caller got does not change what the next gets.
    const a = cp.spawnSync("git", ["rev-parse", "--show-object-format"], { cwd }); a.stdout.fill(0);
    const b = cp.spawnSync("git", ["rev-parse", "--show-object-format"], { cwd });
    console.log(JSON.stringify({ out, fails, stats, second: String(b.stdout).trim(), outputIsArray: Array.isArray(b.output) && b.output[1] === b.stdout }));
  `;
  const file = path.join(dir, "run.mjs"); fs.writeFileSync(file, script);
  const r = spawnSync(process.execPath, [file], { encoding: "utf8", env: { ...process.env, RK_GATE_SPAWN_LEDGER: ledger } });
  assert.equal(r.status, 0, r.stderr);
  const got = JSON.parse(r.stdout.trim().split("\n").pop());
  assert.equal(new Set(got.out).size, 1, "every caller got the same top level"); assert.equal(got.out.length, 5);
  assert.deepEqual(got.fails.map((s) => s !== 0), [true, true]);
  assert.match(got.second, /^sha(1|256)$/); assert.equal(got.outputIsArray, true);
  const rows = fs.readFileSync(ledger, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
  const count = (pred) => rows.filter((x) => pred(x.args.join(" "))).length;
  // spawnSync and execFileSync return different things, so each is asked once: 2 real starts for 5 calls.
  assert.equal(count((a) => a === "rev-parse --show-toplevel"), 2);
  assert.equal(count((a) => a === "status --porcelain"), 2, "a command that reads the working tree runs every time");
  assert.equal(count((a) => a.startsWith("rev-parse --verify --quiet 0000")), 2, "a failed question is asked again");
  assert.equal(count((a) => a === "rev-parse --show-object-format"), 1);
  assert.equal(got.stats.answered, 4);
});

test("NN22: a whole-tree listing of a full commit id is kept on disk and answers the next process without starting git", (t) => {
  const repo = gitFixture();
  t.after(() => fs.rmSync(repo, { recursive: true, force: true }));
  const sha = "3a017e99e2f69ab3c8cae9ca03a285b8de8cb5bf";
  assert.equal(pureQuestion("git", ["-c", "core.attributesFile=D:/tmp/rk-sealed-git-x/empty", "ls-tree", "-r", "-z", "--full-tree", sha]), true, "the sealed listing");
  for (const args of [["ls-tree", "-r", "-z", "--full-tree", "HEAD"], ["ls-tree", "-r", "-z", sha], ["ls-tree", "-z", "--full-tree", sha], ["ls-tree", "-r", "--name-only", sha]]) assert.equal(pureQuestion("git", args), false, args.join(" "));
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-trees-")); const store = path.join(dir, "tree-lists");
  const head = spawnSync("git", ["rev-parse", "HEAD"], { cwd: repo, encoding: "utf8" }).stdout.trim();
  const script = (n) => `
    import { installSpawnLedger } from ${JSON.stringify(new URL("./spawn-ledger.mjs", import.meta.url).href)};
    import { installProbeMemo } from ${JSON.stringify(new URL("./probe-memo.mjs", import.meta.url).href)};
    installSpawnLedger(); const stats = installProbeMemo({ storeDir: ${JSON.stringify(store)} });
    const cp = await import("node:child_process"); const cwd = ${JSON.stringify(repo)};
    const a = cp.spawnSync("git", ["-c", "core.attributesFile=${n}", "ls-tree", "-r", "-z", "--full-tree", ${JSON.stringify(head)}], { cwd, encoding: "utf8", maxBuffer: 1 << 28 }).stdout;
    const b = cp.execFileSync("git", ["ls-tree", "-r", "--name-only", "-z", ${JSON.stringify(head)}], { cwd, maxBuffer: 1 << 28 });
    console.log(JSON.stringify({ a: a.length, aHash: a.slice(0, 200), b: b.length, isBuf: Buffer.isBuffer(b), stats }));
  `;
  const run = (n) => {
    const file = path.join(dir, `run${n}.mjs`); fs.writeFileSync(file, script(n)); const ledger = path.join(dir, `ledger${n}.jsonl`);
    const r = spawnSync(process.execPath, [file], { encoding: "utf8", env: { ...process.env, RK_GATE_SPAWN_LEDGER: ledger } });
    assert.equal(r.status, 0, r.stderr);
    const rows = fs.existsSync(ledger) ? fs.readFileSync(ledger, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l)) : [];
    return { got: JSON.parse(r.stdout.trim().split("\n").pop()), walks: rows.filter((x) => x.args.includes("ls-tree")).length };
  };
  const first = run(1); const second = run(2);
  assert.equal(first.walks, 2); assert.equal(second.walks, 0, "the second process lists nothing: a different temp attributes file does not change the question");
  assert.equal(second.got.stats.stored, 2);
  assert.deepEqual([second.got.a, second.got.aHash, second.got.b, second.got.isBuf], [first.got.a, first.got.aHash, first.got.b, true], "the same bytes, in the encoding each caller asked for");
  fs.rmSync(dir, { recursive: true, force: true });
});

test("a broker job's status is kept only once the receipt records the job's end; a git answer always", async () => {
  const { FINAL_ONLY } = await import("./probe-memo.mjs");
  const running = JSON.stringify({ receipt: { id: "x", terminalAt: null } }); const ended = JSON.stringify({ receipt: { id: "x", terminalAt: "2026-11-18T00:00:00Z" } });
  assert.equal(FINAL_ONLY("rustright", Buffer.from(running)), false);
  assert.equal(FINAL_ONLY("D:\\.rightkit-managed\\agent-bin\\rustright.cmd", ended), true);
  assert.equal(FINAL_ONLY("rustright", JSON.stringify({ id: "x", terminalAt: "2026-11-18T00:00:00Z" })), true, "a bare receipt");
  assert.equal(FINAL_ONLY("rustright", "not json"), false);
  assert.equal(FINAL_ONLY("git", "anything"), true);
});
