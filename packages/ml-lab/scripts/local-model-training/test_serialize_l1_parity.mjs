import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { platform } from "node:os";
import test from "node:test";
import { serializeL1 } from "./serialize_l1.mjs";

const fixturesPath = fileURLToPath(new URL("./l1c1_fixtures.jsonl", import.meta.url));
const fixtureLines = readFileSync(fixturesPath, "utf8")
  .split(/\r?\n/u)
  .filter((line) => line.trim().length > 0)
  .map((line) => JSON.parse(line));

test("serializeL1 matches the frozen fixture byte-for-byte on every row", () => {
  assert.equal(fixtureLines.length, 16, "fixture file must have 16 rows");
  fixtureLines.forEach(({ row, expected }, index) => {
    assert.equal(serializeL1(row), expected, `fixture ${index} diverged`);
  });
});

test("every output starts with the L1C1 wire marker and carries t", () => {
  for (const { row } of fixtureLines) {
    const out = serializeL1(row);
    assert.match(out, /^L1C1\n\{/u);
    const body = JSON.parse(out.slice("L1C1\n".length));
    assert.equal(typeof body.t, "string");
  }
});

test("python and node serializers are byte-identical on all fixtures", () => {
  const python = platform() === "win32" ? ["py", "-3.11"] : ["python3"];
  const script = fileURLToPath(new URL("./serialize_l1.py", import.meta.url));
  const rows = fixtureLines.map(({ row }) => row);
  const result = spawnSync(python[0], [...python.slice(1), script], {
    input: rows.map((row) => JSON.stringify(row)).join("\n"),
    encoding: "utf8",
  });
  assert.equal(result.status, 0, result.stderr);
  const fromPython = result.stdout.trim().split(/\r?\n/u).map((line) => JSON.parse(line));
  assert.equal(fromPython.length, rows.length);
  rows.forEach((row, index) => assert.equal(fromPython[index], serializeL1(row), `fixture ${index} diverged`));
});
