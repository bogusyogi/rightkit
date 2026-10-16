import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { crc32, extractNativeLibs, listZipEntries, pkcs7Certificates, readZipEntry, scanAab, scanAabBuffer } from "./android-aab-scan.mjs";
import { DEV_INSPECT_MARKER, QA_CONTROL_MARKER } from "./qa-control-scan.mjs";
import { ELF, FIXTURE_CERT, FIXTURE_CERT_SHA256, aab, cleanLib, fakeCertificate, filler, markedLib, pkcs7, zip } from "./android-test-fixtures.mjs";

const rules = (scan) => scan.findings.map((finding) => finding.rule).sort();

test("a clean signed bundle has no problems and reports its libraries and signer", () => {
  const scan = scanAabBuffer(aab());
  assert.deepEqual(scan.problems, []);
  assert.deepEqual(scan.findings, []);
  assert.deepEqual(scan.nativeLibs.map((lib) => `${lib.module}/${lib.abi}`).sort(), ["base/arm64-v8a", "base/x86_64"]);
  assert.equal(scan.signature.certSha256, FIXTURE_CERT_SHA256);
  assert.equal(scan.signature.debugCertificate, false);
  assert.equal(scan.webFiles, 1);
});

test("the QA control marker inside a deflated or a stored .so is a finding", () => {
  for (const method of ["deflate", "store"]) {
    const scan = scanAabBuffer(aab({ libs: [{ name: "base/lib/arm64-v8a/libapp.so", data: markedLib(), method }] }));
    assert.deepEqual(rules(scan), ["qa-control-server"], method);
    assert.match(scan.findings[0].file, /base\/lib\/arm64-v8a\/libapp\.so$/);
    assert.match(scan.findings[0].reason, new RegExp(QA_CONTROL_MARKER));
  }
});

test("dev-inspect, remote-debugging and wdio signatures are findings like on desktop", () => {
  const lib = Buffer.concat([ELF, Buffer.from(DEV_INSPECT_MARKER), filler(8), Buffer.from("--remote-debugging-port"), filler(8), Buffer.from("tauri-plugin-wdio-webdriver"), Buffer.from("wdio-webdriver:default")]);
  const scan = scanAabBuffer(aab({ libs: [{ name: "base/lib/arm64-v8a/libapp.so", data: lib }] }));
  assert.deepEqual(rules(scan), ["remote-debugging", "shell-dev-inspect", "wdio-webdriver"]);
});

test("a marker in a dynamic-feature module's library is found", () => {
  const scan = scanAabBuffer(aab({ extra: [{ name: "feature/lib/arm64-v8a/libextra.so", data: markedLib() }] }));
  assert.deepEqual(scan.findings.map((f) => f.file.split("!/")[1]), ["feature/lib/arm64-v8a/libextra.so"]);
});

test("a native library that is not an ELF file fails closed", () => {
  const scan = scanAabBuffer(aab({ libs: [{ name: "base/lib/arm64-v8a/libapp.so", data: Buffer.from("MZ not elf") }] }));
  assert.equal(scan.findings[0].rule, "unscannable");
  assert.match(scan.findings[0].reason, /not an ELF/);
});

test("a bundle with no native library cannot be verified", () => {
  const scan = scanAabBuffer(aab({ libs: [{ name: "base/dex/classes.dex", data: Buffer.from("dex") }] }));
  assert.match(scan.problems.join("\n"), /no native library/);
});

test("browser-QA mock markers in the web assets are findings", () => {
  const scan = scanAabBuffer(aab({ extra: [{ name: "base/assets/qa.js", data: Buffer.from("import './qaMock'; const x = tauriQaMock") }] }));
  assert.ok(scan.findings.some((f) => f.rule === "web-bundle-mock" && /base\/assets\/qa\.js$/.test(f.file)));
});

test("missing bundle structure is a problem", () => {
  const scan = scanAabBuffer(aab({ override: { "BundleConfig.pb": null, "base/manifest/AndroidManifest.xml": null } }));
  assert.match(scan.problems.join("\n"), /BundleConfig\.pb/);
  assert.match(scan.problems.join("\n"), /AndroidManifest\.xml/);
});

test("unsigned, doubly signed, orphaned and debug-signed bundles are refused", () => {
  assert.match(scanAabBuffer(aab({ signed: false })).problems.join("\n"), /not signed/);
  const twice = scanAabBuffer(aab({ extra: [{ name: "META-INF/OTHER.RSA", data: pkcs7([FIXTURE_CERT]) }, { name: "META-INF/OTHER.SF", data: Buffer.from("x") }] }));
  assert.match(twice.problems.join("\n"), /2 signature blocks/);
  assert.match(scanAabBuffer(aab({ override: { "META-INF/UPLOAD.SF": null } })).problems.join("\n"), /no matching META-INF\/UPLOAD\.SF/);
  const debug = scanAabBuffer(aab({ certificate: fakeCertificate("CN=Android Debug,O=Android,C=US") }));
  assert.equal(debug.signature.debugCertificate, true);
});

test("a malformed signature block is a problem, not a pass", () => {
  const scan = scanAabBuffer(aab({ override: { "META-INF/UPLOAD.RSA": Buffer.from("not der at all") } }));
  assert.ok(scan.problems.length > 0);
  assert.equal(scan.signature, null);
});

test("garbage, truncated and tampered inputs fail closed", () => {
  assert.match(scanAabBuffer(Buffer.from("not a zip")).problems[0], /not a readable ZIP/);
  const bytes = aab();
  assert.match(scanAabBuffer(bytes.subarray(0, bytes.length - 40)).problems[0], /not a readable ZIP/);
  // Flip one byte inside the stored x86_64 library: the CRC no longer matches, so the entry is unscannable.
  const tampered = Buffer.from(bytes);
  const at = tampered.indexOf(Buffer.from("RIGHTKIT-QA-CONTROL"), tampered.indexOf("base/lib/x86_64/libapp.so"));
  tampered[at] ^= 0xff;
  const scan = scanAabBuffer(tampered);
  assert.ok(scan.findings.some((f) => f.rule === "unscannable" && /CRC-32/.test(f.reason)));
});

test("duplicate entry names and an exceeded inflate budget are refused", () => {
  assert.match(scanAabBuffer(aab({ extra: [{ name: "BundleConfig.pb", data: Buffer.from("again") }] })).problems.join("\n"), /duplicate entry name BundleConfig\.pb/);
  const scan = scanAabBuffer(aab(), { limits: { totalInflated: 100 } });
  assert.ok(scan.findings.some((f) => /inflate budget/.test(f.reason)));
});

test("zip reader: stored and deflate entries round-trip; pkcs7 yields the certificates in order", () => {
  const bytes = zip([{ name: "a", data: Buffer.from("alpha"), method: "store" }, { name: "b", data: Buffer.from("bravo bravo bravo") }]);
  const entries = listZipEntries(bytes);
  assert.deepEqual(entries.map((entry) => entry.name), ["a", "b"]);
  assert.equal(readZipEntry(bytes, entries[0]).toString(), "alpha");
  assert.equal(readZipEntry(bytes, entries[1]).toString(), "bravo bravo bravo");
  assert.equal(crc32(Buffer.from("123456789")), 0xcbf43926);
  const second = fakeCertificate("CN=Intermediate");
  assert.deepEqual(pkcs7Certificates(pkcs7([FIXTURE_CERT, second])), [FIXTURE_CERT, second]);
});

test("scanAab reads a file and extractNativeLibs writes index-named libraries inside the target directory", () => {
  const dir = mkdtempSync(path.join(tmpdir(), "rk-aab-scan-"));
  try {
    const file = path.join(dir, "app.aab");
    writeFileSync(file, aab({ libs: [{ name: "base/lib/arm64-v8a/lib..evil.so", data: cleanLib() }, { name: "base/lib/x86_64/libb.so", data: cleanLib() }] }));
    assert.deepEqual(scanAab(file).problems, []);
    const out = path.join(dir, "libs");
    const written = extractNativeLibs(file, out);
    assert.equal(written.length, 2);
    for (const target of written) {
      assert.equal(path.dirname(target), out);
      assert.ok(readFileSync(target).subarray(0, 4).equals(ELF));
    }
  } finally { rmSync(dir, { recursive: true, force: true }); }
});
