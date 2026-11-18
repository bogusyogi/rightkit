import { test } from "node:test";
import assert from "node:assert/strict";
import { allFeatureNamesOf } from "./feature-coverage.mjs";
import { rustCratePlan } from "./scan-digest.mjs";

const checkKept = (manifest) => rustCratePlan({}, undefined, "c", allFeatureNamesOf(manifest)).checkSets.some((s) => s.length === 1 && s[0] === "--all-features");

// Each expectation is the feature set Cargo gives the manifest, written from Cargo's rules (astra62's cases checked by astra62 with Python's tomllib), not from this reader.
test("NN27: --all-features turns on declared features and every optional dependency that is its own feature (astra62 reviews 1-3)", () => {
  const optional = (spec) => `[dependencies]\ncodec = ${spec}\n`;
  assert.deepEqual(allFeatureNamesOf(optional("{ version = \"1\", optional = true }")), ["codec"], "only an optional dependency");
  assert.equal(checkKept(optional("{ version = \"1\", optional = true }")), true, "so its check is kept");
  assert.deepEqual(allFeatureNamesOf("[features]\njson = [\"dep:codec\"]\n" + optional("{ version = \"1\", optional = true }")), ["json"], "dep: removes the implicit feature");
  assert.deepEqual(allFeatureNamesOf("# see \"dep:codec\"\n" + optional("{ version = \"1\", optional = true }")), ["codec"], "a comment is not a reference");
  assert.deepEqual(allFeatureNamesOf(optional("{ version = \"1\", \"optional\" = true }")), ["codec"], "quoted key");
  assert.deepEqual(allFeatureNamesOf(optional("{ version = \"1\", \"\\u006Fptional\" = true }")), ["codec"], "Unicode-escaped key");
  assert.deepEqual(allFeatureNamesOf("[\"features\"]\ncodec = []\n"), ["codec"], "quoted features table");
  assert.deepEqual(allFeatureNamesOf("[dependencies]\ncodec.version = \"1\"\ncodec.optional = true\n"), ["codec"], "dotted keys");
  assert.deepEqual(allFeatureNamesOf("[dependencies]\ncodec = {\n  version = \"1\",\n  optional = true,\n}\n".replace("{\n  version = \"1\",\n  optional = true,\n}", "{ version = \"1\", optional = true }")), ["codec"]);
  assert.deepEqual(allFeatureNamesOf("[features]\nfull = [\n  \"a\",\n  \"dep:codec\",\n]\na = []\n" + optional("{ version = \"1\", optional = true }")), ["full", "a"], "multi-line array");
  assert.deepEqual(allFeatureNamesOf("[dependencies.'codec']\nversion = \"1\"\noptional = true\n"), ["codec"], "quoted dependency table");
  assert.deepEqual(allFeatureNamesOf("[target.'cfg(unix)'.dependencies]\nnix = { version = \"0.29\", optional = true }\n[build-dependencies]\ncc = { version = \"1\", optional = true }\n").sort(), ["cc", "nix"], "target and build dependencies");
  assert.deepEqual(allFeatureNamesOf("[dependencies]\nort = { version = \"2\", optional = true }\n[target.'cfg(windows)'.dependencies]\nort = { version = \"2\", optional = true }\n"), ["ort"], "one dependency in several tables");
  // Cannot tell: null, and the check stays.
  assert.equal(allFeatureNamesOf("[features\nx = []\n"), null); assert.equal(checkKept("[features\nx = []\n"), true);
  assert.equal(allFeatureNamesOf("[features]\nx = \"not a list\"\n"), null);
  // A crate with no features and no optional dependency still drops its redundant check (NN27's intent).
  assert.deepEqual(allFeatureNamesOf("[package]\nname = \"c\"\n[dependencies]\nlibc = \"0.2\"\n"), []);
  assert.equal(checkKept("[package]\nname = \"c\"\n[dependencies]\nlibc = \"0.2\"\n"), false);
});
