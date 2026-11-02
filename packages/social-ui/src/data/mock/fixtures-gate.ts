// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented gate policy and stored verdict records for the mock. Phrases, hashes and ids are fictional.
// The thresholds are invented for the mock; they are not engine defaults, because the engine has none.
import type { GatePolicy, GateRecord } from "../types.js";
import { SCOPE_ALPHA } from "./fixtures-optimise.js";

export const FIXTURE_GATE_POLICIES: GatePolicy[] = [
  {
    // The first channel has a policy at revision 3. The second and third channels have none, so the screen
    // shows that nothing can pass there.
    scopeId: SCOPE_ALPHA,
    revision: 3,
    createdAtUtc: "2026-09-15T08:00:00Z",
    bannedPhrases: ["guarantee"],
    stockOpeners: ["Here's", "In this"],
    minWords: 40,
    dashMaxPer100WordsMilli: 1500,
    semicolonMaxPer100WordsMilli: 500,
    varianceMinMilli: 4000,
    varianceMinSentences: 5,
    tricolonMaxPer100SentencesMilli: 20000,
    tricolonMinSentences: 4,
    required: {
      bannedPhrase: true,
      dashDensity: true,
      semicolonDensity: true,
      sentenceLengthVariance: true,
      tricolonFrequency: true,
      stockOpener: true,
    },
  },
];

export const FIXTURE_GATE_RECORDS: GateRecord[] = [
  {
    // An earlier check, under revision 2: blocked. The record stays as it was written.
    scopeId: SCOPE_ALPHA,
    recordSeq: 1,
    subjectId: "fixture-script-hook-01",
    textHash: "fnv1a128:9f1c0a7e4b2d3c5f8a6e1b0d4c2f7a93",
    policyRevision: 2,
    disposition: "block",
    checkedAtUtc: "2026-10-06T09:00:00Z",
  },
  {
    // A later recorded check, under revision 3: passed.
    scopeId: SCOPE_ALPHA,
    recordSeq: 2,
    subjectId: "fixture-script-hook-01",
    textHash: "fnv1a128:2b7d4e9f0a1c6b3d8e5f2a7c9b0d4e61",
    policyRevision: 3,
    disposition: "pass",
    checkedAtUtc: "2026-10-07T09:30:00Z",
  },
];
