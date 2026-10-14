// Default signer-subject reader for .msix / .msixbundle.
// PowerShell Get-AuthenticodeSignature returns nothing for MSIX on hosted Windows runners, so the subject
// is read from the package's own AppxSignature.p7x: a "PKCX" magic followed by a PKCS#7 SignedData.
// Pure Node (node:zlib + node:crypto X509Certificate + a small DER walker); no dependency, any host.
// The certificate chosen is the one named by SignerInfo issuerAndSerialNumber (or subjectKeyIdentifier),
// never merely the first certificate. Every failure throws MSIX_PUBLISHER_MISMATCH (fail closed).

import { X509Certificate } from "node:crypto";
import { closeSync, fstatSync, openSync, readSync } from "node:fs";
import { inflateRawSync } from "node:zlib";

const fail = (message) => Object.assign(new Error(message), { code: "MSIX_PUBLISHER_MISMATCH" });
const MAX_P7X = 4 * 1024 * 1024;
const SIGNED_DATA_OID = "1.2.840.113549.1.7.2";

// --- zip container -----------------------------------------------------------------------------
const u64 = (b, o) => Number(b.readBigUInt64LE(o));

function readAt(fd, position, length) {
  const buffer = Buffer.alloc(length);
  let done = 0;
  while (done < length) {
    const n = readSync(fd, buffer, done, length - done, position + done);
    if (n === 0) break;
    done += n;
  }
  return buffer.subarray(0, done);
}

/** Return the bytes of the named entry of a zip file, or null when absent. */
export function readZipEntry(file, wanted, { maxBytes = MAX_P7X } = {}) {
  const fd = openSync(file, "r");
  try {
    const size = fstatSync(fd).size;
    const tailLength = Math.min(size, 22 + 65535);
    const tail = readAt(fd, size - tailLength, tailLength);
    let entries; let dirSize; let dirOffset; let found = false;
    for (let i = tail.length - 22; i >= 0; i -= 1) {
      if (tail.readUInt32LE(i) !== 0x06054b50) continue;
      if (i + 22 + tail.readUInt16LE(i + 20) > tail.length) continue;
      entries = tail.readUInt16LE(i + 10); dirSize = tail.readUInt32LE(i + 12); dirOffset = tail.readUInt32LE(i + 16);
      if (entries === 0xffff || dirSize === 0xffffffff || dirOffset === 0xffffffff) {
        const at = size - tailLength + i;
        const loc = at >= 20 ? readAt(fd, at - 20, 20) : Buffer.alloc(0);
        if (loc.length < 20 || loc.readUInt32LE(0) !== 0x07064b50) throw new Error("zip64 locator missing");
        const rec = readAt(fd, u64(loc, 8), 56);
        if (rec.length < 56 || rec.readUInt32LE(0) !== 0x06064b50) throw new Error("zip64 end record missing");
        entries = u64(rec, 32); dirSize = u64(rec, 40); dirOffset = u64(rec, 48);
      }
      found = true;
      break;
    }
    if (!found) throw new Error("not a zip container (end of central directory not found)");
    if (dirSize > 256 * 1024 * 1024 || dirOffset + dirSize > size) throw new Error("central directory out of bounds");
    const dir = readAt(fd, dirOffset, dirSize);
    if (dir.length !== dirSize) throw new Error("central directory truncated");
    let at = 0;
    for (let n = 0; n < entries; n += 1) {
      if (at + 46 > dir.length || dir.readUInt32LE(at) !== 0x02014b50) throw new Error("bad central directory entry");
      const flags = dir.readUInt16LE(at + 8); const method = dir.readUInt16LE(at + 10);
      let compressed = dir.readUInt32LE(at + 20); let uncompressed = dir.readUInt32LE(at + 24);
      const nameLen = dir.readUInt16LE(at + 28); const extraLen = dir.readUInt16LE(at + 30); const commentLen = dir.readUInt16LE(at + 32);
      let local = dir.readUInt32LE(at + 42);
      const end = at + 46 + nameLen + extraLen + commentLen;
      if (end > dir.length) throw new Error("central directory entry out of bounds");
      const name = dir.subarray(at + 46, at + 46 + nameLen).toString("utf8");
      if (name === wanted) {
        const extra = dir.subarray(at + 46 + nameLen, at + 46 + nameLen + extraLen);
        for (let e = 0; e + 4 <= extra.length; e += 4 + extra.readUInt16LE(e + 2)) {
          if (extra.readUInt16LE(e) !== 0x0001) continue;
          let p = e + 4;
          if (uncompressed === 0xffffffff) { uncompressed = u64(extra, p); p += 8; }
          if (compressed === 0xffffffff) { compressed = u64(extra, p); p += 8; }
          if (local === 0xffffffff) local = u64(extra, p);
        }
        if (flags & 1) throw new Error(`${wanted} is encrypted`);
        if (uncompressed > maxBytes || compressed > maxBytes) throw new Error(`${wanted} larger than ${maxBytes} bytes`);
        const lh = readAt(fd, local, 30);
        if (lh.length < 30 || lh.readUInt32LE(0) !== 0x04034b50) throw new Error("bad local header");
        const start = local + 30 + lh.readUInt16LE(26) + lh.readUInt16LE(28);
        if (start + compressed > size) throw new Error("entry data out of bounds");
        const raw = readAt(fd, start, compressed);
        let data;
        if (method === 0) data = raw;
        else if (method === 8) data = inflateRawSync(raw, { maxOutputLength: maxBytes });
        else throw new Error(`unsupported zip method ${method}`);
        if (data.length !== uncompressed) throw new Error(`${wanted} length differs from its directory entry`);
        return data;
      }
      at = end;
    }
    return null;
  } finally { closeSync(fd); }
}

// --- DER ---------------------------------------------------------------------------------------
/** Read one TLV at `at`; returns { tag, start (of header), body (start), end }. */
function tlv(buf, at, limit = buf.length) {
  if (at + 2 > limit) throw new Error("DER truncated");
  const tag = buf[at]; let len = buf[at + 1]; let body = at + 2;
  if ((tag & 0x1f) === 0x1f) throw new Error("DER high tag numbers unsupported");
  if (len & 0x80) {
    const n = len & 0x7f;
    if (n === 0) return indefinite(buf, tag, at, limit); // BER indefinite length
    if (n > 4 || body + n > limit) throw new Error("DER bad length");
    len = 0;
    for (let i = 0; i < n; i += 1) len = len * 256 + buf[body + i];
    body += n;
  }
  if (body + len > limit) throw new Error("DER length exceeds container");
  return { tag, start: at, body, end: body + len };
}

// BER indefinite-length constructed value: children until 00 00.
function indefinite(buf, tag, at, limit) {
  if (!(tag & 0x20)) throw new Error("indefinite length on primitive");
  const body = at + 2; let p = body;
  while (true) {
    if (p + 2 > limit) throw new Error("BER unterminated");
    if (buf[p] === 0 && buf[p + 1] === 0) return { tag, start: at, body, end: p, tail: p + 2 };
    p = next(tlv(buf, p, limit));
  }
}
const next = (t) => t.tail ?? t.end;
const children = (buf, t) => {
  const out = []; let p = t.body;
  while (p < t.end) { const c = tlv(buf, p, t.end); out.push(c); p = next(c); }
  return out;
};
const whole = (buf, t) => buf.subarray(t.start, next(t));
const oid = (buf, t) => {
  const b = buf.subarray(t.body, t.end); const parts = [Math.floor(b[0] / 40), b[0] % 40]; let v = 0;
  for (let i = 1; i < b.length; i += 1) { v = v * 128 + (b[i] & 0x7f); if (!(b[i] & 0x80)) { parts.push(v); v = 0; } }
  return parts.join(".");
};
const intHex = (buf, t) => { let h = buf.subarray(t.body, t.end).toString("hex").replace(/^(00)+(?=.)/, ""); return h.toLowerCase(); };

/** Parse PKCS#7 SignedData; return { certificates: [Buffer], signer: { issuer: Buffer, serial: string } | { ski: string } }. */
export function parseSignedData(der) {
  const ci = tlv(der, 0);
  if (ci.tag !== 0x30) throw new Error("ContentInfo is not a SEQUENCE");
  const [type, content] = children(der, ci);
  if (!type || type.tag !== 0x06 || oid(der, type) !== SIGNED_DATA_OID) throw new Error("not a PKCS#7 SignedData");
  if (!content || content.tag !== 0xa0) throw new Error("SignedData content missing");
  const sd = children(der, content)[0];
  if (!sd || sd.tag !== 0x30) throw new Error("SignedData is not a SEQUENCE");
  const parts = children(der, sd);
  const certsSet = parts.find((p) => p.tag === 0xa0);
  const signerSet = parts.filter((p) => p.tag === 0x31).at(-1);
  if (!certsSet) throw new Error("SignedData carries no certificates");
  if (!signerSet) throw new Error("SignedData carries no SignerInfos");
  const certificates = children(der, certsSet).filter((c) => c.tag === 0x30).map((c) => Buffer.from(whole(der, c)));
  const infos = children(der, signerSet);
  if (infos.length !== 1) throw new Error(`expected exactly one SignerInfo, found ${infos.length}`);
  const [, sid] = children(der, infos[0]);
  if (!sid) throw new Error("SignerInfo has no signer identifier");
  let signer;
  if (sid.tag === 0x30) {
    const [issuer, serial] = children(der, sid);
    signer = { issuer: Buffer.from(whole(der, issuer)), serial: intHex(der, serial) };
  } else if (sid.tag === 0x80) signer = { ski: Buffer.from(der.subarray(sid.body, sid.end)).toString("hex") };
  else throw new Error("unsupported SignerInfo signer identifier");
  return { certificates, signer };
}

/** TBSCertificate issuer (DER) and serial (hex) of a certificate. */
function certIdentity(cert) {
  const [tbs] = children(cert, tlv(cert, 0));
  const f = children(cert, tbs);
  const i = f[0].tag === 0xa0 ? 1 : 0; // optional [0] version
  return { serial: intHex(cert, f[i]), issuer: Buffer.from(whole(cert, f[i + 2])) };
}

export function signerCertificate(parsed) {
  const match = parsed.certificates.filter((c) => {
    try {
      if (parsed.signer.ski) {
        const x = new X509Certificate(c);
        return x.raw && (x.subjectKeyIdentifier ?? "").replace(/:/g, "").toLowerCase() === parsed.signer.ski;
      }
      const id = certIdentity(c);
      return id.serial === parsed.signer.serial && id.issuer.equals(parsed.signer.issuer);
    } catch { return false; }
  });
  if (match.length !== 1) throw new Error(`signer certificate not found in the signature (${match.length} matches)`);
  return new X509Certificate(match[0]);
}

// --- subject rendering ---------------------------------------------------------------------------
const needsQuote = (v) => /[,+"\\<>;=\r\n]/.test(v) || /^[ #]|\s$/.test(v);
/** Node renders subject as "K=V" lines; join as an RFC 2253-style display string like .NET Subject ("CN=a, O=b"). */
export function renderSubject(x509) {
  return String(x509.subject).split("\n").filter(Boolean).map((line) => {
    const at = line.indexOf("=");
    const key = line.slice(0, at); const value = line.slice(at + 1);
    return `${key}=${needsQuote(value) ? `"${value.replace(/"/g, '""')}"` : value}`;
  }).join(", ");
}

/** Read the signer certificate subject of a signed .msix / .msixbundle from AppxSignature.p7x. Throws MSIX_PUBLISHER_MISMATCH. */
export function readMsixSignerSubject(file) {
  let p7x;
  try { p7x = readZipEntry(file, "AppxSignature.p7x"); } catch (error) { throw fail(`could not read the signer certificate subject: ${error.message}`); }
  if (!p7x) throw fail("could not read the signer certificate subject: AppxSignature.p7x is missing (package is unsigned)");
  try {
    if (p7x.length < 5 || p7x.subarray(0, 4).toString("latin1") !== "PKCX") throw new Error("AppxSignature.p7x lacks the PKCX header");
    return renderSubject(signerCertificate(parseSignedData(p7x.subarray(4))));
  } catch (error) {
    throw fail(`could not read the signer certificate subject: AppxSignature.p7x unparsable (${error.message})`);
  }
}
