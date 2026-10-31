// The package's test command (`pnpm test`): every `*.test.mjs` (and `cli/*.test.mjs`) of @rightkit/release, in one or more shards. Not shipped: lives under test-support/.
//
// A shard is its own `node --import ./test-support/broker-guard.mjs --test --test-concurrency=1 --test-force-exit --test-shard=<i>/<n>` process, so the files of a shard run one
// after the other exactly as the single serial command always ran them, and every shard is a separate runner with its own throwaway temp and data roots (the broker guard makes
// them per process). Shards never share a runner: node's own `--test-concurrency` would run the files of one runner side by side in ONE shared guard data root, where the
// build-ownership record, the snapshots and the receipts of different files would meet.
//
// How many shards: `RK_TEST_SHARDS=<n>` when set; otherwise one (the serial run, unchanged) on a developer machine and one per core, at most four, where CI sets `CI`
// or `GITHUB_ACTIONS` (a hosted Windows runner needed more than 30 minutes for the serial run: process starts and git are slow there).
// The files of a run are the shard's `index % total` selection node makes over the sorted list this runner hands every shard.
//
// Output: the shards' reports stream live, whole lines at a time and unprefixed, so the node:test summary lines (`ℹ tests|pass|fail|skipped|todo N`) of every shard are in the
// stream for scripts/ci/test-results.mjs, which SUMS them. Exit code: zero only when every shard exited zero.
//
// Arguments: `node test-support/run-test-shards.mjs [<file.test.mjs> ...]` runs just those files (a repeated run of one file is still one shard).
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "../background-process.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const testFilesIn = (dir) => fs.readdirSync(path.join(root, dir)).filter((name) => name.endsWith(".test.mjs")).sort().map((name) => (dir ? `${dir}/${name}` : name));
const named = process.argv.slice(2);
const files = named.length ? named : [...testFilesIn(""), ...testFilesIn("cli")];

const cores = os.availableParallelism?.() ?? os.cpus().length;
const asked = Number(process.env.RK_TEST_SHARDS);
const onCi = process.env.CI === "true" || process.env.GITHUB_ACTIONS === "true";
const shards = Math.max(1, Math.min(files.length, Number.isInteger(asked) && asked >= 1 ? asked : onCi ? Math.min(4, cores) : 1));

/** Hands `write` whole lines only: two shards' output never meets inside a line, so a summary line stays one line. */
function lines(write) {
  let tail = "";
  return {
    push(chunk) { tail += chunk; const parts = tail.split("\n"); tail = parts.pop(); for (const line of parts) write(`${line}\n`); },
    end() { if (tail) write(`${tail}\n`); tail = ""; },
  };
}

const children = new Set();
function runShard(index) {
  return new Promise((resolve) => {
    const argv = ["--import", "./test-support/broker-guard.mjs", "--test", "--test-concurrency=1", "--test-force-exit", ...(shards > 1 ? [`--test-shard=${index}/${shards}`] : []), ...files];
    const child = spawn(process.execPath, argv, { cwd: root, env: process.env, stdio: ["ignore", "pipe", "pipe"] });
    children.add(child);
    const out = lines((line) => process.stdout.write(line));
    const err = lines((line) => process.stderr.write(line));
    child.stdout.setEncoding("utf8"); child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => out.push(chunk));
    child.stderr.on("data", (chunk) => err.push(chunk));
    child.on("error", (error) => { process.stderr.write(`# shard ${index}/${shards} could not start: ${error.message}\n`); resolve(1); });
    child.on("close", (code, signal) => { out.end(); err.end(); children.delete(child); resolve(code ?? (signal ? 1 : 0)); });
  });
}

// A cancelled or timed-out run takes its shards with it: nothing is left running after the command ends.
for (const signal of ["SIGINT", "SIGTERM"]) process.on(signal, () => { for (const child of children) child.kill(); process.exitCode = 1; });

process.stdout.write(`# test shards: ${shards} (${files.length} files${shards > 1 ? `, shard i takes the files at index % ${shards} === i - 1` : ""})\n`);
const codes = await Promise.all(Array.from({ length: shards }, (_, i) => runShard(i + 1)));
process.stdout.write(`# test shards exited: ${codes.join(", ")}\n`);
process.exitCode = codes.every((code) => code === 0) ? 0 : 1;
