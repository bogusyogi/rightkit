// A test that spells a live crate's name and its committed version on one line breaks every publish plan the day the crate is
// bumped. Fixtures must derive the version from the crate's Cargo.toml instead.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const HERE = path.join(ROOT, "scripts", "ci");

export function liveVersions(root = ROOT) {
  const out = new Map();
  for (const d of fs.readdirSync(path.join(root, "crates"))) {
    const f = path.join(root, "crates", d, "Cargo.toml");
    if (!d.startsWith("rightkit-") || !fs.existsSync(f)) continue;
    const m = /^\[package\][^[]*?^version\s*=\s*"([^"]+)"/ms.exec(fs.readFileSync(f, "utf8"));
    if (m) out.set(d, m[1]);
  }
  return out;
}

export function offendingLines(text, versions) {
  const hits = [];
  text.split("\n").forEach((line, i) => {
    for (const [name, version] of versions) {
      const v = version.replaceAll(".", "\\.");
      if (new RegExp(`${name.replace(/[-]/g, "\\-")}(?![\\w-])`).test(line) && new RegExp(`["'\`]=?${v}["'\`]`).test(line)) hits.push(`${i + 1}: ${name} ${version}`);
    }
  });
  return hits;
}

test("no CI test hard-codes a live crate's committed version", () => {
  const versions = liveVersions();
  assert.ok(versions.size > 0);
  const bad = [];
  for (const f of fs.readdirSync(HERE).filter((n) => n.endsWith(".test.mjs") && n !== path.basename(fileURLToPath(import.meta.url)))) {
    for (const h of offendingLines(fs.readFileSync(path.join(HERE, f), "utf8"), versions)) bad.push(`${f}:${h}`);
  }
  assert.deepEqual(bad, [], "derive the version from crates/<name>/Cargo.toml");
});

test("the lint flags a name with its exact version and ignores other versions", () => {
  const v = new Map([["rightkit-x", "1.2.3"]]);
  assert.equal(offendingLines('const a = { name: "rightkit-x", version: "1.2.3" };', v).length, 1);
  assert.equal(offendingLines('dep = "rightkit-x" req "=1.2.3"', v).length, 1);
  assert.equal(offendingLines('{ name: "rightkit-x", version: "0.0.1" }', v).length, 0);
  assert.equal(offendingLines('{ name: "rightkit-xy", version: "1.2.3" }', v).length, 0);
});
