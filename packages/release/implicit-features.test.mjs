import { test } from "node:test";
import assert from "node:assert/strict";
import { allFeatureNamesOf } from "./feature-coverage.mjs";
import { rustCratePlan } from "./scan-digest.mjs";

const checkKept = (manifest) => rustCratePlan({}, undefined, "c", allFeatureNamesOf(manifest)).checkSets.some((s) => s.length === 1 && s[0] === "--all-features");

test("NN27: an optional dependency is a feature --all-features turns on, so its check is kept (astra62 review 2026-11-18)", () => {
  // Only an optional dependency, no [features]: one implicit feature; the default clippy set does not turn it on, so the check stays.
  const onlyOptional = "[package]\nname = \"c\"\n[dependencies]\nserde = { version = \"1\", optional = true }\n";
  assert.deepEqual(allFeatureNamesOf(onlyOptional), ["serde"]);
  assert.equal(checkKept(onlyOptional), true);
  // Named as dep:serde by a feature: no implicit feature; the declared one counts.
  assert.deepEqual(allFeatureNamesOf("[features]\njson = [\"dep:serde\"]\n[dependencies]\nserde = { version = \"1\", optional = true }\n"), ["json"]);
  // A [dependencies.x] table, a target-specific table, a build dependency.
  assert.deepEqual(allFeatureNamesOf("[dependencies.foo]\nversion = \"1\"\noptional = true\n"), ["foo"]);
  assert.deepEqual(allFeatureNamesOf("[target.'cfg(unix)'.dependencies]\nnix = { version = \"0.29\", optional = true }\n"), ["nix"]);
  assert.deepEqual(allFeatureNamesOf("[build-dependencies]\ncc = { version = \"1\", optional = true }\n"), ["cc"]);
  // A form this reader does not parse (an inline table over several lines): null, and a null drops no check.
  const multi = "[dependencies]\nserde = {\n  version = \"1\",\n  optional = true,\n}\n";
  assert.equal(allFeatureNamesOf(multi), null);
  assert.equal(checkKept(multi), true);
  // A crate with no features and no optional dependency still drops its redundant check (NN27's intent).
  assert.deepEqual(allFeatureNamesOf("[package]\nname = \"c\"\n[dependencies]\nlibc = \"0.2\"\n"), []);
  assert.equal(checkKept("[package]\nname = \"c\"\n[dependencies]\nlibc = \"0.2\"\n"), false);
});
