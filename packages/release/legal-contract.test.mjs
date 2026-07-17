import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { assertLegalReleaseContract } from "./legal-contract.mjs";

function digest(text) {
  return createHash("sha256").update(text).digest("hex");
}

function fixture() {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-legal-contract-"));
  const legalDir = path.join(root, "legal");
  const tauriDir = path.join(root, "src-tauri");
  mkdirSync(legalDir, { recursive: true });
  mkdirSync(tauriDir, { recursive: true });
  const definitions = [
    ["license", "LICENSE.md", "Ownership notice\n"],
    ["eula", "EULA.md", "End user license agreement\n"],
    ["acceptable_use", "ACCEPTABLE_USE.md", "Acceptable use\n"],
    ["product_schedule", "PRODUCT_SCHEDULE.md", "Product schedule\n"],
    ["privacy_notice", "PRIVACY.md", "Privacy notice\n"],
    ["third_party_notices", "THIRD_PARTY_NOTICES.md", "Build provenance: fixture 1.0 windows-x86_64; inputs: pnpm-lock=abc Cargo.lock=def; generated-by=fixture\n"],
  ];
  for (const [, file, content] of definitions) writeFileSync(path.join(legalDir, file), content);
  const manifest = {
    schema: 2,
    suite: "right-suite-desktop",
    appKey: "fixture",
    productName: "Fixture",
    licensor: "Damned Ventures LLC",
    effectiveDate: "2026-07-16",
    acceptanceVersion: "fixture-2026-07-16-v1",
    documents: definitions.map(([role, file, content]) => ({
      id: `fixture-${role}`,
      role,
      title: role,
      version: "1.0",
      sha256: digest(content),
      path: file,
      publicUrl: `https://example.test/legal/${role}`,
      treatment: ["privacy_notice"].includes(role) ? "acknowledge" : role === "third_party_notices" ? "notice" : "agree",
      material: !["privacy_notice", "third_party_notices"].includes(role),
    })),
  };
  writeFileSync(path.join(legalDir, "legal-manifest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
  writeFileSync(
    path.join(tauriDir, "tauri.conf.json"),
    `${JSON.stringify({ bundle: { publisher: "Damned Ventures LLC" } }, null, 2)}\n`,
  );
  return {
    root,
    legal: { manifest: "legal/legal-manifest.json", tauriConfig: "src-tauri/tauri.conf.json" },
    manifest,
  };
}

test("accepts a complete, hash-bound legal snapshot", () => {
  const { root, legal } = fixture();
  const result = assertLegalReleaseContract(root, legal, "fixture", "win");
  assert.equal(result.appKey, "fixture");
  assert.equal(result.documents.length, 6);
});

test("rejects a missing legal config", () => {
  const { root } = fixture();
  assert.throws(() => assertLegalReleaseContract(root, null, "fixture", "win"), /must declare legal\.manifest/i);
});

test("rejects changed bytes and unsafe manifest paths", () => {
  const changed = fixture();
  writeFileSync(path.join(changed.root, "legal", "EULA.md"), "changed without a new manifest\n");
  assert.throws(() => assertLegalReleaseContract(changed.root, changed.legal, "fixture", "win"), /EULA\.md.*sha-?256/i);

  const traversal = fixture();
  traversal.manifest.documents[0].path = "../outside.md";
  writeFileSync(path.join(traversal.root, "legal", "legal-manifest.json"), `${JSON.stringify(traversal.manifest)}\n`);
  assert.throws(() => assertLegalReleaseContract(traversal.root, traversal.legal, "fixture", "win"), /path.*snapshot|traversal/i);
});

test("rejects placeholders and unresolved checklist markers", () => {
  const { root, legal, manifest } = fixture();
  const eula = "Terms [COUNSEL: decide later]\n- [ ] fill this in\n";
  writeFileSync(path.join(root, "legal", "EULA.md"), eula);
  manifest.documents.find((document) => document.role === "eula").sha256 = digest(eula);
  writeFileSync(path.join(root, "legal", "legal-manifest.json"), `${JSON.stringify(manifest)}\n`);
  assert.throws(() => assertLegalReleaseContract(root, legal, "fixture", "win"), /placeholder|unchecked|release blocker/i);
});

test("rejects an explicitly unresolved release blocker even without a checkbox", () => {
  const { root, legal, manifest } = fixture();
  const notice = "Build provenance: fixture; inputs: lock=abc; generated-by=fixture\nRELEASE BLOCKER: exact model notice is unresolved.\n";
  writeFileSync(path.join(root, "legal", "THIRD_PARTY_NOTICES.md"), notice);
  manifest.documents.find((document) => document.role === "third_party_notices").sha256 = digest(notice);
  writeFileSync(path.join(root, "legal", "legal-manifest.json"), `${JSON.stringify(manifest)}\n`);
  assert.throws(() => assertLegalReleaseContract(root, legal, "fixture", "win"), /release blocker/i);
});

test("rejects a Windows installer license page: the in-app gate is the only assent surface", () => {
  const { root, legal } = fixture();
  writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), JSON.stringify({ bundle: { licenseFile: "../legal/EULA.md" } }));
  assert.throws(() => assertLegalReleaseContract(root, legal, "fixture", "win"), /licenseFile must be absent/i);
});

test("rejects a macOS DMG SLA too — one licenseFile drives both bundlers", () => {
  const { root, legal } = fixture();
  writeFileSync(path.join(root, "src-tauri", "tauri.conf.json"), JSON.stringify({ bundle: { licenseFile: "../legal/EULA.md" } }));
  assert.throws(() => assertLegalReleaseContract(root, legal, "fixture", "mac"), /licenseFile must be absent/i);
});

test("accepts an app whose installer carries no license page", () => {
  const { root, legal } = fixture();
  for (const platform of ["win", "mac"]) {
    assert.doesNotThrow(() => assertLegalReleaseContract(root, legal, "fixture", platform));
  }
});

test("requires third-party build provenance", () => {
  const { root, legal, manifest } = fixture();
  const notice = "Third-party software is included.\n";
  writeFileSync(path.join(root, "legal", "THIRD_PARTY_NOTICES.md"), notice);
  manifest.documents.find((document) => document.role === "third_party_notices").sha256 = digest(notice);
  writeFileSync(path.join(root, "legal", "legal-manifest.json"), `${JSON.stringify(manifest)}\n`);
  assert.throws(() => assertLegalReleaseContract(root, legal, "fixture", "win"), /third-party.*provenance/i);
});

test("manifest hash evidence is the hash of the manifest bytes", () => {
  const { root, legal } = fixture();
  const result = assertLegalReleaseContract(root, legal, "fixture", "mac");
  const bytes = readFileSync(path.join(root, legal.manifest));
  assert.equal(result.manifestSha256, createHash("sha256").update(bytes).digest("hex"));
});

test("hash-binds declared third-party appendices and rejects missing files", () => {
  const { root, legal, manifest } = fixture();
  const appendix = "full dependency license text\n";
  writeFileSync(path.join(root, "legal", "RUST_LICENSES.txt"), appendix);
  manifest.appendices = [{ id: "rust-licenses", title: "Rust licenses", path: "RUST_LICENSES.txt", sha256: digest(appendix) }];
  writeFileSync(path.join(root, "legal", "legal-manifest.json"), `${JSON.stringify(manifest)}\n`);
  assert.equal(assertLegalReleaseContract(root, legal, "fixture", "mac").appendices.length, 1);

  manifest.appendices[0].path = "missing.txt";
  writeFileSync(path.join(root, "legal", "legal-manifest.json"), `${JSON.stringify(manifest)}\n`);
  assert.throws(() => assertLegalReleaseContract(root, legal, "fixture", "mac"), /appendix.*missing/i);
});
