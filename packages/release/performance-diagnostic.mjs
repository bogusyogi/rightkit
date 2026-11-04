// Optional bounded measurements; never part of a gate verdict or cache key.
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';

export function writePerformanceDiagnostic(dir, category, rows) {
  let temp;
  try {
    if (!dir || !path.isAbsolute(dir) || !['preflight', 'snapshot-verify'].includes(category) || !Array.isArray(rows) || rows.length > 256) return;
    if (!rows.every((r) => r && /^[a-z0-9-]{1,80}$/.test(r.label) && Number.isFinite(r.ms) && r.ms >= 0 && Number.isInteger(r.count) && r.count >= 0)) return;
    const scratch = fs.realpathSync(process.platform === 'win32' ? 'D:\\scratch' : '/Volumes/D/scratch');
    const target = fs.realpathSync(dir);
    if (!(target === scratch || target.startsWith(scratch + path.sep)) || !fs.statSync(target).isDirectory()) return;
    const file = path.join(target, `timing-${category}-${process.pid}-${crypto.randomBytes(8).toString('hex')}.json`);
    temp = `${file}.tmp`;
    fs.writeFileSync(temp, JSON.stringify({ schema: 'rightkit.performance-diagnostic/1', category, rows: rows.map(({ label, ms, count }) => ({ label, ms, count })) }), { flag: 'wx' });
    fs.renameSync(temp, file);
  } catch { if (temp) { try { fs.unlinkSync(temp); } catch {} } }
}
