import { test } from "node:test";
import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, mkdirSync, readdirSync, readFileSync, symlinkSync, writeFileSync, rmSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { createHash } from "node:crypto";
import { syncLegalSnapshot, assertFinalLegalSnapshot, generateNpmLicenseAppendix } from "../dist/tooling.js";

function fixture(t) {
  const root = mkdtempSync(path.join(fileURLToPath(new URL("./", import.meta.url)), "fixture-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const write = (name, text) => { mkdirSync(path.dirname(path.join(root, name)), { recursive: true }); writeFileSync(path.join(root, name), text); };
  write("package.json", JSON.stringify({ name: "example", productName: "Example & Co", version: "1.2.3" }));
  write("pnpm-lock.yaml", "lockfileVersion: 9\n");
  write("masters/privacy.md", "# Privacy\nFinal policy\n");
  write("legal/notices.template.md", "Example {{version}} effective {{effectiveDate}}; lock {{lock}}\n");
  write("legal/appendix.html", "<p>Licenses</p>\n");
  const config = {
    profile: "oss", appKey: "example", productName: "Example", licensor: "Example Ltd",
    effectiveDate: "2026-10-17", acceptanceVersion: "example-v1", draftAcceptanceVersion: "example-draft-v1",
    sourceRoot: "masters",
    documents: [
      { id: "privacy", role: "privacy_notice", title: "Privacy", version: "1", path: "privacy.md", source: "privacy.md", publicUrl: "https://example.com/privacy", treatment: "acknowledge", material: false },
      { id: "notices", role: "third_party_notices", title: "Notices", version: "1", path: "notices.md", publicUrl: "https://example.com/notices", treatment: "notice", material: false },
    ],
    appendices: [{ id: "licenses", title: "Licenses", path: "appendix.html" }],
    notices: { template: "notices.template.md", path: "notices.md", hashes: { lock: "pnpm-lock.yaml" } },
  };
  write("config.json", JSON.stringify(config));
  return { root, write, config };
}

function cli(script, args) {
  return spawnSync(process.execPath, [fileURLToPath(new URL(`../bin/${script}.mjs`, import.meta.url)), ...args], { encoding: "utf8" });
}

test("sync creates documents, notices, hashes & gate contract from config", (t) => {
  const { root, config } = fixture(t);
  const result = syncLegalSnapshot(config, root);
  const bytes = readFileSync(path.join(root, "legal/privacy.md"));
  assert.equal(result.manifest.documents[0].sha256, createHash("sha256").update(bytes).digest("hex"));
  assert.equal(result.contract.draft, false);
  assert.match(readFileSync(path.join(root, "legal/notices.md"), "utf8"), /Example 1.2.3 effective 2026-10-17; lock [a-f0-9]{64}/);
  assert.deepEqual(assertFinalLegalSnapshot(config, root), result.manifest);
  const before = readFileSync(path.join(root, "legal/legal-manifest.json"), "utf8");
  syncLegalSnapshot(config, root);
  assert.equal(readFileSync(path.join(root, "legal/legal-manifest.json"), "utf8"), before);
});

test("draft sync uses draft version; final rejects draft & TBD without reading masters", (t) => {
  const { root, config, write } = fixture(t);
  for (const marker of ["DRAFT: pending approval", "[TBD: address]"]) {
    write("masters/privacy.md", marker);
    assert.equal(syncLegalSnapshot(config, root).contract.acceptanceVersion, "example-draft-v1");
    assert.throws(() => assertFinalLegalSnapshot(config, root));
  }
  write("masters/privacy.md", "Final");
  syncLegalSnapshot(config, root);
  rmSync(path.join(root, "masters"), { recursive: true });
  assert.doesNotThrow(() => assertFinalLegalSnapshot(config, root));
  write("legal/privacy.md", "[TBD]");
  assert.throws(() => assertFinalLegalSnapshot(config, root), /draft\/\[TBD/);
});

test("final rejects stale document, appendix, descriptors & gate", (t) => {
  const { root, config, write } = fixture(t);
  for (const [file, text, pattern] of [
    ["legal/privacy.md", "changed", /hash mismatch/],
    ["legal/appendix.html", "changed", /hash mismatch/],
    ["src/generated/legal-contract.json", "{}", /stale gate/],
  ]) {
    syncLegalSnapshot(config, root);
    const previous = readFileSync(path.join(root, file));
    write(file, text);
    assert.throws(() => assertFinalLegalSnapshot(config, root), pattern);
    write(file, previous);
  }
  assert.throws(() => assertFinalLegalSnapshot({ ...config, productName: "Changed" }, root), /descriptors differ/);
});

test("invalid config, missing inputs & placeholders fail before writes", (t) => {
  const { root, config } = fixture(t);
  syncLegalSnapshot(config, root);
  const before = readFileSync(path.join(root, "legal/legal-manifest.json"), "utf8");
  for (const modify of [
    (c) => { c.documents[0].source = "../outside"; },
    (c) => { c.documents[0].path = "../outside"; },
    (c) => { c.appendices[0].path = "missing.html"; },
    (c) => { c.notices.hashes = {}; },
    (c) => { c.documents[0].publicUrl = "http://example.com"; },
    (c) => { c.contractPath = "../outside.json"; },
  ]) {
    const altered = structuredClone(config); modify(altered);
    assert.throws(() => syncLegalSnapshot(altered, root));
    assert.equal(readFileSync(path.join(root, "legal/legal-manifest.json"), "utf8"), before);
  }
});

test("npm appendix escapes metadata & notices, scans all paths & excludes RightKit", (t) => {
  const { root, write } = fixture(t);
  write("deps/one/LICENSE", "Copyright <one> & rights\n");
  write("deps/two/LICENCE.txt", "Copyright <two>\n");
  mkdirSync(path.join(root, "deps/one/LICENSES"));
  const groups = { MIT: [
    { name: "@rightkit/internal", versions: ["1"], paths: ["missing"] },
    { name: "<pkg>", versions: ["1", "2"], paths: ["deps/two", "deps/one", "deps/one"] },
  ] };
  const result = generateNpmLicenseAppendix({ appRoot: root, groups });
  const html = readFileSync(result.output, "utf8");
  assert.equal(result.packageCount, 1);
  assert.equal(result.fallbackCount, 0);
  assert.match(html, /Example &amp; Co/);
  assert.match(html, /&lt;pkg&gt;/);
  assert.match(html, /Copyright &lt;one&gt; &amp; rights/);
  assert.match(html, /Copyright &lt;two&gt;/);
  assert.doesNotMatch(html, /@rightkit\/internal/);
  assert.match(html, /sha256=[a-f0-9]{64}/);
});

test("npm fallback is explicit for MIT/ISC & unsupported missing licenses fail", (t) => {
  const { root } = fixture(t);
  mkdirSync(path.join(root, "empty"));
  for (const license of ["MIT", "ISC"]) {
    const result = generateNpmLicenseAppendix({ appRoot: root, groups: { [license]: [{ name: "empty", versions: ["1"], paths: ["empty"] }] } });
    assert.equal(result.fallbackCount, 1);
    assert.match(readFileSync(result.output, "utf8"), /not package-specific content/);
  }
  assert.throws(() => generateNpmLicenseAppendix({ appRoot: root, groups: { UNKNOWN: [{ name: "empty", versions: ["1"], paths: ["empty"] }] } }), /no SPDX fallback/);
  assert.throws(() => generateNpmLicenseAppendix({ appRoot: root, groups: { MIT: [{ name: "empty", versions: ["1"], paths: [] }] } }), /missing installed package path/);
});

test("snapshot CLI sync & --final work from unrelated cwd & propagate failure", (t) => {
  const { root, write } = fixture(t);
  const args = ["--root", root, "--config", "config.json"];
  const synced = cli("legal-snapshot", args);
  assert.equal(synced.status, 0, synced.stderr);
  const final = cli("legal-snapshot", [...args, "--final"]);
  assert.equal(final.status, 0, final.stderr);
  write("legal/privacy.md", "[TBD]");
  assert.equal(cli("legal-snapshot", [...args, "--final"]).status, 1);
  assert.equal(cli("legal-snapshot", ["--unknown"]).status, 1);
  assert.equal(cli("legal-snapshot", ["--root"]).status, 1);
});

test("npm CLI consumes captured pnpm JSON & package exposes shipped bins/API", (t) => {
  const { root, write } = fixture(t);
  write("deps/pkg/NOTICE", "Notice");
  write("licenses.json", JSON.stringify({ MIT: [{ name: "pkg", versions: ["1"], paths: ["deps/pkg"] }] }));
  const result = cli("npm-license-appendix", ["--root", root, "--product-name", "CutRight", "--licenses-json", "licenses.json"]);
  assert.equal(result.status, 0, result.stderr);
  assert.match(readFileSync(path.join(root, "legal/THIRD_PARTY_NPM_LICENSES.html"), "utf8"), /CutRight npm third-party licenses/);
  const pkg = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
  assert.equal(pkg.exports["./tooling"].import, "./dist/tooling.js");
  for (const bin of Object.values(pkg.bin)) assert.match(readFileSync(new URL(`../${bin}`, import.meta.url), "utf8"), /^#!\/usr\/bin\/env node/);
  assert.ok(pkg.files.includes("bin"));
});

test("npm generator invokes production pnpm scanner in app root & preserves output on failure", { skip: process.platform === "win32" }, (t) => {
  const { root, write } = fixture(t);
  write("deps/pkg/LICENSE", "License");
  const scanner = `#!${process.execPath}\nimport {writeFileSync} from 'node:fs';\nwriteFileSync('scanner-call.json', JSON.stringify({cwd:process.cwd(),args:process.argv.slice(2)}));\nconsole.log(JSON.stringify({MIT:[{name:'pkg',versions:['1'],paths:['deps/pkg']}]}));\n`;
  write("bin/pnpm", scanner);
  chmodSync(path.join(root, "bin/pnpm"), 0o755);
  const oldPath = process.env.PATH;
  process.env.PATH = `${path.join(root, "bin")}${path.delimiter}${oldPath}`;
  t.after(() => { process.env.PATH = oldPath; });
  const result = generateNpmLicenseAppendix({ appRoot: root });
  assert.deepEqual(JSON.parse(readFileSync(path.join(root, "scanner-call.json"), "utf8")), {
    cwd: root, args: ["licenses", "list", "--prod", "--json"],
  });
  const previous = readFileSync(result.output, "utf8");
  write("bin/pnpm", `#!${process.execPath}\nconsole.error('scanner failed'); process.exit(3);\n`);
  assert.throws(() => generateNpmLicenseAppendix({ appRoot: root }), /scanner failed/);
  assert.equal(readFileSync(result.output, "utf8"), previous);
});

test("proprietary snapshot keeps material digest stable across notice regeneration", (t) => {
  const { root, write, config } = fixture(t);
  delete config.profile;
  write("masters/agreement.md", "Approved agreement v1");
  for (const role of ["license", "eula", "acceptable_use", "product_schedule"]) {
    config.documents.push({ id: role, role, title: role, version: "1", path: `${role}.md`, source: "agreement.md", publicUrl: `https://example.com/${role}`, treatment: "agree", material: true });
  }
  const first = syncLegalSnapshot(config, root);
  write("legal/notices.template.md", "Regenerated notices {{version}} {{lock}}");
  const notices = syncLegalSnapshot(config, root);
  assert.equal(first.contract.materialSha256, notices.contract.materialSha256);
  assert.notEqual(first.manifest.documents[1].sha256, notices.manifest.documents[1].sha256);
  write("masters/agreement.md", "Approved agreement v2");
  const material = syncLegalSnapshot(config, root);
  assert.notEqual(material.contract.materialSha256, first.contract.materialSha256);
  assert.equal(material.contract.acceptanceVersion, config.acceptanceVersion);
  assert.doesNotThrow(() => assertFinalLegalSnapshot(config, root));
});

test("a symlinked legal directory pointing outside the app root is refused before any write", (t) => {
  const { root, config } = fixture(t);
  const outside = mkdtempSync(path.join(fileURLToPath(new URL("./", import.meta.url)), "outside-"));
  t.after(() => rmSync(outside, { recursive: true, force: true }));
  rmSync(path.join(root, "legal"), { recursive: true, force: true });
  symlinkSync(outside, path.join(root, "legal"));
  assert.throws(() => syncLegalSnapshot(config, root), /resolves outside/);
  assert.deepEqual(readdirSync(outside), []);
});

test("rightkit-legal npm-licenses delegates; rust-licenses is BLOCKED with nonzero exit", (t) => {
  const { root, write } = fixture(t);
  write("deps/pkg/NOTICE", "Notice");
  write("licenses.json", JSON.stringify({ MIT: [{ name: "pkg", versions: ["1"], paths: ["deps/pkg"] }] }));
  const npm = cli("rightkit-legal", ["npm-licenses", "--root", root, "--product-name", "CutRight", "--licenses-json", "licenses.json"]);
  assert.equal(npm.status, 0, npm.stderr);
  assert.match(readFileSync(path.join(root, "legal/THIRD_PARTY_NPM_LICENSES.html"), "utf8"), /CutRight npm third-party licenses/);
  const rust = cli("rightkit-legal", ["rust-licenses", "--root", root]);
  assert.equal(rust.status, 1);
  assert.match(rust.stderr, /BLOCKED rust-licenses/);
});
