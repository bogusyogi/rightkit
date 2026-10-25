import assert from "node:assert/strict";
import test from "node:test";
import { cargoManifestState } from "./cargo-manifest-text.mjs";

test("cargoManifestState: [package] and its publish key, read by table", () => {
  const state = (text) => cargoManifestState(text);
  assert.deepEqual(state('[package]\nname = "a"\n'), { hasPackage: true, unpublished: false });
  assert.deepEqual(state('[package]\nname = "a"\npublish = false\n'), { hasPackage: true, unpublished: true });
  assert.deepEqual(state('[package]\r\nname = "a"\r\npublish = [ ]  # nowhere\r\n'), { hasPackage: true, unpublished: true });
  assert.deepEqual(state('[ package ]\npublish = false\n'), { hasPackage: true, unpublished: true }, "whitespace inside the header");
  assert.deepEqual(state('[package] # the crate\npublish = false\n'), { hasPackage: true, unpublished: true }, "a comment after the header");
  assert.deepEqual(state('[package]\npublish = true\n'), { hasPackage: true, unpublished: false });
  assert.deepEqual(state('[package]\npublish = ["internal"]\n'), { hasPackage: true, unpublished: false }, "a registry list publishes");
  assert.deepEqual(state('[workspace]\nmembers = []\n[workspace.package]\npublish = false\n'), { hasPackage: false, unpublished: false }, "a virtual workspace is no package");
  assert.deepEqual(state('[package]\nname = "a"\n[package.metadata.x]\npublish = false\n'), { hasPackage: true, unpublished: false }, "another table's publish key");
  assert.deepEqual(state('[package]\nname = "a"\n[[bin]]\nname = "b"\npublish = false\n'), { hasPackage: true, unpublished: false }, "an array-of-tables header ends the table");
  assert.deepEqual(state('[package]\npublish = false\n[[bin]]\nname = "b"\n'), { hasPackage: true, unpublished: true }, "publish before a [[bin]] still counts");
  assert.deepEqual(state(""), { hasPackage: false, unpublished: false });
});
