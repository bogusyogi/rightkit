// Finder records follow dmgbuild's ds_store/mac_alias formats. No Finder IPC.
// https://github.com/dmgbuild/ds_store/tree/main/src/ds_store
// https://github.com/dmgbuild/mac_alias/blob/main/src/mac_alias/alias.py
import { statSync } from "node:fs";
import path from "node:path";

const u32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; };
const u16 = (n) => { const b = Buffer.alloc(2); b.writeUInt16BE(n); return b; };
const utf16 = (s) => Buffer.from(s, "utf16le").swap16();
const uint = (n, size) => { const b = Buffer.alloc(size); b.writeUIntBE(n, 0, size); return b; };

// Small binary-plist encoder for Finder view dictionaries (strings, data, real,
// integer, boolean). Object references & offset widths are computed per plist.
export function finderPlist(value) {
  const objects = [];
  function add(v, key) {
    const id = objects.length;
    objects.push(null);
    if (Buffer.isBuffer(v)) objects[id] = { kind: 0x40, bytes: v, length: v.length };
    else if (typeof v === "string") objects[id] = { kind: 0x60, bytes: utf16(v), length: v.length };
    else if (typeof v === "boolean") objects[id] = { bytes: Buffer.from([v ? 9 : 8]) };
    else if (typeof v === "number") {
      if (!Number.isFinite(v)) throw new TypeError("non-finite plist value");
      const bytes = Buffer.alloc(9);
      if (["viewOptionsVersion", "backgroundType", "SidebarWidth"].includes(key)) {
        bytes[0] = 0x13; bytes.writeBigInt64BE(BigInt(v), 1);
      } else { bytes[0] = 0x23; bytes.writeDoubleBE(v, 1); }
      objects[id] = { bytes };
    } else {
      const entries = Object.entries(v);
      objects[id] = { kind: 0xd0, length: entries.length,
        refs: [...entries.map(([k]) => add(k)), ...entries.map(([k, val]) => add(val, k))] };
    }
    return id;
  }
  const root = add(value), refSize = objects.length < 256 ? 1 : 2;
  const prefix = (kind, length) => length < 15 ? Buffer.from([kind | length])
    : Buffer.concat([Buffer.from([kind | 15, 0x12]), u32(length)]);
  const chunks = objects.map((o) => o.kind == null ? o.bytes : Buffer.concat([
    prefix(o.kind, o.length), o.refs ? Buffer.concat(o.refs.map((ref) => uint(ref, refSize))) : o.bytes,
  ]));
  let offset = 8;
  const offsets = chunks.map((chunk) => { const start = offset; offset += chunk.length; return start; });
  const offsetSize = offset < 256 ? 1 : offset < 65536 ? 2 : 4;
  const trailer = Buffer.alloc(32);
  trailer[6] = offsetSize; trailer[7] = refSize;
  trailer.writeBigUInt64BE(BigInt(objects.length), 8);
  trailer.writeBigUInt64BE(BigInt(root), 16);
  trailer.writeBigUInt64BE(BigInt(offset), 24);
  return Buffer.concat([Buffer.from("bplist00"), ...chunks, ...offsets.map((n) => uint(n, offsetSize)), trailer]);
}

/** Carbon Alias v2, with mounted HFS+ CNIDs, dates, Unicode & volume-relative paths. */
export function backgroundAlias({ mount, volname, relative, stat = statSync }) {
  const target = path.join(mount, relative), file = stat(target), volume = stat(mount), folder = stat(path.dirname(target));
  const macDate = (s) => Math.floor(s.birthtimeMs / 1000 + 2082844800);
  const header = Buffer.alloc(150);
  header.writeUInt16BE(2, 6); // version; kind=file
  const pascal = (text, offset, size) => {
    const bytes = Buffer.from(text.replaceAll(":", "/"));
    header[offset] = Math.min(bytes.length, size - 1); bytes.copy(header, offset + 1, 0, size - 1);
  };
  pascal(volname, 10, 28); header.writeUInt32BE(macDate(volume), 38);
  header.write("H+", 42, "ascii");
  header.writeUInt32BE(folder.ino, 46); pascal(path.basename(target), 50, 64);
  header.writeUInt32BE(file.ino, 114); header.writeUInt32BE(macDate(file), 118);
  header.writeInt16BE(-1, 130); header.writeInt16BE(-1, 132);
  const tag = (code, bytes) => Buffer.concat([u16(code), u16(bytes.length), bytes, ...(bytes.length % 2 ? [Buffer.alloc(1)] : [])]);
  const unicode = (s) => Buffer.concat([u16(s.length), utf16(s)]);
  const highDate = (s) => { const b = Buffer.alloc(8); b.writeBigUInt64BE(BigInt(Math.floor((s.birthtimeMs / 1000 + 2082844800) * 65536))); return b; };
  const carbonPath = `${volname}:${relative.split(path.sep).map((s) => s.replaceAll(":", "/")).join(":\0")}`;
  const bytes = Buffer.concat([header, tag(0, Buffer.from(path.basename(path.dirname(target)))),
    tag(16, highDate(volume)), tag(17, highDate(file)), tag(1, u32(folder.ino)), tag(2, Buffer.from(carbonPath)),
    tag(14, unicode(path.basename(target))), tag(15, unicode(volname)),
    tag(18, Buffer.from(`/${relative.split(path.sep).join("/")}`)), tag(19, Buffer.from(mount)),
    Buffer.from([255, 255, 0, 0])]);
  if (bytes.length > 32767) throw new RangeError("background alias too large");
  bytes.writeUInt16BE(bytes.length, 4);
  return bytes;
}

/** One sorted B-tree leaf in a valid Bud1 allocator, sufficient for DMG layout. */
export function dmgDSStore({ appName, iconSize = 128, appPosition = [158, 231],
  applicationsPosition = [492, 231], window = [660, 400], windowPosition = [100, 100], alias } = {}) {
  const entries = [];
  const add = (name, code, type, bytes) => entries.push({ name, code, bytes: Buffer.concat([
    u32(name.length), utf16(name), Buffer.from(code + type), ...(type === "blob" ? [u32(bytes.length)] : []), bytes,
  ]) });
  add(".", "vSrn", "long", u32(1));
  add(".", "icvl", "type", Buffer.from("icnv"));
  add(".", "bwsp", "blob", finderPlist({ ShowStatusBar: false, ShowToolbar: false, ShowTabView: false,
    ShowPathbar: false, ShowSidebar: false, ContainerShowSidebar: false, PreviewPaneVisibility: false,
    SidebarWidth: 0, WindowBounds: `{{${windowPosition[0]}, ${windowPosition[1]}}, {${window[0]}, ${window[1]}}}` }));
  add(".", "icvp", "blob", finderPlist({ viewOptionsVersion: 1, backgroundType: alias ? 2 : 0,
    backgroundColorRed: 1, backgroundColorGreen: 1, backgroundColorBlue: 1,
    gridOffsetX: 0, gridOffsetY: 0, gridSpacing: 100, arrangeBy: "none", showIconPreview: true,
    showItemInfo: false, labelOnBottom: true, textSize: 16, iconSize,
    scrollPositionX: 0, scrollPositionY: 0, ...(alias ? { backgroundImageAlias: alias } : {}) }));
  for (const [name, point] of [[appName, appPosition], ["Applications", applicationsPosition]]) {
    add(name, "Iloc", "blob", Buffer.concat([u32(point[0]), u32(point[1]), u32(0xffffffff), u32(0xffff0000)]));
  }
  entries.sort((a, b) => a.name.toLowerCase() < b.name.toLowerCase() ? -1
    : a.name.toLowerCase() > b.name.toLowerCase() ? 1 : a.code < b.code ? -1 : 1);
  const leafData = Buffer.concat([u32(0), u32(entries.length), ...entries.map((e) => e.bytes)]);
  const pagePower = Math.max(12, Math.ceil(Math.log2(leafData.length))), pageSize = 2 ** pagePower;
  if (pagePower > 20) throw new RangeError("DMG layout too large");
  // Larger leaf starts at its own aligned boundary; unused buddies remain free.
  const leafOffset = pageSize, file = Buffer.alloc(leafOffset + pageSize + 4);
  Buffer.concat([u32(1), Buffer.from("Bud1"), u32(2048), u32(2048), u32(2048),
    Buffer.from("0000100c000000870000200b00000000", "hex")]).copy(file);
  const root = Buffer.alloc(2048); let pos = 0;
  const write = (bytes) => { bytes.copy(root, pos); pos += bytes.length; };
  write(u32(3)); write(u32(0)); write(u32(2048 | 11)); write(u32(32 | 5)); write(u32(leafOffset | pagePower));
  pos = 1032; write(u32(1)); write(Buffer.from([4])); write(Buffer.from("DSDB")); write(u32(1));
  for (let power = 0; power < 32; power++) {
    const free = power >= 6 && power <= 10 || power >= 12 && power !== pagePower && power <= 30;
    write(u32(free ? 1 : 0)); if (free) write(u32(2 ** power));
  }
  root.copy(file, 2052);
  Buffer.concat([u32(2), u32(0), u32(entries.length), u32(1), u32(pageSize)]).copy(file, 36);
  leafData.copy(file, leafOffset + 4);
  return file;
}
