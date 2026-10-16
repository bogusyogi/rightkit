import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { renderLanes } from "./sync.mjs";
import { applyCheckoutOptions } from "./checkout.mjs";
import { KNOWN_LANES } from "./manifest.mjs";

const BASE = {
  schemaVersion: 1,
  compile: "github-actions-only",
  lanes: [...KNOWN_LANES],
  swift: { check: { runner: "macos-15", xcodeVersion: "16.4", projectDirectory: "ios", spec: "project.yml", project: "Widgets.xcodeproj", scheme: "Widgets", xcodegenVersion: "2.44.1" } },
  matrix: { os: ["ubuntu-latest", "macos-latest", "windows-latest"], node: ["22.13", "24"] },
  packageManager: "pnpm@11.24.0",
  publish: {
    npm: { oidc: true, provenance: true },
    homebrew: { tap: "acme/homebrew-tap", formulaName: "widgets" },
    winget: { packageIdentifier: "Acme.Widgets" },
  },
  regenerate: { script: "regenerate" },
  templateVersion: "1.0.0",
};
const NODE_LANES = ["ci", "qualification", "package-smoke", "publish-npm", "publish-package-managers", "regenerate"];

function repo() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "rg-checkout-"));
  fs.writeFileSync(path.join(dir, "package.json"), JSON.stringify({ name: "widgets" }));
  return dir;
}

test("checkout unset, empty or longPaths:false leaves every lane byte-identical", () => {
  const r = repo();
  const base = { ...BASE, lanes: NODE_LANES };
  const plain = renderLanes(base, r);
  assert.deepEqual(renderLanes({ ...base, checkout: {} }, r), plain);
  assert.deepEqual(renderLanes({ ...base, checkout: { longPaths: false } }, r), plain);
});

test("checkout.longPaths puts a Windows-gated system core.longpaths step before every checkout", () => {
  const manifest = { ...BASE, lanes: NODE_LANES, checkout: { longPaths: true } };
  for (const { content } of renderLanes(manifest, repo())) {
    const lines = content.split("\n");
    let seen = 0;
    lines.forEach((line, i) => {
      if (!/uses: actions\/checkout@/.test(line)) return;
      seen += 1;
      const header = /- uses:/.test(line) ? i : i - 1;
      assert.equal(lines[header - 1].trim(), "run: git config --system core.longpaths true");
      assert.equal(lines[header - 3].trim(), "if: runner.os == 'Windows'");
      assert.equal(lines[header - 4].trim(), "- name: Enable Windows long paths");
    });
    assert.equal((content.match(/Enable Windows long paths/g) ?? []).length, seen);
  }
});

test("checkout.sparse merges into with: and leaves other-repository checkouts alone", () => {
  const manifest = { ...BASE, lanes: ["ci", "publish-package-managers"], checkout: { sparse: ["packages/", "examples/App"] } };
  const [ci, pm] = renderLanes(manifest, repo()).map((r) => r.content);
  assert.match(ci, /sparse-checkout: \|\n\s+packages\/\n\s+examples\/App\n\s+sparse-checkout-cone-mode: true/);
  assert.doesNotMatch(ci, /core\.longpaths/);
  const tap = pm.split("- name: Checkout Homebrew tap")[1].split("      - run:")[0];
  assert.doesNotMatch(tap, /sparse-checkout/);
  assert.match(pm, /sparse-checkout: \|/);
});

test("rust-hybrid ci renders both options deterministically", () => {
  const manifest = { ...BASE, lanes: ["ci"], profile: "rust-hybrid", gate: { path: "scripts/gate.sh" }, templateVersion: "1.1.0", checkout: { longPaths: true, sparse: ["packages"] } };
  const r = repo();
  const a = renderLanes(manifest, r)[0].content;
  assert.equal(renderLanes(manifest, r)[0].content, a);
  assert.match(a, /Enable Windows long paths/);
  assert.match(a, /sparse-checkout: \|\n\s+packages\n/);
});

test("applyCheckoutOptions adds with: when the step has none", () => {
  const out = applyCheckoutOptions("jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v5\n      - run: x\n", { checkout: { sparse: ["a"] } });
  assert.equal(out, "jobs:\n  a:\n    steps:\n      - uses: actions/checkout@v5\n        with:\n          sparse-checkout: |\n            a\n          sparse-checkout-cone-mode: true\n      - run: x\n");
});

test("defaultBranch renders into push triggers; unset stays main", () => {
  const r = repo();
  const base = { ...BASE, lanes: ["ci", "qualification", "package-smoke"] };
  for (const { content } of renderLanes(base, r)) assert.match(content, /branches: \[main\]/);
  for (const { content } of renderLanes({ ...base, defaultBranch: "master" }, r)) {
    assert.match(content, /branches: \[master\]/);
    assert.doesNotMatch(content, /branches: \[main\]/);
  }
  const rh = { ...BASE, lanes: ["ci"], profile: "rust-hybrid", gate: { path: "scripts/gate.sh" }, templateVersion: "1.1.0", defaultBranch: "master" };
  assert.match(renderLanes(rh, r)[0].content, /branches: \[master\]/);
});
