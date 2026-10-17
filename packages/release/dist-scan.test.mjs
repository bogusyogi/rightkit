import assert from "node:assert/strict";
import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { distScanFailed, main, parseArgs, parseForbidFile, scanDist } from "./dist-scan.mjs";
import { tempRoot } from "./test-support/temp-roots.mjs";

function dist(files) {
  const root = tempRoot("dist-scan-");
  for (const [rel, body] of Object.entries(files)) {
    const full = join(root, rel);
    mkdirSync(join(full, ".."), { recursive: true });
    writeFileSync(full, body);
  }
  return root;
}

test("clean dist passes", () => {
  const root = dist({ "index.html": "<html>", "assets/app-1.js": "console.log(1)" });
  const r = scanDist(root, { forbid: ["Injected read failure"] });
  assert.deepEqual(r.offenders, []);
  assert.equal(distScanFailed(r), false);
  assert.equal(main(["--dist", root, "--forbid", "Injected read failure"]), 0);
});

test("markers in text files and mock chunk names are offenders", () => {
  const root = dist({ "assets/app.js": "x Injected read failure y", "assets/fixtures-ab12.js": "ok", "assets/style.css": "a{}", "a.png": "Injected read failure" });
  const r = scanDist(root, { forbid: ["Injected read failure"] });
  assert.deepEqual(r.offenders.map((o) => o.reason).sort(), ["forbidden chunk name", "marker: Injected read failure"]);
  assert.equal(distScanFailed(r), true);
  assert.equal(main(["--dist", root, "--forbid", "Injected read failure"]), 1);
});

test("forbid-name adds a chunk pattern and forbid-file reads text and JSON", () => {
  const root = dist({ "assets/seed-data.js": "x" });
  const file = join(tempRoot("dist-scan-forbid-"), "forbid.txt");
  writeFileSync(file, "# c\nfoo\n\nbar\n");
  assert.deepEqual(parseForbidFile("foo\n# c\nbar\n"), { forbid: ["foo", "bar"], forbidNames: [] });
  assert.deepEqual(parseForbidFile('{"forbid":["a"],"forbidNames":["^seed"]}'), { forbid: ["a"], forbidNames: ["^seed"] });
  assert.deepEqual(parseArgs(["--dist", root, "--forbid-file", file, "--forbid", "z"]).forbid, ["foo", "bar", "z"]);
  const r = scanDist(root, { forbid: ["q"], forbidNames: ["^seed-"] });
  assert.equal(r.offenders.length, 1);
});

test("an invalid --forbid-name pattern is a usage error", () => {
  const root = dist({ "a.js": "x" });
  assert.equal(main(["--dist", root, "--forbid", "a", "--forbid-name", "("]), 2);
});

test("usage errors, missing dist and empty dist fail closed", () => {
  assert.equal(main([]), 2);
  assert.equal(main(["--dist", join(tempRoot("dist-scan-missing-"), "no-such-dist"), "--forbid", "a"]), 2);
  const root = dist({});
  assert.equal(main(["--dist", root]), 2);
  assert.equal(main(["--dist", root, "--forbid", "a"]), 1);
});
