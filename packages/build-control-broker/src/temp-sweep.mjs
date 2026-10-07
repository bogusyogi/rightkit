import { readdir, rename, rm, stat } from 'node:fs/promises';
import { join } from 'node:path';

/** Never swept: broker-owned live endpoints in the shared temp root. */
const KEEP = new Set(['sccache-gateway.sock']);

/**
 * Remove leaked top-level entries from the broker's shared job temp root.
 *
 * Every managed job gets TMP/TEMP/TMPDIR = this root, and test harnesses leak
 * into it (tempfile dirs a child still held when they were dropped, killed
 * runs). An entry is removed only when it has been untouched for `olderThanMs`
 * AND predates the oldest still-running job (`notAfterMs`), so nothing a live
 * job could own is touched. Entries are renamed before deletion so a locked
 * tree (Windows) fails whole and is retried on the next sweep. Never throws.
 */
export async function sweepTemporary(root, { olderThanMs = 6 * 3_600_000, notAfterMs = Infinity, now = Date.now(), maxEntries = 2000 } = {}) {
  const cutoff = Math.min(now - olderThanMs, notAfterMs);
  let names;
  try { names = await readdir(root); } catch { return { removed: 0, skipped: 0, scanned: 0 }; }
  let removed = 0; let skipped = 0; let scanned = 0;
  for (const name of names) {
    if (scanned >= maxEntries) break;
    if (KEEP.has(name)) continue;
    scanned += 1;
    const path = join(root, name);
    try {
      const info = await stat(path);
      if (info.mtimeMs >= cutoff) continue;
      const doomed = name.startsWith('.rk-sweep-') ? path : join(root, `.rk-sweep-${process.pid}-${now}-${removed + skipped}`);
      if (doomed !== path) await rename(path, doomed);
      await rm(doomed, { recursive: true, force: true, maxRetries: 0 });
      removed += 1;
    } catch {
      skipped += 1;
    }
  }
  return { removed, skipped, scanned };
}
