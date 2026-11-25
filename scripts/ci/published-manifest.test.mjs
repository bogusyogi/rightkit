import assert from "node:assert/strict";
import test from "node:test";
import { effectiveRequirements, frozenManifestProblems, packageIdentity, rightkitDependencies } from "./published-manifest.mjs";

const root = (fs) => `[workspace]\nmembers = ["crates/*"]\n\n[workspace.package]\nversion = "9.9.9"\n\n[workspace.dependencies]\nrightkit-fs = { version = "${fs}", path = "crates/rightkit-fs" }\nserde = "1"\n`;
const crate = (version, deps) => `[package]\nname = "rightkit-http"\nversion = "${version}"\n\n[dependencies]\n${deps}\nserde = "1"\n\n[dev-dependencies]\nrightkit-qa = "0.10"\n`;

test("RightKit requirements are read in every form a manifest uses; dev-dependencies are not", () => {
  const text = `[dependencies]\nrightkit-a = "0.1"\nrightkit-b = { version = "0.2", features = ["x"] }\nrightkit-c.workspace = true\nalias = { package = "rightkit-d", version = "0.4" }\n\n[dependencies.rightkit-e]\nversion = "0.5"\n\n[target.'cfg(windows)'.dependencies]\nrightkit-f = "0.6"\n\n[dev-dependencies]\nrightkit-g = "0.7"\n`;
  assert.deepEqual(Object.keys(rightkitDependencies(text).normal).sort(), ["rightkit-a", "rightkit-b", "rightkit-c", "rightkit-d", "rightkit-e", "rightkit-f"]);
  assert.deepEqual(effectiveRequirements(text, root("0.5.0")), { "rightkit-a": "0.1", "rightkit-b": "0.2", "rightkit-c": null, "rightkit-d": "0.4", "rightkit-e": "0.5", "rightkit-f": "0.6" });
  assert.deepEqual(effectiveRequirements(`[dependencies]\nrightkit-fs = { workspace = true }\n`, root("0.5.0")), { "rightkit-fs": "0.5.0" });
  assert.deepEqual(packageIdentity(`[package]\nname = "x"\nversion.workspace = true\n`, root("0.5.0")), { name: "x", version: "9.9.9" });
});

test("a published crate whose requirements change at the same version is refused, whether its own manifest or the workspace's changed", () => {
  const before = { root: root("0.4.9"), crates: new Map([["crates/rightkit-http", crate("0.4.0", "rightkit-fs = { workspace = true }")]]) };
  const viaRoot = { root: root("0.5.0"), crates: before.crates };
  assert.deepEqual(frozenManifestProblems({ before, after: viaRoot, published: { "rightkit-http": "0.4.0" } }).map((p) => p.changed), [["rightkit-fs 0.4.9 -> 0.5.0"]]);
  const viaOwn = { root: before.root, crates: new Map([["crates/rightkit-http", crate("0.4.0", 'rightkit-fs = "0.5.0"')]]) };
  assert.equal(frozenManifestProblems({ before, after: viaOwn, published: { "rightkit-http": "0.4.0" } }).length, 1);
});

test("a raised version, an unpublished version, or unchanged requirements pass", () => {
  const before = { root: root("0.4.9"), crates: new Map([["crates/rightkit-http", crate("0.4.0", "rightkit-fs = { workspace = true }")]]) };
  const bumped = { root: root("0.5.0"), crates: new Map([["crates/rightkit-http", crate("0.4.1", "rightkit-fs = { workspace = true }")]]) };
  assert.deepEqual(frozenManifestProblems({ before, after: bumped, published: { "rightkit-http": "0.4.0" } }), []);
  assert.deepEqual(frozenManifestProblems({ before, after: { root: root("0.5.0"), crates: before.crates }, published: { "rightkit-http": "0.3.9" } }), []);
  assert.deepEqual(frozenManifestProblems({ before, after: before, published: { "rightkit-http": "0.4.0" } }), []);
});
