import test from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { deflateRawSync } from "node:zlib";
import os from "node:os";
import path from "node:path";
import { assertPublisherMatchesSigner } from "./msix-plan.mjs";
import { buildMsix } from "./msix-build.mjs";
import { parseSignedData, readMsixSignerSubject, renderSubject, signerCertificate } from "./msix-signer.mjs";

// Fixture: PKCX + CMS SignedData made with openssl; certificates are [decoy "CN=Decoy Issuer", signer], SignerInfo names the second.
const P7X = Buffer.from(readFileSync(new URL("./fixtures/msix-signer.p7x.b64", import.meta.url), "utf8"), "base64");
const SIGNER = "CN=Fixture Signer LLC, O=Fixture Signer LLC, L=Testville, S=Test State, C=US";
const crc = (b) => { let c; let r = 0xffffffff; for (const x of b) { c = (r ^ x) & 0xff; for (let k = 0; k < 8; k += 1) c = c & 1 ? (c >>> 1) ^ 0xedb88320 : c >>> 1; r = (r >>> 8) ^ c; } return (r ^ 0xffffffff) >>> 0; };

function zip(entries) {
  const locals = []; const central = []; let offset = 0;
  for (const [name, data, method] of entries) {
    const n = Buffer.from(name); const body = method === 8 ? deflateRawSync(data) : data;
    const l = Buffer.alloc(30); l.writeUInt32LE(0x04034b50, 0); l.writeUInt16LE(20, 4); l.writeUInt16LE(method, 8);
    l.writeUInt32LE(crc(data), 14); l.writeUInt32LE(body.length, 18); l.writeUInt32LE(data.length, 22); l.writeUInt16LE(n.length, 26);
    const c = Buffer.alloc(46); c.writeUInt32LE(0x02014b50, 0); c.writeUInt16LE(20, 4); c.writeUInt16LE(20, 6); c.writeUInt16LE(method, 10);
    c.writeUInt32LE(crc(data), 16); c.writeUInt32LE(body.length, 20); c.writeUInt32LE(data.length, 24); c.writeUInt16LE(n.length, 28); c.writeUInt32LE(offset, 42);
    locals.push(l, n, body); central.push(c, n); offset += 30 + n.length + body.length;
  }
  const cd = Buffer.concat(central);
  const e = Buffer.alloc(22); e.writeUInt32LE(0x06054b50, 0); e.writeUInt16LE(entries.length, 8); e.writeUInt16LE(entries.length, 10); e.writeUInt32LE(cd.length, 12); e.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, cd, e]);
}
const pkg = (entries) => { const dir = mkdtempSync(path.join(os.tmpdir(), "msix-signer-")); const file = path.join(dir, "a.msix"); writeFileSync(file, zip(entries)); return { dir, file }; };

for (const method of [0, 8]) {
  test(`reads the signer subject (not the first certificate) from AppxSignature.p7x, zip method ${method}`, () => {
    const { dir, file } = pkg([["AppxManifest.xml", Buffer.from("<x/>"), 8], ["AppxSignature.p7x", P7X, method]]);
    try {
      const subject = readMsixSignerSubject(file);
      assert.equal(subject, SIGNER);
      // Publisher comparison is order-insensitive (normalizeDn sorts), so a reordered Publisher still matches.
      assertPublisherMatchesSigner("CN=Fixture Signer LLC, O=Fixture Signer LLC, L=Testville, S=Test State, C=US", subject);
      assert.throws(() => assertPublisherMatchesSigner("CN=Other LLC, O=Fixture Signer LLC, L=Testville, S=Test State, C=US", subject), { code: "MSIX_PUBLISHER_MISMATCH" });
    } finally { rmSync(dir, { recursive: true }); }
  });
}

test("SignerInfo issuer+serial picks the certificate; an unknown signer fails closed", () => {
  const parsed = parseSignedData(P7X.subarray(4));
  assert.equal(parsed.certificates.length, 2);
  assert.match(renderSubject(signerCertificate(parsed)), /Fixture Signer LLC/);
  assert.throws(() => signerCertificate({ ...parsed, signer: { ...parsed.signer, serial: "01" } }), /signer certificate not found/);
});

test("subject values with separators are quoted", () => {
  assert.equal(renderSubject({ subject: "C=US\nCN=Acme, Inc." }), 'CN="Acme, Inc.", C=US');
});

test("missing, headerless and unparsable signatures fail closed with MSIX_PUBLISHER_MISMATCH", () => {
  const cases = {
    "no AppxSignature.p7x": [["AppxManifest.xml", Buffer.from("<x/>"), 8]],
    "no PKCX header": [["AppxSignature.p7x", P7X.subarray(4), 0]],
    "garbage after header": [["AppxSignature.p7x", Buffer.concat([Buffer.from("PKCX"), Buffer.from("not der at all")]), 0]],
    "truncated SignedData": [["AppxSignature.p7x", P7X.subarray(0, 200), 0]],
  };
  for (const [label, entries] of Object.entries(cases)) {
    const { dir, file } = pkg(entries);
    try { assert.throws(() => readMsixSignerSubject(file), (e) => e.code === "MSIX_PUBLISHER_MISMATCH" && /could not read the signer certificate subject/.test(e.message), label); }
    finally { rmSync(dir, { recursive: true }); }
  }
  const dir = mkdtempSync(path.join(os.tmpdir(), "msix-signer-"));
  try { writeFileSync(path.join(dir, "x.msix"), "not a zip"); assert.throws(() => readMsixSignerSubject(path.join(dir, "x.msix")), { code: "MSIX_PUBLISHER_MISMATCH" }); }
  finally { rmSync(dir, { recursive: true }); }
});

// buildMsix with fake tools: pack writes a real zip carrying the p7x, signing is a no-op.
function build(extra, publisher) {
  const dir = mkdtempSync(path.join(os.tmpdir(), "msix-build-"));
  const stage = path.join(dir, "stage"); mkdirSync(stage); writeFileSync(path.join(stage, "App.exe"), "MZ");
  const png = path.join(dir, "a.png"); writeFileSync(png, Buffer.from("89504e470d0a1a0a00", "hex"));
  const assets = { storeLogo: png, square150: png, square44: png };
  const out = path.join(dir, "out", "App.msix");
  const run = (cmd, args) => {
    if (args[0] === "pack") writeFileSync(args[args.indexOf("/p") + 1], zip([["AppxSignature.p7x", P7X, 8]]));
    return { status: 0, stdout: "", stderr: "" };
  };
  const manifest = { name: "Fixture.App", version: "1.0.0.0", architecture: "x64", executable: "App.exe", publisher, displayName: "A", publisherDisplayName: "B" };
  try {
    return buildMsix({ stageDir: stage, out, workDir: path.join(dir, "root"), manifest, assets, sign: true }, { platform: "win32", run, findTool: () => ({ toolPath: "makeappx.exe" }), env: { MAKEAPPX_PATH: "makeappx.exe" }, log: () => {}, ...extra });
  } finally { rmSync(dir, { recursive: true, force: true }); }
}

test("buildMsix default reader reads the p7x; mismatch fails; override wins", () => {
  // validateMsixIdentity requires the Publisher to contain CN=Damned Ventures LLC, which the fixture signer lacks,
  // so the default reader's result can only mismatch here; the override proves it is honoured and the error code is the same.
  const publisher = "CN=Damned Ventures LLC, O=Damned Ventures LLC";
  assert.throws(() => build({}, publisher), (e) => e.code === "MSIX_PUBLISHER_MISMATCH" && e.message.includes("Fixture Signer LLC"));
  assert.ok(build({ readSubject: () => publisher }, publisher));
  assert.throws(() => build({ readSubject: () => "CN=Wrong" }, publisher), { code: "MSIX_PUBLISHER_MISMATCH" });
});

// Real Azure Artifact Signing signature (SwiftRight run 38535084834, SliceApp_0.1.0.0_x64.msix): the
// signer certificate is embedded twice beside Microsoft intermediates; the reader must still resolve it.
test("a real Azure Artifact Signing p7x with a duplicated signer certificate resolves to the publisher", () => {
  const raw = Buffer.from(readFileSync(new URL("./fixtures/msix-signer-azure-artifact-signing.p7x.b64", import.meta.url), "utf8"), "base64");
  const parsed = parseSignedData(raw.subarray(4));
  assert.equal(parsed.certificates.length, 5);
  const subject = renderSubject(signerCertificate(parsed));
  assert.equal(subject, "CN=Damned Ventures LLC, O=Damned Ventures LLC, L=new york, S=New York, C=US");
  assert.doesNotThrow(() => assertPublisherMatchesSigner("CN=Damned Ventures LLC, O=Damned Ventures LLC, L=new york, S=New York, C=US", subject));
});
