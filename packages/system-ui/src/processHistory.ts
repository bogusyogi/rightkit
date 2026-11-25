import type { ProcessRow } from "./api.js";

/**
 * One poll of one app: CPU percent, memory bytes, and disk bytes per second (null when unknown).
 * `total` is the lifetime read plus written bytes the rate is taken from; `members` names the processes
 * it covers, so a change in the member set drops the rate; `at` is when the poll was read (ms).
 */
export interface ProcessSample {
  cpu: number;
  memory: number;
  disk: number | null;
  total: number | null;
  members: string;
  at: number;
}

/** The kept samples of each app, by `processHistoryKey`. */
export type ProcessHistory = Map<string, ProcessSample[]>;

/** Samples kept per process: 120 polls, about six minutes at a 3 s poll. */
export const HISTORY_SAMPLES = 120;
/** Processes tracked at once; the oldest entries are dropped first. */
export const HISTORY_PROCESSES = 2000;
/** The longest gap between two polls over which a disk rate is still taken, in seconds. */
export const HISTORY_RATE_GAP_SECONDS = 10;

/** Identity of one process: pid plus start time, so a reused pid starts with no history. */
export function processHistoryKey(row: ProcessRow): string {
  return `${row.lead.pid}:${row.lead.start_time}`;
}

/** The processes an app row covers, by pid and start time. */
function memberSignature(row: ProcessRow): string {
  return row.members.map((member) => `${member.identity.pid}:${member.identity.start_time}`).join(",");
}

/**
 * Adds one poll to each app's history, drops apps that are gone, and caps the map.
 * The disk rate is the change in the app's lifetime total since its previous poll, over the seconds
 * between the host's own polls (`now`, in ms). It is dropped (null) when there is no previous reading,
 * the total went backwards (a pid reused or a member gone), the member set changed, the gap is over
 * `HISTORY_RATE_GAP_SECONDS`, or the row has no disk I/O.
 */
export function recordProcessHistory(history: ProcessHistory, rows: ProcessRow[], now: number): void {
  const live = new Set<string>();
  for (const row of rows) {
    const id = processHistoryKey(row);
    live.add(id);
    const samples = history.get(id) ?? [];
    const io = row.disk_io;
    const total = io ? io.total_read_bytes + io.total_written_bytes : null;
    const members = memberSignature(row);
    const previous = samples[samples.length - 1];
    const seconds = previous ? (now - previous.at) / 1000 : 0;
    // Narrowed inline: the rate needs the previous total and this one, and both must agree on the members.
    const disk =
      previous &&
      previous.total != null &&
      total != null &&
      previous.members === members &&
      total >= previous.total &&
      seconds > 0 &&
      seconds <= HISTORY_RATE_GAP_SECONDS
        ? (total - previous.total) / seconds
        : null;
    samples.push({ cpu: row.cpu_usage_percent, memory: row.memory_bytes, disk, total, members, at: now });
    if (samples.length > HISTORY_SAMPLES) samples.splice(0, samples.length - HISTORY_SAMPLES);
    history.set(id, samples);
  }
  for (const id of [...history.keys()]) if (!live.has(id)) history.delete(id);
  for (const id of [...history.keys()]) {
    if (history.size <= HISTORY_PROCESSES) break;
    history.delete(id);
  }
}
