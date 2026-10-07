import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { createProtectedFileManifest, DEFAULT_MANIFEST_RELATIVE, verifyProtectedFileManifest } from "./package-manifest.mjs";

test("admin-install manifest binds exact package file inventory and bytes", () => {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-adapter-package-"));
  try {
    mkdirSync(path.join(root, "ownership", "adapters"), { recursive: true });
    mkdirSync(path.join(root, "runtime"), { recursive: true });
    writeFileSync(path.join(root, "ownership", "adapters", "private-local.mjs"), "fixed launcher\n");
    writeFileSync(path.join(root, "runtime", "node.exe"), "runtime bytes");
    const manifestPath = path.join(root, ...DEFAULT_MANIFEST_RELATIVE.split("/"));
    writeFileSync(manifestPath, JSON.stringify(createProtectedFileManifest(root)), { flag: "wx" });
    assert.equal(verifyProtectedFileManifest(root).ok, true);
    writeFileSync(path.join(root, "ownership", "adapters", "private-local.mjs"), "tampered launcher\n");
    assert.match(verifyProtectedFileManifest(root).problems.join(";"), /changed or is missing/);
    writeFileSync(path.join(root, "ownership", "adapters", "private-local.mjs"), "fixed launcher\n");
    writeFileSync(path.join(root, "unexpected.txt"), "not in install manifest");
    assert.match(verifyProtectedFileManifest(root).problems.join(";"), /unexpected protected file/);
    rmSync(path.join(root, "unexpected.txt"));
    mkdirSync(path.join(root, "unexpected-empty-directory"));
    assert.match(verifyProtectedFileManifest(root).problems.join(";"), /unexpected protected file/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test("enrollment and fixed policy paths can change without exempting code or arbitrary neighbors", () => {
  const root = mkdtempSync(path.join(tmpdir(), "rightkit-policy-lifecycle-"));
  try {
    mkdirSync(path.join(root, "ownership", "adapters"), { recursive: true });
    writeFileSync(path.join(root, "ownership", "adapters", "private-local.mjs"), "fixed launcher\n");
    const manifestPath = path.join(root, ...DEFAULT_MANIFEST_RELATIVE.split("/"));
    writeFileSync(manifestPath, JSON.stringify(createProtectedFileManifest(root)), { flag: "wx" });
    for (const name of ["trust.json", "public-check.json", "public-check-app.pem"]) writeFileSync(path.join(root, "ownership", name), "admin policy fixture\n");
    mkdirSync(path.join(root, "ownership", "installer"));
    writeFileSync(path.join(root, "ownership", "installer", "install-trust.mjs"), "separately protected installer\n");
    assert.equal(verifyProtectedFileManifest(root).ok, true);
    writeFileSync(path.join(root, "ownership", "trust.json"), "updated enrollment fixture\n");
    assert.equal(verifyProtectedFileManifest(root).ok, true);
    writeFileSync(path.join(root, "ownership", "trust.json.backup"), "not an exempt policy path\n");
    assert.match(verifyProtectedFileManifest(root).problems.join(";"), /unexpected protected file/);
    rmSync(path.join(root, "ownership", "trust.json.backup"));
    rmSync(path.join(root, "ownership", "trust.json"));
    mkdirSync(path.join(root, "ownership", "trust.json"));
    assert.match(verifyProtectedFileManifest(root).problems.join(";"), /invalid file type/);
  } finally { rmSync(root, { recursive: true, force: true }); }
});
