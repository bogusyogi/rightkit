import { lstatSync, readdirSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { pathSize, platformKey, scratchOwner, scratchRoots } from "./disk-policy.mjs";

// The daily `disk.report` line of disk-hygiene.jsonl. Pulse's Storage card reads it: the field names below are a contract
// (frozen in disk-report.test.mjs). Pure apart from `measureScratchUnits`.

const UNIT_LIMIT = 50;
const GROWER_LIMIT = 10;

export { scratchOwner };

/** True when the last report is from another UTC day than `now` (or there is none). */
export function reportDue({ lastReportTs, now }) {
  const last = Date.parse(lastReportTs ?? "");
  if (!Number.isFinite(last)) return true;
  return new Date(last).toISOString().slice(0, 10) !== new Date(now).toISOString().slice(0, 10);
}

/** One du per child of every scratch root (daily only: the hourly pass never sizes anything). */
export function measureScratchUnits({ policy, platform = process.platform, env = process.env, home = os.homedir(), size = (p) => pathSize(p, { platform }) } = {}) {
  const units = [];
  for (const root of scratchRoots(policy, { platform, env, home })) {
    let names = [];
    try { names = readdirSync(root); } catch { continue; }
    for (const name of names) {
      const full = path.join(root, name);
      try { if (lstatSync(full).isSymbolicLink()) continue; } catch { continue; }
      units.push({ name, path: full, bytes: size(full) });
    }
  }
  return units;
}

const round = (n, places = 2) => Math.round(n * 10 ** places) / 10 ** places;

// bytes per category: the outermost writers on the agent volume (a writer nested in another counts once) plus what no writer
// holds. The identity is  sum(categories) - overlapBytes = usedBytes: du sums allocated bytes per writer, so APFS clones and hard links
// can make the writers add up to MORE than the volume's used bytes. Then `overlapBytes` = writers - used (the double-counted bytes),
// `unmanaged` is 0 and no category is scaled; otherwise `overlapBytes` is 0 and `unmanaged` = used - writers. The change in used
// bytes between two reports is therefore  sum(delta.categories) - delta.overlapBytes.
function categoriesOf(check, agentUnmanaged, usedBytes) {
  const categories = {};
  const add = (name, bytes) => { categories[name] = (categories[name] ?? 0) + bytes; };
  for (const w of agentUnmanaged ? agentUnmanaged.writers ?? [] : check.writers ?? []) add(w.category ?? w.id, w.bytes ?? 0);
  const writerBytes = Object.values(categories).reduce((a, b) => a + b, 0);
  categories.unmanaged = Math.max(0, usedBytes - writerBytes);
  return { categories, overlapBytes: Math.max(0, writerBytes - usedBytes) };
}

const sumDelta = (now, before) => {
  const out = {};
  for (const name of new Set([...Object.keys(now), ...Object.keys(before)])) out[name] = (now[name] ?? 0) - (before[name] ?? 0);
  return out;
};

/** The report line for a finished full pass. `check` is the `checkDisk` result after the pass, `previous` the last
 * `disk.report` event (or null), `scratchUnits` the result of `measureScratchUnits`. */
export function buildDiskReport({ check, previous = null, now, policy, scratchUnits = [], moved = 0, purged = 0, host = os.hostname() }) {
  const floors = check.floors ?? [check.floor];
  // The report describes the agent volume. When one is flagged and cannot be read there is no lead: never another volume's numbers
  // under its name (a day of internal-drive bytes next to a day of D: bytes makes a delta of terabytes). Policies that flag no
  // agent volume at all report their first readable floor.
  const lead = floors.some((f) => f.agentVolume) ? floors.find((f) => f.agentVolume && !f.absent) ?? null : floors.find((f) => !f.absent) ?? null;
  const agentUnmanaged = lead ? (check.unmanaged ?? []).find((u) => u.floor === (lead.id ?? lead.label)) ?? (check.unmanaged ?? [])[0] ?? null : null;
  const usedBytes = lead ? lead.totalBytes - lead.freeBytes : null;
  const { categories, overlapBytes } = lead ? categoriesOf(check, agentUnmanaged, usedBytes) : { categories: null, overlapBytes: null };

  const units = {};
  const owners = {};
  for (const w of check.writers ?? []) if (w.bytes > 0) units[w.id] = (units[w.id] ?? 0) + w.bytes;
  const scratchByOwner = {};
  for (const u of scratchUnits) {
    units[`scratch/${u.name}`] = u.bytes;
    owners[`scratch/${u.name}`] = scratchOwner(u.name);
    scratchByOwner[scratchOwner(u.name)] = (scratchByOwner[scratchOwner(u.name)] ?? 0) + u.bytes;
  }
  const kept = Object.entries(units).sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])).slice(0, UNIT_LIMIT);
  const compact = Object.fromEntries(kept);

  let delta = null;
  let growers = [];
  const hours = previous ? Math.max(0, (now - Date.parse(previous.ts)) / 3600_000) : 0;
  // No delta unless both reports describe the agent volume: a report without one (null sizes) is no baseline, and neither is this one.
  const comparable = lead && previous && Number.isFinite(previous.usedBytes) && previous.categories && typeof previous.categories === "object";
  if (comparable && Number.isFinite(hours)) {
    delta = { hours: round(hours), usedBytes: usedBytes - previous.usedBytes, categories: sumDelta(categories, previous.categories), overlapBytes: overlapBytes - (previous.overlapBytes ?? 0) };
    growers = Object.entries(compact)
      .map(([id, bytes]) => ({ id, bytes, deltaBytes: bytes - (previous.units?.[id] ?? 0), owner: owners[id] ?? null }))
      .filter((g) => g.deltaBytes > 0)
      .sort((a, b) => b.deltaBytes - a.deltaBytes || a.id.localeCompare(b.id))
      .slice(0, GROWER_LIMIT);
  }

  const budgetsOver = (check.writers ?? []).filter((w) => w.over).map((w) => ({ id: w.id, bytes: w.bytes, capBytes: w.capBytes }));
  for (const [prefix, capBytes] of Object.entries(policy.scratchPrefixCaps ?? {})) {
    if ((scratchByOwner[prefix] ?? 0) > capBytes) budgetsOver.push({ id: `scratch:${prefix}`, bytes: scratchByOwner[prefix], capBytes });
  }

  // What the data in flight occupies at steady state: a day's growth lives as scratch, then in holding, then the daily pass
  // that finally purges it.
  const growthBytesPerDay = delta ? Math.round((Math.max(0, delta.usedBytes) / Math.max(delta.hours, 1)) * 24) : 0;
  const daysParked = (policy.scratchStaleDays ?? policy.unknownStaleDays ?? 3) + (policy.holdingDays ?? 3) + 1;

  return {
    ts: new Date(now).toISOString(),
    event: "disk.report",
    schema: 1,
    platform: check.platform ?? platformKey(),
    host,
    freeBytes: lead?.freeBytes ?? null,
    usedBytes,
    totalBytes: lead?.totalBytes ?? null,
    floors: floors.map((f) => ({ id: f.id ?? f.label, freeBytes: f.absent ? null : f.freeBytes, softBytes: f.absent ? null : f.thresholdBytes, hardBytes: f.hardFreeBytes ?? null, warn: f.warn })),
    categories,
    delta,
    units: compact,
    growers,
    budgetsOver,
    unmanagedBytes: categories ? categories.unmanaged : null,
    overlapBytes,
    uncoveredTopLevel: (agentUnmanaged?.uncoveredTopLevel ?? []).map((e) => e.name),
    scratchByOwner,
    steadyState: { growthBytesPerDay, daysParked, estimateBytes: growthBytesPerDay * daysParked },
    movedBytes: moved,
    purgedBytes: purged,
  };
}
