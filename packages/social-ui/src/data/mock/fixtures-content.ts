// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented content records, variants and experiment summaries for the mock. Titles, captions, hashes and
// ids are fictional. Counts are invented whole numbers. Each summary's reason follows the engine's wording
// (docs/CONTENT-RECORDS.md): one line per failed check, joined with "; ".
import type { ContentExperimentSummary, ContentRecord, ContentVariant } from "../types.js";
import { SCOPE_ALPHA } from "./fixtures-optimise.js";

export const FIXTURE_CONTENT: ContentRecord[] = [
  {
    // Active, with two caption variants and one call to action. No occurrence has reached the minimum.
    id: "fixture-clip-puddle",
    scopeId: SCOPE_ALPHA,
    title: "Fixture clip: puddle chorus",
    media: [{ hash: "blake3:0f0f0f0f0f0f0f01", kind: "video", durationMs: 42000 }],
    baseCaption: "Fixture base caption for the puddle chorus.",
    evergreen: true,
    cooldownDays: 14,
    expiresAtUtc: "2027-03-31T23:59:59Z",
    state: "active",
    withdrawalReason: null,
    revision: 4,
    createdAtUtc: "2026-09-01T09:00:00Z",
    updatedAtUtc: "2026-10-06T10:00:00Z",
  },
  {
    // Withdrawn: final. Its planned occurrences were cancelled when it was withdrawn.
    id: "fixture-clip-lighthouse",
    scopeId: SCOPE_ALPHA,
    title: "Fixture clip: lighthouse still",
    media: [{ hash: "blake3:1a2b3c4d5e6f7081", kind: "image", durationMs: null }],
    baseCaption: "Fixture base caption for the lighthouse still.",
    evergreen: false,
    cooldownDays: 7,
    expiresAtUtc: null,
    state: "withdrawn",
    withdrawalReason: "Fixture reason: the stock image licence ended.",
    revision: 3,
    createdAtUtc: "2026-08-20T09:00:00Z",
    updatedAtUtc: "2026-09-30T12:00:00Z",
  },
];

export const FIXTURE_CONTENT_VARIANTS: ContentVariant[] = [
  {
    id: "fixture-cap-a",
    scopeId: SCOPE_ALPHA,
    contentId: "fixture-clip-puddle",
    kind: "caption",
    text: "Fixture caption A: the chorus, with the puddle in view.",
    createdAtUtc: "2026-09-01T09:05:00Z",
  },
  {
    id: "fixture-cap-b",
    scopeId: SCOPE_ALPHA,
    contentId: "fixture-clip-puddle",
    kind: "caption",
    text: "Fixture caption B: the chorus, sung twice.",
    createdAtUtc: "2026-09-01T09:06:00Z",
  },
  {
    id: "fixture-cta-a",
    scopeId: SCOPE_ALPHA,
    contentId: "fixture-clip-puddle",
    kind: "cta",
    text: "Fixture call to action: watch the full episode.",
    createdAtUtc: "2026-09-01T09:07:00Z",
  },
];

export const FIXTURE_CONTENT_SUMMARIES: ContentExperimentSummary[] = [
  {
    // Insufficient evidence: both caption variants are below 20 published occurrences, and only one
    // call to action is stored. The engine names no winner, and neither does this screen.
    scopeId: SCOPE_ALPHA,
    contentId: "fixture-clip-puddle",
    minOccurrences: 20,
    verdict: "insufficient_evidence",
    reason:
      "caption variant fixture-cap-a has 3 published occurrences; the minimum is 20; caption variant fixture-cap-b has 2 published occurrences; the minimum is 20; fewer than two cta variants are stored (1); cta variant fixture-cta-a has 1 published occurrences; the minimum is 20",
    caption: [
      { variantId: "fixture-cap-a", text: "Fixture caption A: the chorus, with the puddle in view.", planned: 1, published: 3, cancelled: 0, meetsMinimum: false },
      { variantId: "fixture-cap-b", text: "Fixture caption B: the chorus, sung twice.", planned: 2, published: 2, cancelled: 1, meetsMinimum: false },
    ],
    cta: [
      { variantId: "fixture-cta-a", text: "Fixture call to action: watch the full episode.", planned: 0, published: 1, cancelled: 0, meetsMinimum: false },
    ],
  },
  {
    // The withdrawn record has no variants, so both kinds are below two variants.
    scopeId: SCOPE_ALPHA,
    contentId: "fixture-clip-lighthouse",
    minOccurrences: 20,
    verdict: "insufficient_evidence",
    reason: "fewer than two caption variants are stored (0); fewer than two cta variants are stored (0)",
    caption: [],
    cta: [],
  },
];
