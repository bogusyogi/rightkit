// Minimal PE import reader: names of DLLs a Windows binary loads (regular + delay-load).
// Pure Buffer parsing so dependency closure checks run on any host.

export const PE_MACHINES = Object.freeze({ 0x8664: "x64", 0xaa64: "arm64", 0x014c: "x86" });

function fail(message) { throw Object.assign(new Error(message), { code: "PE_INVALID" }); }

export function readPeImports(bytes) {
  const buf = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
  if (buf.length < 0x40 || buf.readUInt16LE(0) !== 0x5a4d) fail("not a PE file (missing MZ)");
  const pe = buf.readUInt32LE(0x3c);
  if (pe + 24 > buf.length || buf.readUInt32LE(pe) !== 0x00004550) fail("not a PE file (missing PE signature)");
  const machine = buf.readUInt16LE(pe + 4);
  const sectionCount = buf.readUInt16LE(pe + 6);
  const optionalSize = buf.readUInt16LE(pe + 20);
  const optional = pe + 24;
  const magic = buf.readUInt16LE(optional);
  if (magic !== 0x20b && magic !== 0x10b) fail("unsupported PE optional header");
  const directories = optional + (magic === 0x20b ? 112 : 96);
  const sectionsAt = optional + optionalSize;
  const sections = [];
  for (let i = 0; i < sectionCount; i++) {
    const at = sectionsAt + i * 40;
    if (at + 40 > buf.length) fail("truncated PE section table");
    sections.push({ virtualSize: buf.readUInt32LE(at + 8), rva: buf.readUInt32LE(at + 12), rawSize: buf.readUInt32LE(at + 16), rawPtr: buf.readUInt32LE(at + 20) });
  }
  const toOffset = (rva) => {
    for (const s of sections) if (rva >= s.rva && rva < s.rva + Math.max(s.virtualSize, s.rawSize)) return s.rawPtr + (rva - s.rva);
    fail(`RVA 0x${rva.toString(16)} is outside every section`);
  };
  const cstring = (rva) => {
    const start = toOffset(rva);
    const end = buf.indexOf(0, start);
    if (end < 0) fail("unterminated import name");
    return buf.toString("latin1", start, end);
  };
  const directory = (index) => {
    const at = directories + index * 8;
    return at + 8 <= buf.length ? { rva: buf.readUInt32LE(at), size: buf.readUInt32LE(at + 4) } : { rva: 0, size: 0 };
  };
  const imports = new Set(), delayImports = new Set();
  const table = directory(1);
  if (table.rva) {
    for (let at = toOffset(table.rva); at + 20 <= buf.length; at += 20) {
      const name = buf.readUInt32LE(at + 12);
      if (!name && !buf.readUInt32LE(at) && !buf.readUInt32LE(at + 16)) break;
      imports.add(cstring(name));
    }
  }
  const delay = directory(13);
  if (delay.rva) {
    for (let at = toOffset(delay.rva); at + 32 <= buf.length; at += 32) {
      const attrs = buf.readUInt32LE(at), name = buf.readUInt32LE(at + 4);
      if (!attrs && !name) break;
      delayImports.add(cstring(name));
    }
  }
  return { machine, architecture: PE_MACHINES[machine] ?? `0x${machine.toString(16)}`, imports: [...imports], delayImports: [...delayImports] };
}
