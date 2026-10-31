// GB-26 (GATE durable retry/E9; BR12): authoritative result files (the gate's stage journal, the protected QA receipts) are replaced atomically. A
// failure at ANY step leaves the previous valid file readable, the new content wholly present or wholly absent, no temp file behind, and a typed error.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { ATOMIC_WRITE_FAILED, AtomicWriteError, writeFileAtomic } from "./atomic-write.mjs";
import { spawnSync } from "./background-process.mjs";
import { QA_RECEIPT_SCHEMA, readQaReceipt, writeQaReceipt } from "./release-qa-receipt.mjs";
import { RECEIPT_SCHEMA, readReceipt, receiptKey, writeReceipt } from "./gate-stages.mjs";
import { tmpDir } from "./test-support/door-fixture.mjs";

/** A node:fs whose named function fails once it is called (after `after` successful calls). */
const failing = (name, { after = 0, error = new Error(`injected ${name} failure (ENOSPC)`) } = {}) => {
  let calls = 0;
  return new Proxy(fs, { get(target, key) { const value = target[key]; if (key !== name || typeof value !== "function") return typeof value === "function" ? value.bind(target) : value; return (...args) => { if (calls++ >= after) throw error; return value.apply(target, args); }; } });
};
const leftovers = (dir) => fs.readdirSync(dir).filter((n) => n.endsWith(".tmp"));

test("[GB-26] a failure at every boundary keeps the previous valid file readable, removes the temp file and is typed with its step", (t) => {
  const dir = tmpDir(t, "rk-gb26-");
  const file = path.join(dir, "receipt.json");
  writeFileAtomic(file, '{"v":1}\n');
  for (const [fn, step] of [["mkdirSync", "mkdir"], ["openSync", "open"], ["writeSync", "write"], ["fsyncSync", "fsync"], ["closeSync", "close"], ["renameSync", "rename"]]) {
    assert.throws(() => writeFileAtomic(file, '{"v":2,"big":"' + "x".repeat(100000) + '"}\n', { fs: failing(fn) }), (e) => e instanceof AtomicWriteError && e.code === ATOMIC_WRITE_FAILED && e.step === step && /ENOSPC/.test(e.message), `${fn} -> ${step}`);
    assert.equal(fs.readFileSync(file, "utf8"), '{"v":1}\n', `${step}: the previous valid file is untouched`);
    assert.deepEqual(leftovers(dir), [], `${step}: no temp file is left behind`);
  }
  writeFileAtomic(file, '{"v":3}\n');
  assert.equal(fs.readFileSync(file, "utf8"), '{"v":3}\n', "a later write succeeds and is wholly present");
  assert.deepEqual(leftovers(dir), []);
});

test("[GB-26] a crashed writer's temp file is never read as the destination, and a replace keeps the permission bits", (t) => {
  const dir = tmpDir(t, "rk-gb26b-");
  const file = path.join(dir, "j.json");
  fs.writeFileSync(path.join(dir, ".j.json.4242.deadbeef.tmp"), '{"torn":');
  assert.equal(fs.existsSync(file), false, "absent, not truncated");
  writeFileAtomic(file, "{}\n", { mode: 0o640 });
  if (process.platform !== "win32") assert.equal(fs.statSync(file).mode & 0o777, 0o640);
  fs.chmodSync(file, 0o600);
  writeFileAtomic(file, '{"a":1}\n');
  if (process.platform !== "win32") assert.equal(fs.statSync(file).mode & 0o777, 0o600, "the replaced file keeps its permissions");
  assert.deepEqual(JSON.parse(fs.readFileSync(file, "utf8")), { a: 1 });
});

test("[GB-26] the gate's stage journal and the protected QA receipt are written through it: a failed write leaves the previous record readable; reconciling the same record rewrites it whole", (t) => {
  const commonDir = tmpDir(t, "rk-gb26c-");
  const commit = "a".repeat(40);
  const receipt = (stage) => ({ schema: RECEIPT_SCHEMA, commit, app: ".", binding: { tree: "t" }, stages: { check: { ok: true }, ...stage } });
  writeReceipt({ commonDir, receipt: receipt({}) });
  const dir = path.join(commonDir, "rightkit-gate", "receipts");
  assert.deepEqual(readReceipt({ commonDir, commit }).stages, { check: { ok: true } });
  assert.deepEqual(leftovers(dir), [], "the default write leaves no temp file");
  // Disk failure while writing the NEXT version of the journal: the previous valid record still reads.
  assert.throws(() => writeReceipt({ commonDir, receipt: receipt({ compile: { ok: true } }), write: (file, data) => writeFileAtomic(file, data, { fs: failing("fsyncSync") }) }), (e) => e.code === ATOMIC_WRITE_FAILED);
  assert.deepEqual(readReceipt({ commonDir, commit }).stages, { check: { ok: true } }, "the previous record is readable, not truncated");
  assert.deepEqual(leftovers(dir), []);
  writeReceipt({ commonDir, receipt: receipt({ compile: { ok: true } }) });
  assert.deepEqual(Object.keys(readReceipt({ commonDir, commit }).stages), ["check", "compile"], "the retry is whole");
  assert.ok(fs.existsSync(path.join(dir, `${receiptKey(commit)}.json`)));
  // The protected QA receipt: same primitive.
  const qa = path.join(commonDir, "qa", "installed.receipt.json");
  writeQaReceipt(qa, { schema: QA_RECEIPT_SCHEMA, stage: "installed", n: 1 });
  writeQaReceipt(qa, { schema: QA_RECEIPT_SCHEMA, stage: "installed", n: 2 });
  assert.equal(readQaReceipt(qa).n, 2);
  assert.deepEqual(leftovers(path.dirname(qa)), []);
  const source = fs.readFileSync(new URL("./release-qa-receipt.mjs", import.meta.url), "utf8");
  assert.match(source, /export function writeQaReceipt[\s\S]*?writeFileAtomic\(/, "no direct overwrite of the authoritative QA receipt");
  assert.doesNotMatch(source.slice(source.indexOf("export function writeQaReceipt"), source.indexOf("function loadKeyOrRefuse")), /writeFileSync/);
});

test("[GB-26, E2E L3-01] a stage-journal writer killed by a real file-size limit mid-write leaves the previous receipt readable (no torn JSON)", { skip: process.platform === "win32" && "needs a POSIX RLIMIT_FSIZE" }, (t) => {
  const commonDir = tmpDir(t, "rk-l3-fsize-");
  const commit = "a".repeat(40);
  const base = { schema: RECEIPT_SCHEMA, commit, app: ".", binding: { tree: "t" } };
  writeReceipt({ commonDir, receipt: { ...base, stages: { check: { ok: true } } } });
  // The child writes a 200 KB receipt under `ulimit -f 8` (8 blocks): the kernel stops the write part way, as a full disk would.
  const child = `import { writeReceipt } from ${JSON.stringify(new URL("./gate-stages.mjs", import.meta.url).href)};
    writeReceipt({ commonDir: ${JSON.stringify(commonDir)}, receipt: { ...${JSON.stringify(base)}, stages: { check: { ok: true }, compile: { ok: true, pad: "x".repeat(200000) } } } });`;
  // `ulimit` needs a shell. The class A launch guard refuses `exec "$0" "$@"` (an exec'd word built by expansion), so the program and the child
  // text are single-quoted literals in the shell text, which the guard reads to the end.
  const sq = (text) => `'${text.replaceAll("'", "'\\''")}'`;
  const r = spawnSync("sh", ["-c", `ulimit -f 8; exec ${sq(process.execPath)} --input-type=module -e ${sq(child)}`], { encoding: "utf8" });
  assert.notEqual(r.status, 0, "the writer really was stopped by the limit");
  assert.deepEqual(readReceipt({ commonDir, commit })?.stages, { check: { ok: true } }, "the previous receipt is still whole and readable");
  assert.deepEqual(leftovers(path.join(commonDir, "rightkit-gate", "receipts")), [], "no temp file is left behind");
});
