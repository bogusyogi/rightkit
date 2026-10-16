// Test-only builders for tiny fixture AABs (ZIP files) and PKCS#7 signature blocks. Nothing here is a real certificate.
import { createHash } from "node:crypto";
import { deflateRawSync } from "node:zlib";
import { crc32 } from "./android-aab-scan.mjs";
import { QA_CONTROL_MARKER } from "./qa-control-scan.mjs";

export const ELF = Buffer.from([0x7f, 0x45, 0x4c, 0x46]);
export const filler = (n) => Buffer.alloc(n, 0x90);
export const cleanLib = () => Buffer.concat([ELF, filler(256), Buffer.from("RIGHTKIT-QA-CONTROL")]);
export const markedLib = () => Buffer.concat([ELF, filler(64), Buffer.from(QA_CONTROL_MARKER), filler(64)]);

export function derLength(length) {
  if (length < 0x80) return Buffer.from([length]);
  const bytes = [];
  for (let n = length; n > 0; n = Math.floor(n / 256)) bytes.unshift(n % 256);
  return Buffer.from([0x80 | bytes.length, ...bytes]);
}
export const der = (tag, ...parts) => { const body = Buffer.concat(parts); return Buffer.concat([Buffer.from([tag]), derLength(body.length), body]); };

// A stand-in "certificate": any DER SEQUENCE. `subject` text lets a test plant the debug-certificate marker.
export const fakeCertificate = (subject = "CN=Fixture Upload Key") => der(0x30, der(0x0c, Buffer.from(subject)));
export const sha256Hex = (bytes) => createHash("sha256").update(bytes).digest("hex");

// PKCS#7 SignedData: ContentInfo { OID, [0] { SignedData { version, digestAlgorithms, contentInfo, [0] certificates, signerInfos } } }
export function pkcs7(certificates) {
  const oid = der(0x06, Buffer.from([0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02]));
  const signedData = der(0x30,
    der(0x02, Buffer.from([1])),
    der(0x31),
    der(0x30, der(0x06, Buffer.from([0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x01]))),
    der(0xa0, ...certificates),
    der(0x31));
  return der(0x30, oid, der(0xa0, signedData));
}

// entries: [{ name, data, method?: "store" | "deflate" }]; returns the ZIP bytes (no ZIP64, no comment).
export function zip(entries) {
  const locals = []; const centrals = []; let offset = 0;
  for (const { name, data, method = "deflate" } of entries) {
    const nameBytes = Buffer.from(name); const raw = Buffer.from(data);
    const body = method === "store" ? raw : deflateRawSync(raw);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(method === "store" ? 0 : 8, 8);
    local.writeUInt32LE(crc32(raw), 14); local.writeUInt32LE(body.length, 18); local.writeUInt32LE(raw.length, 22); local.writeUInt16LE(nameBytes.length, 26);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6); central.writeUInt16LE(method === "store" ? 0 : 8, 10);
    central.writeUInt32LE(crc32(raw), 16); central.writeUInt32LE(body.length, 20); central.writeUInt32LE(raw.length, 24); central.writeUInt16LE(nameBytes.length, 28); central.writeUInt32LE(offset, 42);
    locals.push(local, nameBytes, body); centrals.push(central, nameBytes);
    offset += local.length + nameBytes.length + body.length;
  }
  const directory = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(entries.length, 8); end.writeUInt16LE(entries.length, 10);
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, directory, end]);
}

export const FIXTURE_CERT = fakeCertificate();
export const FIXTURE_CERT_SHA256 = sha256Hex(FIXTURE_CERT);

// A well-formed, signed AAB. `override` replaces or (with null) removes entries by name; `extra` appends entries.
export function aab({ libs, certificate = FIXTURE_CERT, override = {}, extra = [], signed = true } = {}) {
  const base = [
    { name: "BundleConfig.pb", data: Buffer.from("config") },
    { name: "base/manifest/AndroidManifest.xml", data: Buffer.from("manifest") },
    { name: "base/assets/index.js", data: Buffer.from("console.log('release')") },
    ...(libs ?? [
      { name: "base/lib/arm64-v8a/libapp.so", data: cleanLib() },
      { name: "base/lib/x86_64/libapp.so", data: cleanLib(), method: "store" },
    ]),
    ...(signed ? [
      { name: "META-INF/MANIFEST.MF", data: Buffer.from("Manifest-Version: 1.0\n") },
      { name: "META-INF/UPLOAD.SF", data: Buffer.from("Signature-Version: 1.0\n") },
      { name: "META-INF/UPLOAD.RSA", data: pkcs7([certificate]) },
    ] : []),
  ];
  const entries = base.filter((entry) => override[entry.name] !== null).map((entry) => (override[entry.name] ? { ...entry, data: override[entry.name] } : entry));
  return zip([...entries, ...extra]);
}
