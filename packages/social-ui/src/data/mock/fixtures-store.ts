// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented store health for the mock. Every digest, path and receipt is fictional. The mock clock is
// 2026-10-12 18:30 UTC. The mock computes the heartbeat's age and state from the `now` it is given, as the engine
// does, so these record holds only the beat.
import type { RestoreReceipt } from "../types.js";

export interface FixtureHeartbeat {
  pid: number;
  /** Milliseconds since the Unix epoch. */
  updatedMs: number;
  recordState: string;
}

// 2026-10-12T18:25:00Z, five minutes before the mock clock: alive against the 900-second threshold.
export const FIXTURE_HEARTBEAT: FixtureHeartbeat = { pid: 4102, updatedMs: Date.parse("2026-10-12T18:25:00Z"), recordState: "serving" };

/** One restore from an invented backup. The previous store is named as set aside, never deleted. */
export const FIXTURE_RESTORE_RECEIPTS: RestoreReceipt[] = [
  {
    receiptId: "fixture-rr-20261011-093000-0123456789ab",
    restoredAtUtc: "2026-10-11T09:30:00Z",
    manifestDigest: "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    schemaBefore: 7,
    schemaAfter: 8,
    previousStore: "/fixture/postright/store/postright.sqlite.pre-restore-1760175000000",
  },
];
