// A minimal ustar + gzip writer for tests that need a real npm-shaped archive without running npm or pnpm.
import zlib from "node:zlib";

/** A minimal ustar writer: entries `{ path, data, mode, type, mtime }` in the given order. */
export function tarOf(entries) {
  const blocks = [];
  for (const { path: name, data = "", mode = 0o644, type = "0", mtime = 0 } of entries) {
    const body = Buffer.from(data);
    const header = Buffer.alloc(512);
    header.write(name, 0, 100, "utf8");
    header.write(`${mode.toString(8).padStart(7, "0")}\0`, 100);
    header.write("0000000\0", 108); header.write("0000000\0", 116);
    header.write(`${(type === "5" ? 0 : body.length).toString(8).padStart(11, "0")}\0`, 124);
    header.write(`${mtime.toString(8).padStart(11, "0")}\0`, 136);
    header.write("        ", 148);
    header.write(type, 156);
    header.write("ustar\0", 257); header.write("00", 263);
    let sum = 0; for (const b of header) sum += b;
    header.write(`${sum.toString(8).padStart(6, "0")}\0 `, 148);
    blocks.push(header);
    if (type !== "5") { blocks.push(body); if (body.length % 512) blocks.push(Buffer.alloc(512 - (body.length % 512))); }
  }
  blocks.push(Buffer.alloc(1024));
  return Buffer.concat(blocks);
}
export const tgzOf = (entries, level = 6) => zlib.gzipSync(tarOf(entries), { level });

