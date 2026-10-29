import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { loadCandidateSource } from "./candidate-source.mjs";

const SHA = "a".repeat(40); const TREE = "b".repeat(40); const PROMOTION = "c".repeat(40);
const blobId = (text) => crypto.createHash("sha1").update(`blob ${Buffer.byteLength(text)}\0`).update(text).digest("hex");

// Real files + Git protocol responses. No candidate command, network or installation runs.
function fixture(t, { files: additions = {}, responses = {} } = {}) {
  const scratch = process.env.RUNNER_TEMP ?? (process.platform === "win32" ? "D:\\scratch" : os.tmpdir());
  fs.mkdirSync(scratch, { recursive: true });
  // macOS's tmpdir is /var/folders/..., a symlink to /private/var: the candidate root must be a real path.
  const directory = fs.mkdtempSync(path.join(fs.realpathSync(scratch), "candidate-source-"));
  t.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  fs.mkdirSync(path.join(directory, ".git"));
  fs.writeFileSync(path.join(directory, ".git", "config"), "[core]\nrepositoryformatversion = 0\n");
  const files = {
    "package.json": '{"name":"fixture","private":true}',
    "pnpm-lock.yaml": 'lockfileVersion: "9.0"\n',
    "pnpm-workspace.yaml": "packages:\n  - packages/*\n  - '!packages/parked'\n",
    "ci/gate.config.json": "{}",
    "ci/asset-gated.json": '{"crates":{},"npm":{}}',
    // Every crate is its own Cargo workspace with its own Cargo.lock; there is no root manifest or lock (#1285).
    "crates/one/Cargo.toml": '[package]\nname = "rightkit-one"\nversion = "0.1.0"\n\n[workspace]\n',
    "crates/one/Cargo.lock": 'version = 4\n[[package]]\nname = "rightkit-one"\nversion = "0.1.0"\n',
    "packages/one/package.json": '{"name":"@rightkit/one","scripts":{"test":"node --test"}}',
    "packages/parked/package.json": '{"name":"@rightkit/parked","scripts":{"test":"node --test"}}',
    ...additions,
  };
  const blobs = new Map();
  for (const [name, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(directory, name)), { recursive: true });
    fs.writeFileSync(path.join(directory, name), text);
    blobs.set(blobId(text), text);
  }
  const calls = [];
  const spawn = (command, args, options) => {
    assert.equal(command, "git");
    assert.deepEqual(args.slice(0, 6), ["-c", "core.hooksPath=", "-c", "core.fsmonitor=false", "-c", "core.untrackedCache=false"]);
    assert.equal(options.shell, false);
    assert.equal(options.windowsHide, true);
    assert.equal(options.env.GIT_NO_REPLACE_OBJECTS, "1");
    const query = args.slice(6); calls.push(query);
    let stdout = ""; let status = 0;
    const key = query.join(" ");
    if (Object.hasOwn(responses, key)) {
      const response = responses[key];
      if (typeof response === "object") return response;
      stdout = response;
    } else if (key === "rev-parse --show-toplevel") stdout = fs.realpathSync(directory);
    else if (key === "rev-parse --verify HEAD^{commit}") stdout = SHA;
    else if (key === "rev-parse --is-shallow-repository") stdout = "false";
    else if (key === `rev-parse --verify ${SHA}^{tree}`) stdout = TREE;
    else if (query[0] === "merge-base" || query[0] === "status" || key === "ls-files --others -z") { /* clean */ }
    else if (key === "ls-files -v -z") stdout = Object.keys(files).map((name) => `H ${name}\0`).join("");
    else if (query[0] === "ls-tree") stdout = Object.entries(files).map(([name, text]) => `100644 blob ${blobId(text)}\t${name}\0`).join("");
    else if (query[0] === "cat-file") {
      assert.ok(blobs.has(query[2]), "reads only inventoried blobs");
      stdout = query[1] === "-s" ? String(Buffer.byteLength(blobs.get(query[2]))) : blobs.get(query[2]);
    } else if (query[0] === "grep") {
      stdout = Object.entries(files).filter(([name, text]) => /^(crates|packages)\//.test(name) && text.includes("RIGHTKIT_REQUIRE_ASSETS")).map(([name]) => `${SHA}:${name}\0`).join("");
      if (!stdout) status = 1;
    } else assert.fail(`unexpected Git command ${key}`);
    return { status, stdout: Buffer.from(stdout), stderr: Buffer.alloc(0) };
  };
  return { directory, sourceSha: SHA, promotionSha: PROMOTION, registry: "npm", units: ["@rightkit/one"], spawn, calls };
}

test("candidate source authenticates blobs, inventories packages & derives native host obligations", (t) => {
  const input = fixture(t);
  const loaded = loadCandidateSource(input);
  assert.deepEqual(loaded.source, { sha: SHA, tree: TREE });
  assert.equal(loaded.root, fs.realpathSync(input.directory));
  assert.deepEqual(Object.keys(loaded.hosts), ["macos", "windows"]);
  assert.deepEqual(loaded.hosts.macos.selection.npm.test, ["packages/one"]);
  assert.equal(loaded.sourceHashes["pnpm-workspace.yaml"].length, 64);
  assert.ok(loaded.sourceHashes["packages/parked/package.json"], "excluded manifest bytes remain authenticated");
  const original = loaded.readData("package.json");
  assert.ok(Buffer.isBuffer(original));
  assert.equal(original.toString(), '{"name":"fixture","private":true}');
  original.fill(0);
  assert.equal(loaded.readData("package.json").toString(), '{"name":"fixture","private":true}', "returned buffers cannot mutate authenticated source");
  assert.throws(() => loaded.readData("../outside"), /unsafe|missing|untracked|candidate/i);
  assert.ok(input.calls.every((args) => !["checkout", "fetch", "config", "reset"].includes(args[0])));
});

test("candidate cannot be trusted checkout or contain invalid commit identifiers", () => {
  assert.throws(() => loadCandidateSource({ directory: fileURLToPath(new URL("../..", import.meta.url)), sourceSha: SHA, promotionSha: PROMOTION, units: ["@rightkit/one"] }), /separate/);
  assert.throws(() => loadCandidateSource({ sourceSha: "--help", promotionSha: PROMOTION }), /SHA/);
});

for (const [label, query, response, expected] of [
  ["wrong HEAD", "rev-parse --verify HEAD^{commit}", PROMOTION, /HEAD/],
  ["shallow history", "rev-parse --is-shallow-repository", "true", /full history/],
  ["nonancestor", `merge-base --is-ancestor ${SHA} ${PROMOTION}`, { status: 1, stdout: "" }, /ancestor/],
  ["tracked drift", "status --porcelain=v1 --untracked-files=all --ignore-submodules=none", " M crates/one/Cargo.lock", /drift/],
  ["ignored untracked installation", "ls-files --others -z", "node_modules/x\0", /untracked/],
  ["hidden index entry", "ls-files -v -z", "h crates/one/Cargo.lock\0", /hidden/],
  ["symlink blob", `ls-tree -r -z --full-tree ${SHA}`, `120000 blob ${TREE}\tcrates/one/Cargo.toml\0`, /symlink/],
  ["traversal path", `ls-tree -r -z --full-tree ${SHA}`, `100644 blob ${TREE}\t../Cargo.toml\0`, /unsafe/],
  ["oversized output", "rev-parse --verify HEAD^{commit}", { status: 0, stdout: Buffer.alloc(8 * 1024 * 1024 + 1) }, /limit/],
]) test(`candidate rejects ${label}`, (t) => {
  assert.throws(() => loadCandidateSource(fixture(t, { responses: { [query]: response } })), expected);
});

test("excluded package cannot be requested & workspace syntax cannot hide exclusions", (t) => {
  assert.throws(() => loadCandidateSource({ ...fixture(t), units: ["@rightkit/parked"] }), /unknown npm/);
  assert.throws(() => loadCandidateSource(fixture(t, { files: { "pnpm-workspace.yaml": "packages: [packages/*, '!packages/parked']\n" } })), /exclusions/);
});

test("missing manifest, duplicate inventory & undeclared asset reader fail closed", (t) => {
  assert.throws(() => loadCandidateSource(fixture(t, { files: { "packages/two/package.json": '{"name":"@rightkit/one"}' } })), /duplicate/);
  assert.throws(() => loadCandidateSource(fixture(t, { files: { "crates/one/src/lib.rs": 'const KEY: &str = "RIGHTKIT_REQUIRE_ASSETS";' } })), /asset declaration/);
  const input = fixture(t);
  fs.unlinkSync(path.join(input.directory, "crates", "one", "Cargo.lock"));
  assert.throws(() => loadCandidateSource(input), /ENOENT/);
  // A crate manifest without its own lock is refused: the candidate holds no lock to bind that crate to.
  assert.throws(() => loadCandidateSource(fixture(t, { files: { "crates/two/Cargo.toml": '[package]\nname = "rightkit-two"\nversion = "0.1.0"\n\n[workspace]\n' } })), /candidate missing crates\/two\/Cargo\.lock/);
});

test("blob content cannot be substituted despite matching checkout identity", (t) => {
  const text = '{"name":"@rightkit/one","scripts":{"test":"node --test"}}';
  const responses = { [`cat-file blob ${blobId(text)}`]: text.replace("one", "two") };
  assert.throws(() => loadCandidateSource(fixture(t, { responses })), /authentication/);
});

test("executable Git configuration is rejected before invoking Git", (t) => {
  const input = fixture(t);
  fs.writeFileSync(path.join(input.directory, ".git", "config"), '[filter "candidate"]\nclean = node candidate.mjs\n');
  assert.throws(() => loadCandidateSource(input), /Git config/);
  assert.equal(input.calls.length, 0);
});

test("asset gating remains mandatory while publication obligations isolate requested units", (t) => {
  const input = fixture(t, { files: {
    "ci/asset-gated.json": '{"crates":{"rightkit-one":"models"},"npm":{}}',
    "crates/one/src/lib.rs": 'const KEY: &str = "RIGHTKIT_REQUIRE_ASSETS";',
    "crates/two/Cargo.toml": '[package]\nname = "rightkit-two"\nversion = "0.1.0"\n\n[workspace]\n\n[dependencies]\nrightkit-one = { path = "../one", version = "0.1.0" }\n',
    "crates/two/Cargo.lock": '[[package]]\nname = "rightkit-one"\nversion = "0.1.0"\n[[package]]\nname = "rightkit-two"\nversion = "0.1.0"\ndependencies = ["rightkit-one"]\n',
  } });
  const request = { ...input, registry: "crates", units: ["rightkit-one"] };
  assert.throws(() => loadCandidateSource(request), /need assets/);
  const loaded = loadCandidateSource({ ...request, allow: { missingAssets: ["rightkit-one"] } });
  for (const host of Object.values(loaded.hosts)) {
    assert.deepEqual(host.selection.rust.test, ["rightkit-one"]);
    assert.deepEqual(host.selection.rust.check, []);
    assert.ok(!host.commands.some((row) => row.unit === "rust:rightkit-two"));
    assert.equal(host.unitPolicies[0].allowMissingAssets, true);
  }
});

test("reviewed candidate allowsets preserve host exclusion, ignored & asset policies", (t) => {
  const input = fixture(t, { files: {
    "ci/gate.config.json": '{"rust":{"rightkit-one":{"os":["macos"]}}}',
    "ci/asset-gated.json": '{"crates":{"rightkit-one":"models"},"npm":{}}',
  } });
  const allow = { hostExclusion: ["rightkit-one"], noTest: [], ignored: ["rightkit-one"], missingAssets: ["rightkit-one"] };
  const loaded = loadCandidateSource({ ...input, registry: "crates", units: ["rightkit-one"], allow });
  assert.deepEqual(loaded.hosts.windows.selection.excluded, ["rightkit-one"]);
  assert.deepEqual(loaded.hosts.macos.selection.rust.test, ["rightkit-one"]);
  for (const host of Object.values(loaded.hosts)) {
    assert.equal(host.unitPolicies[0].allowIgnored, true);
    assert.equal(host.unitPolicies[0].allowMissingAssets, true);
  }
  assert.equal(loaded.hosts.windows.unitPolicies[0].status, "allowed-exclusion");
  assert.deepEqual(allow, { hostExclusion: ["rightkit-one"], noTest: [], ignored: ["rightkit-one"], missingAssets: ["rightkit-one"] });
});

test("reviewed candidate noTest waiver reaches both host obligations", (t) => {
  const input = fixture(t, { files: { "packages/one/package.json": '{"name":"@rightkit/one"}' } });
  assert.throws(() => loadCandidateSource(input), /no test script/);
  const loaded = loadCandidateSource({ ...input, allow: { hostExclusion: [], noTest: ["@rightkit/one"], ignored: [], missingAssets: [] } });
  for (const host of Object.values(loaded.hosts)) {
    assert.deepEqual(host.selection.noTest, ["@rightkit/one"]);
    assert.equal(host.unitPolicies[0].status, "allowed-no-test");
  }
});

test("candidate allowsets reject unknown keys, malformed lists & names outside request before Git", (t) => {
  const input = fixture(t);
  for (const allow of [null, [], "all", new Date(), Object.create({ ignored: ["@rightkit/one"] }), { allowMissingAssets: [] }, { typo: [] }, { ignored: null }, { ignored: "@rightkit/one" }, { ignored: [3] }, { ignored: Array(1) }, { ignored: [""] }, { ignored: ["with whitespace"] }, { ignored: ["@rightkit/one", "@rightkit/one"] }, { ignored: ["@rightkit/other"] }]) {
    assert.throws(() => loadCandidateSource({ ...input, allow }), /allowset/);
  }
  assert.equal(input.calls.length, 0);
});
