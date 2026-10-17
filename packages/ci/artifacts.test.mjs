import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import crypto from "node:crypto";
import { fetchArtifacts, parseFetchArgs, treeDigest, globToRegExp, artifactsCommand, RECEIPT_SCHEMA } from "./artifacts.mjs";

const sha = (text) => crypto.createHash("sha256").update(text).digest("hex");

function fakeGh(runs) {
  const calls = [];
  const execFile = async (cmd, args) => {
    calls.push([cmd, ...args]);
    if (cmd === "gh") {
      if (args[0] === "api" && !/\/zip$/.test(args[1])) return { stdout: JSON.stringify([{ artifacts: runs.artifacts }]) };
      assert.equal(args[0], "api");
      const id = Number(/artifacts\/(\d+)\/zip$/.exec(args[1])[1]);
      const artifact = (runs.replacement?.[id] ?? runs.artifacts.find((entry) => entry.id === id));
      const name = artifact.name;
      if (runs.fail?.includes(name)) throw new Error("boom\nsecond line");
      return { stdout: Buffer.from(JSON.stringify(runs.files[name] ?? {})) };
    }
    const zip = args.find((arg) => arg.endsWith(".zip") || args.includes("-xf") && arg === args[args.indexOf("-xf") + 1]);
    const dir = args[args.indexOf("-d") + 1] ?? args[args.indexOf("-C") + 1];
    for (const [rel, text] of Object.entries(JSON.parse(fs.readFileSync(zip, "utf8")))) {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), text);
    }
    return { stdout: "" };
  };
  return { execFile, calls };
}

const tmp = () => fs.mkdtempSync(path.join(os.tmpdir(), "right-git-artifacts-"));
const RUN = {
  artifacts: [{ id: 1, name: "swiftright-render-abc" }, { id: 2, name: "results-mac" }, { id: 3, name: "results-win" }],
  files: {
    "swiftright-render-abc": { "render.dll": "dll-bytes" },
    "results-mac": { "results-mac.jsonl": "mac\n", "shared.txt": "same" },
    "results-win": { "results-win.jsonl": "win\n", "shared.txt": "same", "sub/a.txt": "a" },
  },
};

test("parseFetchArgs requires a run id and an output dir and rejects unsafe values", () => {
  assert.deepEqual(parseFetchArgs(["--run", "12", "--out", "o", "--name", "results-*", "--expect-sha256", "A".repeat(64), "--file", "render.dll"]), { run: "12", out: "o", name: "results-*", expectSha256: "a".repeat(64), file: "render.dll" });
  for (const argv of [[], ["--run", "x", "--out", "o"], ["--run", "1"], ["--run", "1", "--out", "o", "--name", "a b"], ["--run", "1", "--out", "o", "--expect-sha256", "abc"], ["--run", "1", "--out", "o", "--file", "x"], ["--run", "1", "--out", "o", "--expect-sha256", "a".repeat(64), "--file", "../x"], ["--run", "1", "--out", "o", "--repo", "bad"], ["--run", "1", "--out", "o", "--wat"]]) {
    assert.throws(() => parseFetchArgs(argv), (error) => error.code === "ARTIFACTS_USAGE", JSON.stringify(argv));
  }
});

test("globToRegExp supports * and ? and anchors the match", () => {
  assert.ok(globToRegExp("results-*").test("results-mac"));
  assert.ok(!globToRegExp("results-*").test("xresults-mac"));
  assert.ok(globToRegExp("a?c.txt").test("abc.txt"));
  assert.ok(!globToRegExp("a?c.txt").test("abcxtxt"));
});

test("fetch merges per-job artifacts into one tree and writes a receipt with the tree digest", async () => {
  const out = path.join(tmp(), "run");
  const receiptPath = path.join(tmp(), "receipt.json");
  const { execFile, calls } = fakeGh(RUN);
  const receipt = await fetchArtifacts({ run: "99", out, name: "results-*", repo: "Orthic-Labs/swiftright", receipt: receiptPath }, { execFile });
  assert.equal(receipt.schema, RECEIPT_SCHEMA);
  assert.deepEqual(receipt.artifacts.map((artifact) => artifact.name), ["results-mac", "results-win"]);
  assert.equal(receipt.fileCount, 4);
  assert.equal(fs.readFileSync(path.join(out, "sub", "a.txt"), "utf8"), "a");
  assert.equal(fs.readFileSync(path.join(out, "shared.txt"), "utf8"), "same");
  const expected = treeDigest([{ path: "shared.txt", sha256: sha("same") }, { path: "results-mac.jsonl", sha256: sha("mac\n") }, { path: "results-win.jsonl", sha256: sha("win\n") }, { path: "sub/a.txt", sha256: sha("a") }]);
  assert.equal(receipt.treeSha256, expected);
  assert.equal(JSON.parse(fs.readFileSync(receiptPath, "utf8")).treeSha256, expected);
  assert.equal(receipt.verified, false);
  assert.ok(calls.some((call) => call[0] === "gh" && call[2] === "repos/Orthic-Labs/swiftright/actions/artifacts/2/zip"));
  assert.ok(calls.every((call) => !call.includes("--name")), "downloads go by artifact id, never by name");
  assert.deepEqual(receipt.artifacts.map((artifact) => artifact.id), [2, 3]);
  assert.ok(receipt.artifacts.every((artifact) => /^[0-9a-f]{64}$/.test(artifact.zipSha256)));
  assert.deepEqual(fs.readdirSync(path.dirname(out)).filter((entry) => entry.includes(".fetch-")), []);
});

test("a per-file pin passes when it matches and the receipt records it", async () => {
  const out = path.join(tmp(), "pair");
  const { execFile } = fakeGh(RUN);
  const receipt = await fetchArtifacts({ run: "99", out, name: "swiftright-render-*", expectSha256: sha("dll-bytes"), file: "render.dll" }, { execFile });
  assert.equal(receipt.verified, true);
  assert.deepEqual(receipt.pin, { subject: "render.dll", expected: sha("dll-bytes"), actual: sha("dll-bytes") });
  assert.equal(fs.readFileSync(path.join(out, "render.dll"), "utf8"), "dll-bytes");
});

test("a pin mismatch fails, leaves the output untouched and removes staging", async () => {
  const root = tmp();
  const out = path.join(root, "pair");
  const { execFile } = fakeGh(RUN);
  await assert.rejects(fetchArtifacts({ run: "99", out, name: "swiftright-render-*", expectSha256: "0".repeat(64), file: "render.dll" }, { execFile }), (error) => error.code === "ARTIFACTS_PIN_MISMATCH");
  assert.equal(fs.existsSync(out), false);
  assert.deepEqual(fs.readdirSync(root), []);
  await assert.rejects(fetchArtifacts({ run: "99", out, name: "swiftright-render-*", expectSha256: "0".repeat(64), file: "missing.dll" }, { execFile }), (error) => error.code === "ARTIFACTS_PIN_FILE");
});

test("the tree pin verifies the whole merged tree", async () => {
  const out = path.join(tmp(), "tree");
  const { execFile } = fakeGh(RUN);
  const digest = treeDigest([{ path: "render.dll", sha256: sha("dll-bytes") }]);
  const receipt = await fetchArtifacts({ run: "99", out, name: "swiftright-render-*", expectSha256: digest }, { execFile });
  assert.deepEqual(receipt.pin, { subject: "tree", expected: digest, actual: digest });
});

test("conflicting files across artifacts, no match, expired, failed download and unsafe names are refused", async () => {
  const conflict = { artifacts: RUN.artifacts, files: { ...RUN.files, "results-win": { ...RUN.files["results-win"], "shared.txt": "different" } } };
  await assert.rejects(fetchArtifacts({ run: "1", out: path.join(tmp(), "o"), name: "results-*" }, fakeGh(conflict)), (error) => error.code === "ARTIFACTS_CONFLICT");
  await assert.rejects(fetchArtifacts({ run: "1", out: path.join(tmp(), "o"), name: "nothing-*" }, fakeGh(RUN)), (error) => error.code === "ARTIFACTS_NONE");
  await assert.rejects(fetchArtifacts({ run: "1", out: path.join(tmp(), "o") }, fakeGh({ ...RUN, artifacts: [{ id: 1, name: "old", expired: true }] })), (error) => error.code === "ARTIFACTS_EXPIRED");
  await assert.rejects(fetchArtifacts({ run: "1", out: path.join(tmp(), "o"), name: "results-mac" }, fakeGh({ ...RUN, fail: ["results-mac"] })), (error) => error.code === "ARTIFACTS_DOWNLOAD" && !/second line/.test(error.message));
  await assert.rejects(fetchArtifacts({ run: "1", out: path.join(tmp(), "o") }, fakeGh({ ...RUN, artifacts: [{ id: 1, name: "../evil" }] })), (error) => error.code === "ARTIFACTS_NAME");
  const listing = { execFile: async () => { throw new Error("no auth"); } };
  await assert.rejects(fetchArtifacts({ run: "1", out: path.join(tmp(), "o") }, listing), (error) => error.code === "ARTIFACTS_LIST");
});

test("artifactsCommand prints a summary or JSON and returns exit codes", async () => {
  const lines = [];
  const io = { log: (line) => lines.push(line), error: (line) => lines.push(`E ${line}`) };
  const out = path.join(tmp(), "cmd");
  assert.equal(await artifactsCommand(["fetch", "--run", "5", "--out", out, "--name", "swiftright-render-*", "--json"], fakeGh(RUN), io), 0);
  assert.equal(JSON.parse(lines[0]).run, "5");
  assert.equal(await artifactsCommand(["fetch", "--run", "5"], fakeGh(RUN), io), 2);
  assert.equal(await artifactsCommand(["bogus"], fakeGh(RUN), io), 2);
  assert.equal(await artifactsCommand(["fetch", "--run", "5", "--out", path.join(tmp(), "x"), "--name", "none-*"], fakeGh(RUN), io), 1);
  assert.match(lines.at(-1), /ARTIFACTS_NONE/);
});

test("a destination symlink (directory or file) is refused and nothing outside the output root is written", async () => {
  const root = tmp();
  const outside = path.join(root, "outside");
  fs.mkdirSync(outside);
  const out = path.join(root, "out");
  fs.mkdirSync(out);
  fs.symlinkSync(outside, path.join(out, "sub"));
  const { execFile } = fakeGh(RUN);
  await assert.rejects(fetchArtifacts({ run: "9", out, name: "results-win" }, { execFile }), (error) => error.code === "ARTIFACTS_DEST_SYMLINK");
  assert.deepEqual(fs.readdirSync(outside), []);
  fs.rmSync(path.join(out, "sub"));
  const victim = path.join(root, "victim.txt");
  fs.writeFileSync(victim, "keep");
  fs.symlinkSync(victim, path.join(out, "results-win.jsonl"));
  await assert.rejects(fetchArtifacts({ run: "9", out, name: "results-win" }, { execFile }), (error) => error.code === "ARTIFACTS_DEST_SYMLINK");
  assert.equal(fs.readFileSync(victim, "utf8"), "keep");
  const linkedOut = path.join(root, "linked");
  fs.symlinkSync(outside, linkedOut);
  await assert.rejects(fetchArtifacts({ run: "9", out: linkedOut, name: "results-win" }, { execFile }), (error) => error.code === "ARTIFACTS_DEST_SYMLINK");
  assert.deepEqual(fs.readdirSync(outside), []);
});

test("downloads use the listed artifact id: a same-name replacement cannot change what the receipt names", async () => {
  const out = path.join(tmp(), "ids");
  const gh = fakeGh({ ...RUN, replacement: {}, });
  const receipt = await fetchArtifacts({ run: "9", out, name: "swiftright-render-*" }, gh);
  assert.deepEqual(receipt.artifacts.map((artifact) => artifact.id), [1]);
  await assert.rejects(fetchArtifacts({ run: "9", out: path.join(tmp(), "n"), name: "results-mac" }, fakeGh({ ...RUN, artifacts: [{ name: "results-mac" }] })), (error) => error.code === "ARTIFACTS_ID");
});

test("a receipt path that collides with a fetched file is refused before anything is written", async () => {
  const root = tmp();
  const out = path.join(root, "out");
  const { execFile } = fakeGh(RUN);
  await assert.rejects(fetchArtifacts({ run: "9", out, name: "swiftright-render-*", expectSha256: sha("dll-bytes"), file: "render.dll", receipt: path.join(out, "render.dll") }, { execFile }), (error) => error.code === "ARTIFACTS_RECEIPT_COLLISION");
  assert.equal(fs.existsSync(out), false);
  const receipt = await fetchArtifacts({ run: "9", out, name: "swiftright-render-*", receipt: path.join(out, "receipt.json") }, { execFile });
  assert.equal(receipt.fileCount, 1);
});
