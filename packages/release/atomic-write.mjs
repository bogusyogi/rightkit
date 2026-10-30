// The one durable replace-write for authoritative result files (GB-26; the gate's stage journal, the protected QA receipts): the bytes go to a temp
// file beside the destination, are fsynced, and replace the destination with one rename, then the directory is fsynced. A crash, a full disk or a
// failing step leaves the previous valid file untouched and the new content wholly present or wholly absent, never a truncated mix; the temp file is
// removed on every failure path and the failure is typed (RK_ATOMIC_WRITE, with the step that failed). The replaced file keeps its permission bits.
// A LEAF module: node builtins only.
import { randomBytes } from "node:crypto";
import nodeFs from "node:fs";
import path from "node:path";

export const ATOMIC_WRITE_FAILED = "RK_ATOMIC_WRITE";

export class AtomicWriteError extends Error {
  constructor(step, file, cause) {
    super(`atomic write of ${file} failed at ${step}: ${cause?.message ?? cause}`);
    this.name = "AtomicWriteError";
    this.code = ATOMIC_WRITE_FAILED;
    this.step = step;
    this.file = file;
    if (cause) this.cause = cause;
  }
}

/** Writes `data` (string or Buffer) to `file` atomically. `fs` is a seam for fault injection (the node:fs functions it uses). */
export function writeFileAtomic(file, data, { mode, fs = nodeFs } = {}) {
  const dir = path.dirname(file);
  const temp = path.join(dir, `.${path.basename(file)}.${process.pid}.${randomBytes(6).toString("hex")}.tmp`);
  let step = "mkdir";
  let fd;
  let created = false;
  try {
    fs.mkdirSync(dir, { recursive: true });
    let keep = mode;
    if (keep === undefined) { try { keep = fs.statSync(file).mode & 0o777; } catch { keep = undefined; } }
    step = "open";
    fd = fs.openSync(temp, "wx", keep ?? 0o644);
    created = true;
    step = "write";
    const bytes = Buffer.isBuffer(data) ? data : Buffer.from(String(data), "utf8");
    let at = 0;
    while (at < bytes.length) at += fs.writeSync(fd, bytes, at, bytes.length - at);
    step = "fsync";
    fs.fsyncSync(fd);
    step = "close";
    const closing = fd; fd = undefined;
    fs.closeSync(closing);
    step = "rename";
    fs.renameSync(temp, file);
    created = false;
  } catch (error) {
    if (fd !== undefined) { try { fs.closeSync(fd); } catch { /* the original failure is the one reported */ } }
    if (created) { try { fs.rmSync(temp, { force: true }); } catch { /* a leftover temp is never read as the destination */ } }
    throw error instanceof AtomicWriteError ? error : new AtomicWriteError(step, file, error);
  }
  // The rename is done: the new content is the file. Making the directory entry durable is best effort (not every platform can open a directory).
  let dirFd;
  try { dirFd = fs.openSync(dir, "r"); fs.fsyncSync(dirFd); } catch { /* POSIX only; the replace itself already happened */ } finally { if (dirFd !== undefined) { try { fs.closeSync(dirFd); } catch { /* closed */ } } }
}
