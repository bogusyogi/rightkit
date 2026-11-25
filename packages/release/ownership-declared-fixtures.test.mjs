import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import test from "node:test";
import { declaredFixtureDigests } from "./ownership.mjs";

const sha = (s) => createHash("sha256").update(s).digest("hex");

test("declared fixtures are read from every .rightkit/fixture-files.json, keyed by app-relative path", () => {
  const decl = (files) => Buffer.from(JSON.stringify({ schema: 1, files }));
  const inventory = { files: [
    { rel: ".rightkit/fixture-files.json", bytes: decl({ "engine/tests/fixtures/a/Cargo.toml": { sha256: sha("a"), note: "known-answer input" }, "bad/Cargo.toml": { sha256: "nope" } }) },
    { rel: "sub/.rightkit/fixture-files.json", bytes: decl({ "fixtures/package.json": { sha256: sha("b") } }) },
    { rel: "broken/.rightkit/fixture-files.json", bytes: Buffer.from("{not json") },
    { rel: "engine/Cargo.toml", bytes: Buffer.from("[package]") },
  ] };
  const got = declaredFixtureDigests(inventory);
  assert.deepEqual([...got.entries()].sort(), [["engine/tests/fixtures/a/Cargo.toml", sha("a")], ["sub/fixtures/package.json", sha("b")]]);
});
