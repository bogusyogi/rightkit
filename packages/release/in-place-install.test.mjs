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

test("swapInPlace keeps exactly one .previous and restores it when verification fails", (t) => {
  const root = temp(t);
  const dest = path.join(root, "App");
  const write = (dir, text) => { fs.mkdirSync(dir, { recursive: true }); fs.writeFileSync(path.join(dir, "marker"), text); };
  write(dest, "v1");
  write(stagingPathOf(dest), "v2");
  swapInPlace({ staging: stagingPathOf(dest), destination: dest });
  write(stagingPathOf(dest), "v3");
  swapInPlace({ staging: stagingPathOf(dest), destination: dest });
  assert.equal(fs.readFileSync(path.join(dest, "marker"), "utf8"), "v3");
  assert.equal(fs.readFileSync(path.join(previousPathOf(dest), "marker"), "utf8"), "v2");
  assert.deepEqual(fs.readdirSync(root).sort(), ["App", "App.previous"]);
  write(stagingPathOf(dest), "v4");
  assert.throws(() => swapInPlace({ staging: stagingPathOf(dest), destination: dest, verify: () => { throw new Error("bad build"); } }), /bad build/);
  // The installed copy is back at the fixed path; the failed staging copy is gone.
  assert.equal(fs.readFileSync(path.join(dest, "marker"), "utf8"), "v3");
  assert.deepEqual(fs.readdirSync(root), ["App"]);
});

test("replaceFileInPlace installs a file at one fixed path with a kept .previous", (t) => {
  const root = temp(t);
  const dest = path.join(root, "bin", "tool");
  for (const [n, text] of [[1, "one"], [2, "two"], [3, "three"]]) {
    const from = path.join(root, `build-${n}`);
    fs.writeFileSync(from, text);
    replaceFileInPlace(from, dest);
  }
  assert.equal(fs.readFileSync(dest, "utf8"), "three");
  assert.equal(fs.readFileSync(`${dest}.previous`, "utf8"), "two");
  assert.deepEqual(fs.readdirSync(path.dirname(dest)).sort(), ["tool", "tool.previous"]);
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
  assert.equal(fs.readFileSync(`${second}.previous`, "utf8"), "gen1");
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
