import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { previousPathOf, replaceFileInPlace, stagingPathOf, swapInPlace } from "./in-place-install.mjs";
import { stageDevelopmentBinary, stagedBinaryPath } from "./stage-binary.mjs";
import { assertStableDesignatedRequirement } from "./development-signing.mjs";

const temp = (t) => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rightkit-in-place-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  return root;
};

test("fixed staging and previous names never carry a pid, digest or version", () => {
  assert.equal(previousPathOf("/Apps/Pulse.app"), "/Apps/Pulse.app.previous");
  assert.equal(stagingPathOf("/Apps/Pulse.app"), "/Apps/Pulse.app.staging");
});

test("swapInPlace retains NO .previous: the park is transient, deleted on success, renamed back on failure", (t) => {
  const root = temp(t);
  const dest = path.join(root, "App");
  const write = (dir, text) => { fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "marker"), text); };
  write(dest, "v1");
  write(previousPathOf(dest), "stale-retained");
  write(stagingPathOf(dest), "v2");
  assert.deepEqual(swapInPlace({ staging: stagingPathOf(dest), destination: dest, previous: previousPathOf(dest) }), { destination: dest, previous: null });
  write(stagingPathOf(dest), "v3");
  swapInPlace({ staging: stagingPathOf(dest), destination: dest, previous: previousPathOf(dest) });
  assert.equal(fs.readFileSync(path.join(dest, "marker"), "utf8"), "v3");
  assert.deepEqual(fs.readdirSync(root).sort(), ["App"], "no retained .previous after a verified swap");
  write(stagingPathOf(dest), "v4");
  assert.throws(() => swapInPlace({ staging: stagingPathOf(dest), destination: dest, previous: previousPathOf(dest), verify: () => { throw new Error("bad build"); } }), /bad build/);
  // The transiently parked copy is back at the fixed path; the failed staging copy and park are gone.
  assert.equal(fs.readFileSync(path.join(dest, "marker"), "utf8"), "v3");
  assert.deepEqual(fs.readdirSync(root), ["App"]);
  // Default (no previous argument) also keeps nothing.
  write(stagingPathOf(dest), "v5");
  swapInPlace({ staging: stagingPathOf(dest), destination: dest });
  assert.equal(fs.readFileSync(path.join(dest, "marker"), "utf8"), "v5");
  assert.deepEqual(fs.readdirSync(root), ["App"]);
});

test("swapInPlace with previous null replaces in place and keeps NO old copy, even when verification fails", (t) => {
  const root = temp(t);
  const dest = path.join(root, "App");
  const write = (dir, text) => { fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "marker"), text); };
  write(dest, "v1");
  write(previousPathOf(dest), "stale-previous");
  write(stagingPathOf(dest), "v2");
  assert.deepEqual(swapInPlace({ staging: stagingPathOf(dest), destination: dest, previous: null }), { destination: dest, previous: null });
  assert.equal(fs.readFileSync(path.join(dest, "marker"), "utf8"), "v2");
  assert.deepEqual(fs.readdirSync(root), ["App"], "no .previous (a stale one is removed) and no staging");
  write(stagingPathOf(dest), "v3");
  assert.throws(() => swapInPlace({ staging: stagingPathOf(dest), destination: dest, previous: null, verify: () => { throw new Error("bad build"); } }), /bad build/);
  assert.deepEqual(fs.readdirSync(root), [], "the failed copy and staging are gone and the old copy is NOT restored");
});

test("replaceFileInPlace installs a file at one fixed path with NO .previous", (t) => {
  const root = temp(t);
  const dest = path.join(root, "bin", "tool");
  fs.mkdirSync(path.dirname(dest), { recursive: true });
  fs.writeFileSync(`${dest}.previous`, "stale");
  for (const [n, text] of [[1, "one"], [2, "two"], [3, "three"]]) {
    const from = path.join(root, `build-${n}`);
    fs.writeFileSync(from, text);
    replaceFileInPlace(from, dest);
  }
  assert.equal(fs.readFileSync(dest, "utf8"), "three");
  assert.deepEqual(fs.readdirSync(path.dirname(dest)), ["tool"]);
});

test("a staged dev or QA binary lives at one stable path whatever generation built it", (t) => {
  const root = temp(t);
  const generation = (n, text) => {
    const file = path.join(root, "ws", "abc", "g", String(n), "target", "debug", "myapp");
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, text);
    return file;
  };
  const first = stageDevelopmentBinary({ root, source: generation(1, "gen1"), triple: "aarch64-apple-darwin" });
  const second = stageDevelopmentBinary({ root, source: generation(2, "gen2"), triple: "aarch64-apple-darwin" });
  assert.equal(first, second);
  assert.equal(first, stagedBinaryPath({ root, name: "myapp", triple: "aarch64-apple-darwin" }));
  assert.equal(fs.readFileSync(second, "utf8"), "gen2");
  assert.equal(fs.existsSync(`${second}.previous`), false, "no backup of the old staged binary");
  assert.ok(!second.includes(`${path.sep}g${path.sep}`), "never a broker generation path");
  assert.throws(() => stagedBinaryPath({ root, name: "../escape" }), /bare file name/);
});

test("dev signing fails when the designated requirement differs from the installed copy", () => {
  const requirement = (text) => () => ({ status: 0, stdout: `designated => ${text}\n`, stderr: "" });
  const stable = 'identifier "app.fixture" and anchor apple generic and certificate leaf[subject.OU] = "6KLGD3LLKF"';
  assert.equal(assertStableDesignatedRequirement({ root: ".", installed: "/Apps/F.app", expected: stable, run: requirement(stable) }), stable);
  assert.throws(
    () => assertStableDesignatedRequirement({ root: ".", installed: "/Apps/F.app", expected: stable, run: requirement('identifier "app.fixture" and anchor apple generic and certificate leaf[subject.OU] = "OTHER"') }),
    (error) => error.code === "DEV_MAC_SIGNING_REQUIREMENT_CHANGED",
  );
  // An old ad hoc (cdhash) or unsigned install carries no stable grants, so it does not block the move.
  assert.equal(assertStableDesignatedRequirement({ root: ".", installed: "/Apps/F.app", expected: stable, run: requirement('cdhash H"abcd"') }), null);
  assert.equal(assertStableDesignatedRequirement({ root: ".", installed: "/Apps/F.app", expected: stable, run: () => ({ status: 1, stdout: "", stderr: "not signed" }) }), null);
});
