// A single-holder lock file for the iOS upload, the same mechanism as the build lane's run lock (build-release.mjs acquireLock):
// the file is created exclusively with the holder's pid, and a lock whose holder is no longer alive is replaced. Two
// `upload --apply` runs of one release can therefore never interleave. The file lives in `.right-release/locks/`, which the source gate
// already exempts from the clean-tree check.
//
// Replacing a stale lock is race-free: the stale file is first RENAMED to a unique tombstone (rename is atomic, so exactly one process
// wins; the others see ENOENT and just retry the exclusive create), and only the winner unlinks the tombstone. A lock that was created
// but not yet written (empty or unparsable) is held until it is older than a short grace period; it is never taken for stale at once.
// If the file renamed away turns out not to be the stale one that was judged (another process replaced it in between), it is put back.
import * as nodeFs from "node:fs";
import path from "node:path";

export const LOCK_GRACE_MS = 5_000;

export class IosLockError extends Error {
  constructor(code, message) { super(message); this.name = "IosLockError"; this.code = code; }
}

const processAlive = (pid) => {
  if (!Number.isInteger(Number(pid)) || Number(pid) <= 0) return false;
  try { process.kill(Number(pid), 0); return true; } catch (error) { return error.code === "EPERM"; }
};

export function acquireFileLock(file, label, { fs = nodeFs, isAlive = processAlive, pid = process.pid, now = () => Date.now(), graceMs = LOCK_GRACE_MS } = {}) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  const locked = (detail) => new IosLockError("IOS_UPLOAD_LOCKED", `${label} is already running (${detail}, lock ${file})`);
  for (let attempt = 0; attempt < 5; attempt += 1) {
    try {
      const fd = fs.openSync(file, "wx");
      try { fs.writeFileSync(fd, `${JSON.stringify({ pid, label, startedAt: new Date().toISOString() })}\n`); } finally { fs.closeSync(fd); }
      let released = false;
      const release = () => {
        if (released) return;
        released = true;
        process.removeListener("exit", release);
        try { fs.unlinkSync(file); } catch { /* already released */ }
      };
      process.on("exit", release);
      return { release };
    } catch (error) {
      if (error.code !== "EEXIST") throw error;
    }
    let raw;
    try { raw = fs.readFileSync(file, "utf8"); } catch (error) {
      if (error.code === "ENOENT") continue; // released meanwhile: create it
      throw error;
    }
    let holder;
    try { holder = JSON.parse(raw); } catch { holder = undefined; }
    if (holder && typeof holder === "object") {
      if (isAlive(holder.pid) && holder.pid !== pid) throw locked(`pid ${holder.pid}`);
    } else {
      // created but not written yet (or damaged): held until it is older than the grace period
      let age;
      try { age = now() - fs.statSync(file).mtimeMs; } catch (error) { if (error.code === "ENOENT") continue; throw error; }
      if (age < graceMs) throw locked("its lock file is still being written");
    }
    // stale: take it away atomically; only one process can win this rename
    const tombstone = `${file}.stale-${pid}-${now()}-${Math.random().toString(16).slice(2)}`;
    try { fs.renameSync(file, tombstone); } catch (error) {
      if (error.code === "ENOENT") continue; // somebody else won; retry the exclusive create
      throw error;
    }
    let taken;
    try { taken = fs.readFileSync(tombstone, "utf8"); } catch { taken = raw; }
    if (taken !== raw) {
      // not the stale file we judged: a live lock was replaced under us. Put it back if the name is still free.
      try { fs.linkSync(tombstone, file); } catch { /* the name is taken again; the holder will see its lock gone only if it checks */ }
      try { fs.unlinkSync(tombstone); } catch { /* ignore */ }
      throw locked("another process took the lock while it was being replaced");
    }
    try { fs.unlinkSync(tombstone); } catch { /* ignore */ }
  }
  throw new IosLockError("IOS_UPLOAD_LOCKED", `${label}: cannot take the lock ${file}`);
}
