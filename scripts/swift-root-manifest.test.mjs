// Drift guard: the root Package.swift (what apps resolve from github.com/Orthic-Labs/rightkit) must expose exactly what
// swift/RightKitSwift/Package.swift builds in place. Textual parse, no dependencies; no Swift toolchain needed.
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PKG_DIR = "swift/RightKitSwift";
const read = (file) => fs.readFileSync(path.join(ROOT, file), "utf8");
const stripComments = (text) => text.replace(/\/\/[^\n]*/g, "");

// Returns the argument text of every `.<kind>(...)` call, with balanced parentheses and string awareness.
function calls(text, kind) {
  const out = [];
  const needle = `.${kind}(`;
  for (let at = text.indexOf(needle); at !== -1; at = text.indexOf(needle, at + 1)) {
    let depth = 1, i = at + needle.length, inString = false;
    for (; i < text.length && depth > 0; i += 1) {
      const c = text[i];
      if (inString) { if (c === "\\") i += 1; else if (c === '"') inString = false; }
      else if (c === '"') inString = true;
      else if (c === "(") depth += 1;
      else if (c === ")") depth -= 1;
    }
    out.push(text.slice(at + needle.length, i - 1));
  }
  return out;
}
const strings = (text) => [...text.matchAll(/"([^"]*)"/g)].map((m) => m[1]);
// A bracketed list argument (`name: [ ... ]`, nested parentheses allowed) or a string argument.
const field = (args, name) => {
  const m = args.match(new RegExp(`${name}:\\s*(\\[[^\\]]*(?:\\([^)]*\\)[^\\]]*)*\\]|"[^"]*")`));
  return m ? m[1] : null;
};

function parse(file) {
  const raw = read(file);
  const text = stripComments(raw);
  const products = [...calls(text, "library"), ...calls(text, "executable")].map((a) => ({ name: strings(a)[0], targets: strings(field(a, "targets") ?? "") }));
  const targets = [
    ...calls(text, "target").map((args) => ({ kind: "target", args })),
    ...calls(text, "testTarget").map((args) => ({ kind: "testTarget", args })),
    ...calls(text, "executableTarget").map((args) => ({ kind: "executableTarget", args })),
  ].map(({ kind, args }) => ({
    kind,
    name: strings(args)[0],
    dependencies: strings(field(args, "dependencies") ?? ""),
    resources: (field(args, "resources") ?? "").replace(/\s+/g, ""),
    linker: (args.match(/linkerSettings:\s*\[[^\]]*\]/) ?? [""])[0].replace(/\s+/g, ""),
    path: field(args, "path")?.replace(/"/g, "") ?? null,
  }));
  return {
    tools: raw.match(/swift-tools-version:\s*([\d.]+)/)?.[1],
    platforms: (text.match(/platforms:\s*\[[^\]]*\]/) ?? [""])[0].replace(/\s+/g, ""),
    products,
    targets,
  };
}

const nested = parse(`${PKG_DIR}/Package.swift`);
const root = parse("Package.swift");

test("manifests expose the same products", () => {
  assert.ok(nested.products.length >= 15, "parser found the products");
  assert.deepEqual(root.products, nested.products);
});

test("manifests declare the same targets, dependencies, resources and linker settings", () => {
  assert.ok(nested.targets.length >= 30, "parser found the targets");
  const names = (m) => m.targets.map((t) => `${t.kind}:${t.name}`).sort();
  assert.deepEqual(names(root), names(nested));
  const rootTargets = new Map(root.targets.map((t) => [t.name, t]));
  for (const t of nested.targets) {
    const r = rootTargets.get(t.name);
    assert.deepEqual(r.dependencies, t.dependencies, `dependencies of ${t.name}`);
    assert.equal(r.resources, t.resources, `resources of ${t.name}`);
    assert.equal(r.linker, t.linker, `linkerSettings of ${t.name}`);
  }
});

test("manifests agree on tools version and platforms", () => {
  assert.ok(root.tools, "root tools version found");
  assert.equal(root.tools, nested.tools);
  assert.ok(root.platforms.length > 0, "platforms found");
  assert.equal(root.platforms, nested.platforms);
});

test("every root target path exists and is the default location the nested manifest uses", () => {
  for (const t of root.targets) {
    const folder = t.kind === "testTarget" ? "Tests" : "Sources";
    assert.equal(t.path, `${PKG_DIR}/${folder}/${t.name}`, `path of ${t.name}`);
    assert.ok(fs.statSync(path.join(ROOT, t.path)).isDirectory(), `${t.path} must exist`);
  }
  for (const t of nested.targets) assert.equal(t.path, null, `${t.name} in the nested manifest uses the default path`);
});

test("resource files named by the manifests exist under their target paths", () => {
  for (const t of root.targets) {
    for (const rel of strings(t.resources)) {
      assert.ok(fs.existsSync(path.join(ROOT, t.path, rel)), `${t.name} resource ${rel}`);
    }
  }
});

test("the VERSION file is clean semver", () => {
  assert.match(read(`${PKG_DIR}/VERSION`).trim(), /^\d+\.\d+\.\d+$/);
});
