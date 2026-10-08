import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "./background-process.mjs";
import { capabilityKeywords, formatStagedReport, main, newManifestAdvisories, scanStaged } from "./ownership-staged.mjs";

const TRUST = { trust: null, enrolled: false, pending: true };
const blockManifest = {
  schema: 1, version: "t", internalOwners: [],
  capabilities: [
    { package: "rightkit-audio", capability: "audio", fingerprints: [{ id: "audio.src.fake", type: "source", pattern: "fakeaudio::", codeOnly: true, extensions: [".rs"], tier: "block", usageContext: "test", tierReason: "test", policy: "test" }] },
  ],
};
const adviseManifest = {
  schema: 1, version: "t", internalOwners: [],
  capabilities: [
    { package: "rightkit-audio", capability: "audio", fingerprints: [{ id: "audio.src.fake", type: "source", pattern: "fakeaudio::", codeOnly: true, extensions: [".rs"], tier: "advise", usageContext: "test", tierReason: "test" }] },
  ],
};
const keywordManifest = {
  schema: 1, version: "t", internalOwners: [],
  capabilities: [
    { package: "rightkit-http", capability: "http-client", fingerprints: [] },
    { package: "rightkit-audio", capability: "audio", fingerprints: [] },
  ],
};

function git(cwd, args) {
  const result = spawnSync("git", ["-C", cwd, ...args], { encoding: "utf8", windowsHide: true, env: { ...process.env, GIT_TERMINAL_PROMPT: "0" } });
  assert.equal(result.status, 0, `git ${args.join(" ")}: ${result.stderr}`);
  return String(result.stdout);
}

function fixtureRepo(t) {
  const dir = realpathSync(mkdtempSync(path.join(os.tmpdir(), "rk-staged-")));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  git(dir, ["init", "-q", "."]);
  git(dir, ["config", "user.email", "fixture@example.invalid"]);
  git(dir, ["config", "user.name", "fixture"]);
  git(dir, ["config", "core.hooksPath", path.join(dir, ".no-hooks")]);
  writeFileSync(path.join(dir, "lib.rs"), "fn main() {}\n");
  git(dir, ["add", "lib.rs"]);
  git(dir, ["commit", "-qm", "init"]);
  return dir;
}

test("staged gate blocks on staged block-tier content even when the working tree is clean", (t) => {
  const repo = fixtureRepo(t);
  writeFileSync(path.join(repo, "lib.rs"), "fn main() { fakeaudio::open(); }\n");
  git(repo, ["add", "lib.rs"]);
  writeFileSync(path.join(repo, "lib.rs"), "fn main() {}\n");
  const result = scanStaged(repo, { manifest: blockManifest, trust: TRUST });
  assert.equal(result.ok, false);
  assert.equal(result.blocking.length, 1);
  assert.equal(result.blocking[0].file, "lib.rs");
  assert.equal(result.blocking[0].fingerprint, "audio.src.fake");
});

test("staged gate passes when the staged content is clean even if the working tree carries the finding", (t) => {
  const repo = fixtureRepo(t);
  writeFileSync(path.join(repo, "lib.rs"), "fn main() {}\n");
  git(repo, ["add", "lib.rs"]);
  writeFileSync(path.join(repo, "lib.rs"), "fn main() { fakeaudio::open(); }\n");
  const result = scanStaged(repo, { manifest: blockManifest, trust: TRUST });
  assert.equal(result.ok, true);
  assert.equal(result.blocking.length, 0);
});

test("advise-tier findings print a summary and pass", (t) => {
  const repo = fixtureRepo(t);
  writeFileSync(path.join(repo, "lib.rs"), "fn main() { fakeaudio::open(); }\n");
  git(repo, ["add", "lib.rs"]);
  const result = scanStaged(repo, { manifest: adviseManifest, trust: TRUST });
  assert.equal(result.ok, true);
  assert.equal(result.advisory.length, 1);
  const printed = [];
  const status = main(["--app", repo], { print: (line) => printed.push(line), scanner: () => result });
  assert.equal(status, 0);
  assert.match(printed.join("\n"), /1 advise/);
  assert.match(printed.join("\n"), /passed/);
});

test("staged scan leaves HEAD, the index, and the working tree untouched", (t) => {
  const repo = fixtureRepo(t);
  writeFileSync(path.join(repo, "lib.rs"), "fn main() { fakeaudio::open(); }\n");
  git(repo, ["add", "lib.rs"]);
  writeFileSync(path.join(repo, "extra.rs"), "untracked\n");
  const before = { head: git(repo, ["rev-parse", "HEAD"]), status: git(repo, ["status", "--porcelain"]), index: readFileSync(path.join(repo, ".git", "index")) };
  scanStaged(repo, { manifest: blockManifest, trust: TRUST });
  assert.equal(git(repo, ["rev-parse", "HEAD"]), before.head);
  assert.equal(git(repo, ["status", "--porcelain"]), before.status);
  assert.deepEqual(readFileSync(path.join(repo, ".git", "index")), before.index);
});

test("the gate fails closed with a message outside a Git repository", () => {
  const printed = [];
  const status = main(["--app", os.tmpdir()], { print: (line) => printed.push(line), scanner: () => { throw new Error("must not scan"); } });
  assert.equal(status, 1);
  assert.match(printed.join("\n"), /not inside a Git repository|FAILED/);
});

test("new manifest keyword advisory fires for an undeclared owner match and clears once the owner is declared", (t) => {
  const repo = fixtureRepo(t);
  mkdirSync(path.join(repo, "svc"));
  writeFileSync(path.join(repo, "svc", "package.json"), JSON.stringify({ name: "acme-svc", description: "Our own http-client wrapper" }));
  git(repo, ["add", "svc/package.json"]);
  const undeclared = newManifestAdvisories(repo, keywordManifest);
  assert.equal(undeclared.length, 1);
  assert.equal(undeclared[0].file, "svc/package.json");
  assert.equal(undeclared[0].capability, "http-client");
  assert.match(undeclared[0].message, /rightkit-http/);

  writeFileSync(path.join(repo, "svc", "package.json"), JSON.stringify({ name: "acme-svc", description: "Our own http-client wrapper", dependencies: { "rightkit-http": "1.0.0" } }));
  git(repo, ["add", "svc/package.json"]);
  assert.deepEqual(newManifestAdvisories(repo, keywordManifest), []);
});

test("keyword advisory ignores manifests that are not newly added and the owner's own package", (t) => {
  const repo = fixtureRepo(t);
  writeFileSync(path.join(repo, "package.json"), JSON.stringify({ name: "rightkit-http", description: "http-client owner" }));
  git(repo, ["add", "package.json"]);
  assert.deepEqual(newManifestAdvisories(repo, keywordManifest), []);
  git(repo, ["commit", "-qm", "owner"]);
  writeFileSync(path.join(repo, "package.json"), JSON.stringify({ name: "other", description: "http-client wrapper" }));
  git(repo, ["add", "package.json"]);
  assert.deepEqual(newManifestAdvisories(repo, keywordManifest), []);
});

test("capability keywords match hyphen, space and underscore spellings only as whole words", () => {
  const [http] = capabilityKeywords(keywordManifest);
  assert.equal(http.pattern.test("uses http client"), true);
  assert.equal(http.pattern.test("http_client"), true);
  assert.equal(http.pattern.test("myhttp-clientele"), false);
});

test("report lists block findings and the failing verdict", () => {
  const text = formatStagedReport({
    ok: false, blocking: [{ fingerprint: "f1", package: "rightkit-audio", file: "lib.rs", line: 3, text: "fakeaudio::open" }],
    advisory: [], keywords: [], problems: [], warnings: [], coverage: [], covered: 0,
  });
  assert.match(text, /BLOCK f1 \(rightkit-audio\) lib\.rs:3/);
  assert.match(text, /FAILED/);
});
