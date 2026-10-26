// The engine licence path has one pattern: the owner hands the engine a snapshot over rightkit_credentials::handoff
// (rightkit#886, C3-05). Before it, rightkit-license's README presented the engine reading the owner's credential store
// (CredentialTokenSource::from_record_store) while rightkit-credentials' README said only the owner binary reads it. This
// test keeps the two documents, and the deprecation in the source, from drifting apart again.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

const read = (rel) => readFileSync(fileURLToPath(new URL(`../../${rel}`, import.meta.url)), "utf8");
const licence = read("crates/rightkit-license/README.md");
const credentials = read("crates/rightkit-credentials/README.md");

test("the licence README names the snapshot and never offers from_record_store except as deprecated", () => {
  for (const name of ["engine_snapshot", "answer_engine_request", "EngineSnapshotCell", "EngineEntitlements::suite_snapshot", "licence.token", "licence.device", "licence.clock"]) {
    assert.ok(licence.includes(name), `crates/rightkit-license/README.md names ${name}`);
  }
  const offering = licence.split("\n").filter((line) => line.includes("from_record_store") && !/deprecated/i.test(line));
  assert.deepEqual(offering, [], "every mention of from_record_store says it is deprecated");
});

test("the credentials README still says only the owner reads the store, and points at the snapshot", () => {
  assert.match(credentials, /Only the owner binary reads the OS store/);
  for (const name of ["licence.token", "licence.device", "licence.clock", "engine_snapshot", "EngineEntitlements::from_snapshot"]) {
    assert.ok(credentials.includes(name), `crates/rightkit-credentials/README.md names ${name}`);
  }
});

test("from_record_store carries #[deprecated] and the three handoff names match the documents", () => {
  const reader = read("crates/rightkit-license/src/client/reader.rs");
  assert.match(reader, /#\[deprecated\([\s\S]*?\)\]\s*pub fn from_record_store\(/);
  const snapshot = read("crates/rightkit-license/src/engine_snapshot.rs");
  for (const [constant, name] of [["SNAPSHOT_TOKEN", "licence.token"], ["SNAPSHOT_DEVICE", "licence.device"], ["SNAPSHOT_CLOCK", "licence.clock"]]) {
    assert.match(snapshot, new RegExp(`pub const ${constant}: &str = "${name.replace(".", "\\.")}";`), `${constant} is ${name}`);
  }
});
