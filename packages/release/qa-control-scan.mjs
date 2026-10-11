// Release-blocking scan: a shipped binary must not contain the QA control server.
//
// `rightkit-control` compiles its embedded control server only with the `qa-control` cargo feature, and every
// binary that has it carries QA_CONTROL_MARKER (a `#[used]` static in `embedded/mod.rs`, also read by
// `Control::build`). This module finds that marker in Mach-O, PE and ELF files. It is called for release and
// update builds (signing set, hardening targets, sealed files). The development lane (`development.mjs`)
// never calls it: dev builds are the QA builds.
import { closeSync, lstatSync, openSync, readSync, readdirSync } from "node:fs";
import path from "node:path";

// Keep identical to `QA_CONTROL_MARKER` in crates/rightkit-control/src/embedded/mod.rs.
export const QA_CONTROL_MARKER = "RIGHTKIT-QA-CONTROL-SERVER-PRESENT";
const MARKER = Buffer.from(QA_CONTROL_MARKER, "latin1");
const CHUNK = 4 * 1024 * 1024;

// Mach-O thin (both endians, 32/64-bit), fat/universal (0xCAFEBABE, 0xCAFEBABF), PE ("MZ"), ELF.
const MAGICS = [
  [0xfe, 0xed, 0xfa, 0xce], [0xfe, 0xed, 0xfa, 0xcf], [0xce, 0xfa, 0xed, 0xfe], [0xcf, 0xfa, 0xed, 0xfe],
  [0xca, 0xfe, 0xba, 0xbe], [0xca, 0xfe, 0xba, 0xbf], [0x7f, 0x45, 0x4c, 0x46],
].map((bytes) => Buffer.from(bytes));
const MZ = Buffer.from([0x4d, 0x5a]);

export function isNativeBinaryHeader(head) {
  if (!Buffer.isBuffer(head)) return false;
  if (head.length >= 2 && head.subarray(0, 2).equals(MZ)) return true;
  return MAGICS.some((magic) => head.length >= 4 && head.subarray(0, 4).equals(magic));
}

export function bufferHasQaControlMarker(buffer) {
  return Buffer.from(buffer).includes(MARKER);
}

// Streams the file in chunks with an overlap of marker.length - 1 bytes, so a marker split across chunks is found.
export function fileHasQaControlMarker(file) {
  const fd = openSync(file, "r");
  try {
    const buffer = Buffer.alloc(CHUNK);
    let tail = Buffer.alloc(0);
    for (;;) {
      const read = readSync(fd, buffer, 0, CHUNK, null);
      if (read === 0) return false;
      const window = Buffer.concat([tail, buffer.subarray(0, read)]);
      if (window.includes(MARKER)) return true;
      tail = Buffer.from(window.subarray(Math.max(0, window.length - (MARKER.length - 1))));
    }
  } finally {
    closeSync(fd);
  }
}

function readHead(file) {
  const fd = openSync(file, "r");
  try {
    const head = Buffer.alloc(4);
    const read = readSync(fd, head, 0, 4, 0);
    return head.subarray(0, read);
  } finally {
    closeSync(fd);
  }
}

// Returns findings; empty means clean. Fails closed: a path that cannot be read is a finding, not a pass.
// Directories (a `.app`) are walked. Symlinks are not followed (they would escape the bundle).
export function scanForQaControl(paths) {
  const findings = [];
  const visit = (target) => {
    let stat;
    try { stat = lstatSync(target); } catch (error) { findings.push({ file: target, reason: `could not scan: ${error.code ?? error.message}` }); return; }
    if (stat.isSymbolicLink()) return;
    if (stat.isDirectory()) {
      let names;
      try { names = readdirSync(target); } catch (error) { findings.push({ file: target, reason: `could not list: ${error.code ?? error.message}` }); return; }
      for (const name of names.sort()) visit(path.join(target, name));
      return;
    }
    if (!stat.isFile()) return;
    try {
      if (!isNativeBinaryHeader(readHead(target))) return;
      if (fileHasQaControlMarker(target)) findings.push({ file: target, reason: `contains ${QA_CONTROL_MARKER}` });
    } catch (error) {
      findings.push({ file: target, reason: `could not scan: ${error.code ?? error.message}` });
    }
  };
  for (const target of paths) visit(path.resolve(target));
  return findings;
}

export function assertNoQaControl(paths, label = "release") {
  const findings = scanForQaControl(paths);
  if (findings.length === 0) return;
  throw new Error(
    `${label}: the QA control server must never ship (rightkit-control feature \`qa-control\`); `
    + `${findings.length} finding(s):\n${findings.map((f) => `- ${f.file}: ${f.reason}`).join("\n")}\n`
    + "Build release and update tiers without the app's qa-native feature.",
  );
}
