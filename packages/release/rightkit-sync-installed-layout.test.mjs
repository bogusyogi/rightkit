// Installed-layout regression (@rightkit/release 0.4.1): a published release runs from a package store, where the
// module-location defaults of rightkit-sync.mjs point inside the store. Every publish call site must hand the
// requested workspace's feed file and root to recordPublished, so only that repo's feed and consumer copies move.
import assert from "node:assert/strict";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { publishGatedTarball } from "./publication-gate.mjs";
import { syncConsumerCopies, workspaceFeedOptions } from "./rightkit-sync.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const NAME = "@rightkit/release";

function makeWorkspace(base, label, versions) {
  const root = path.join(base, label);
  const release = path.join(root, "packages", "release");
  fs.mkdirSync(path.join(root, "crates"), { recursive: true });
  fs.mkdirSync(path.join(root, "packages", "build-control-policy", "src"), { recursive: true });
  fs.mkdirSync(release, { recursive: true });
  fs.writeFileSync(path.join(release, "rightkit-versions.json"), `${JSON.stringify(versions, null, 2)}\n`);
  fs.writeFileSync(path.join(release, "rightkit-ownership.json"), '{"capabilities":[]}\n');
  for (const name of ["rightkit-drift.mjs", "background-process.mjs", "ownership-source.mjs", "ownership-availability.mjs", "ownership-dependency-allowlist.mjs"]) fs.writeFileSync(path.join(release, name), "export {};\n");
  fs.writeFileSync(path.join(release, "ownership-dependency-allowlist.json"), '{"schema":1}\n');
  fs.writeFileSync(path.join(release, "ownership-repositories.json"), '{"repositories":[]}\n');
  syncConsumerCopies(root);
  return root;
}

function snapshot(dir) {
  const entries = {};
  const walk = (current) => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const absolute = path.join(current, entry.name);
      if (entry.isDirectory()) walk(absolute);
      else entries[path.relative(dir, absolute).split(path.sep).join("/")] = fs.readFileSync(absolute).toString("base64");
    }
  };
  walk(dir);
  return entries;
}

test("installed layout: recordPublished updates only the requested workspace feed and consumer copies", (context) => {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "rk-installed-")));
  context.after(() => fs.rmSync(base, { recursive: true, force: true }));
  // The module lives in a store directory outside every workspace, as under pnpm's content store.
  const moduleDir = path.join(base, "store", "links", "@rightkit", "release", "0.4.0", "node_modules", "@rightkit", "release");
  fs.mkdirSync(moduleDir, { recursive: true });
  fs.copyFileSync(path.join(here, "rightkit-sync.mjs"), path.join(moduleDir, "rightkit-sync.mjs"));
  const runnerDir = path.join(base, "runner");
  fs.mkdirSync(runnerDir);
  const runner = path.join(runnerDir, "record.mjs");
  fs.writeFileSync(runner, [
    "const [moduleUrl, workspace] = process.argv.slice(2);",
    "const sync = await import(moduleUrl);",
    `sync.recordPublished({ registry: "npm", name: "${NAME}", version: "0.4.1" }, { ...sync.workspaceFeedOptions(workspace), strict: true, log: () => {} });`,
    "",
  ].join("\n"));

  const requested = makeWorkspace(base, "requested", { stagedNpm: { [NAME]: "0.4.0" }, registry: { npm: { [NAME]: "0.4.0" } } });
  const other = makeWorkspace(base, "other", { stagedNpm: { [NAME]: "0.4.0" }, registry: { npm: { [NAME]: "0.4.0" } } });
  const storeBefore = snapshot(path.join(base, "store"));
  const otherBefore = snapshot(other);

  const result = spawnSync(process.execPath, [runner, pathToFileURL(path.join(moduleDir, "rightkit-sync.mjs")).href, requested], { encoding: "utf8" });
  assert.equal(result.status, 0, `${result.stderr}${result.stdout}`);

  const feed = JSON.parse(fs.readFileSync(path.join(requested, "packages", "release", "rightkit-versions.json"), "utf8"));
  assert.equal(feed.stagedNpm[NAME], "0.4.1");
  assert.equal(feed.registry.npm[NAME], "0.4.1");
  assert.deepEqual(feed.legacyNpm[NAME], ["0.4.0"]);
  assert.equal(
    fs.readFileSync(path.join(requested, "packages", "build-control-policy", "src", "ownership-session-versions.json"), "utf8"),
    fs.readFileSync(path.join(requested, "packages", "release", "rightkit-versions.json"), "utf8"),
    "the consumer copy in the requested workspace follows the feed",
  );
  assert.deepEqual(snapshot(path.join(base, "store")), storeBefore, "nothing is written under the module directory");
  assert.deepEqual(snapshot(other), otherBefore, "a different workspace is untouched");
});

test("workspaceFeedOptions binds the feed file and root to the requested workspace", () => {
  const root = path.resolve(os.tmpdir(), "rk-requested");
  assert.deepEqual(workspaceFeedOptions(root), { root, file: path.join(root, "packages", "release", "rightkit-versions.json") });
});

test("publishGatedTarball records into the requested workspace feed, not the module default", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rk-gated-"));
  try {
    const tarball = path.join(dir, "x.tgz");
    fs.writeFileSync(tarball, "tarball-bytes");
    const bytes = fs.readFileSync(tarball);
    const evidencePath = path.join(dir, "e.json");
    const sha = (algorithm) => crypto.createHash(algorithm).update(bytes).digest("hex");
    fs.writeFileSync(evidencePath, JSON.stringify({ artifactSha256: sha("sha256") }));
    const spawn = (_command, argv) => (argv[0] === "publish" ? { status: 0, stdout: "ok", stderr: "" } : { status: 0, stdout: `${sha("sha1")}\n` });
    const calls = [];
    const workspaceRoot = path.join(dir, "workspace");
    publishGatedTarball({ name: "@rightkit/x", version: "1.0.0", tarball, evidencePath, workspaceRoot }, {
      readToken: () => "SECRET", spawnSync: spawn, sleep: () => {},
      recordPublished: (entry, options) => calls.push({ entry, options }),
    });
    assert.deepEqual(calls, [{ entry: { registry: "npm", name: "@rightkit/x", version: "1.0.0" }, options: workspaceFeedOptions(workspaceRoot) }]);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("no release source calls recordPublished without the requested workspace binding", () => {
  const offenders = [];
  for (const file of fs.readdirSync(here).filter((name) => name.endsWith(".mjs") && !name.endsWith(".test.mjs") && name !== "rightkit-sync.mjs")) {
    const text = fs.readFileSync(path.join(here, file), "utf8");
    for (const match of text.matchAll(/recordPublished\)?\(\{[^\n]*$/gm)) {
      if (!/workspaceFeedOptions\(|workspaceRoot \? workspaceFeedOptions/.test(match[0])) offenders.push(`${file}: ${match[0].trim().slice(0, 80)}`);
    }
  }
  assert.deepEqual(offenders, []);
});
