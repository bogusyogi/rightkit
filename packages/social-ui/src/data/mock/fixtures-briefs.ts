// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented outgoing briefs, deliveries and dispatch attempts for the mock. Ids, hashes, paths and messages
// are fictional. The envelope body is illustrative: the desktop shows whatever the engine stored.
import type { DeliveryEnvelope, DispatchAttempt, JsonValue, OutgoingBriefDetail } from "../types";
import { SCOPE_ALPHA } from "./fixtures-optimise";

const CHANNEL: JsonValue = { venture: "fixture-alpha", channel: "alpha-shorts", account: "alpha-main" };
const INPUT_HASH = `blake3:${"5".repeat(64)}`;

export const FIXTURE_BRIEFS: OutgoingBriefDetail[] = [
  {
    // Issued, not yet delivered. Its first attempt found no engine, and its second timed out.
    scopeId: SCOPE_ALPHA,
    requestId: "fixture-req-0001",
    briefId: "brief.fixture-0001",
    revision: 1,
    state: "issued",
    createdAtUtc: "2026-10-08T09:00:00Z",
    updatedAtUtc: "2026-10-08T09:00:00Z",
    summary: {
      briefType: "talking_head",
      platforms: ["youtube_shorts"],
      deliverables: ["video"],
      hook: "Fixture hook: the puddle that never dries.",
      reviewMode: "manual",
      policyRevision: 3,
      stageCount: 1,
    },
    document: {
      contract: "content-brief",
      contractVersion: "1.1.0",
      briefId: "brief.fixture-0001",
      revision: 1,
      briefType: "talking_head",
      channel: CHANNEL,
      platforms: ["youtube_shorts"],
      deliverables: ["video"],
      hook: "Fixture hook: the puddle that never dries.",
      review: { mode: "manual", policyRevision: 3 },
      stages: [{ id: "script", requestId: "req.fixture-0001.script", inputHash: INPUT_HASH, dependsOn: [] }],
    },
  },
  {
    // Delivered: one envelope is stored for it, and its attempt succeeded.
    scopeId: SCOPE_ALPHA,
    requestId: "fixture-req-0002",
    briefId: "brief.fixture-0002",
    revision: 1,
    state: "delivered",
    createdAtUtc: "2026-10-09T09:45:00Z",
    updatedAtUtc: "2026-10-09T10:15:00Z",
    summary: {
      briefType: "talking_head",
      platforms: ["youtube_shorts"],
      deliverables: ["video"],
      hook: "Fixture hook: the slow reveal.",
      reviewMode: "manual",
      policyRevision: 3,
      stageCount: 1,
    },
    document: {
      contract: "content-brief",
      contractVersion: "1.1.0",
      briefId: "brief.fixture-0002",
      revision: 1,
      briefType: "talking_head",
      channel: CHANNEL,
      platforms: ["youtube_shorts"],
      deliverables: ["video"],
      hook: "Fixture hook: the slow reveal.",
      review: { mode: "manual", policyRevision: 3 },
      stages: [{ id: "script", requestId: "req.fixture-0002.script", inputHash: INPUT_HASH, dependsOn: [] }],
    },
  },
];

/** A fixture brief document for the two multi-deliverable briefs below. */
function twoDeliverableBrief(
  requestId: string,
  briefId: string,
  state: "partial" | "delivered",
  hook: string,
  createdAtUtc: string,
  updatedAtUtc: string,
): OutgoingBriefDetail {
  return {
    scopeId: SCOPE_ALPHA,
    requestId,
    briefId,
    revision: 1,
    state,
    createdAtUtc,
    updatedAtUtc,
    summary: {
      briefType: "talking_head",
      platforms: ["youtube_shorts"],
      deliverables: ["reels", "square"],
      hook,
      reviewMode: "manual",
      policyRevision: 3,
      stageCount: 1,
    },
    document: {
      contract: "content-brief",
      contractVersion: "1.1.0",
      briefId,
      revision: 1,
      briefType: "talking_head",
      channel: CHANNEL,
      platforms: ["youtube_shorts"],
      deliverables: ["reels", "square"],
      hook,
      review: { mode: "manual", policyRevision: 3 },
      stages: [{ id: "script", requestId: `req.${requestId}.script`, inputHash: INPUT_HASH, dependsOn: [] }],
    },
  };
}

/** A fixture video envelope for one deliverable of a two-deliverable brief. */
function deliverableEnvelope(requestId: string, briefId: string, deliverable: string, acceptedAtUtc: string): DeliveryEnvelope {
  const envelopeId = `fixture-envelope-${requestId}-${deliverable}`;
  return {
    scopeId: SCOPE_ALPHA,
    envelopeId,
    requestId,
    deliverable,
    briefId,
    briefRevision: 1,
    acceptedAtUtc,
    document: {
      contract: "delivery-envelope",
      contractVersion: "1.1.0",
      envelopeId,
      briefId,
      briefRevision: 1,
      media: [{ hash: `blake3:${(deliverable === "reels" ? "7" : "8").repeat(64)}` }],
    },
  };
}

// Two more briefs: one delivered with an envelope for each of two deliverables, and one partial because the
// engine refused the second deliverable as a paid route.
FIXTURE_BRIEFS.push(
  twoDeliverableBrief(
    "fixture-req-0003",
    "brief.fixture-0003",
    "partial",
    "Fixture hook: the half-finished wave.",
    "2026-10-10T09:00:00Z",
    "2026-10-10T09:30:00Z",
  ),
  twoDeliverableBrief(
    "fixture-req-0004",
    "brief.fixture-0004",
    "delivered",
    "Fixture hook: both sizes at once.",
    "2026-10-11T09:00:00Z",
    "2026-10-11T09:20:00Z",
  ),
);

export const FIXTURE_DELIVERIES: DeliveryEnvelope[] = [
  deliverableEnvelope("fixture-req-0003", "brief.fixture-0003", "reels", "2026-10-10T09:30:00Z"),
  deliverableEnvelope("fixture-req-0004", "brief.fixture-0004", "reels", "2026-10-11T09:20:00Z"),
  deliverableEnvelope("fixture-req-0004", "brief.fixture-0004", "square", "2026-10-11T09:20:00Z"),
  {
    scopeId: SCOPE_ALPHA,
    envelopeId: "fixture-envelope-0002",
    requestId: "fixture-req-0002",
    deliverable: "",
    briefId: "brief.fixture-0002",
    briefRevision: 1,
    acceptedAtUtc: "2026-10-09T10:15:00Z",
    document: {
      contract: "delivery-envelope",
      contractVersion: "1.1.0",
      envelopeId: "fixture-envelope-0002",
      briefId: "brief.fixture-0002",
      briefRevision: 1,
      media: [{ hash: `blake3:${"6".repeat(64)}` }],
    },
  },
];

export const FIXTURE_DISPATCH_ATTEMPTS: DispatchAttempt[] = [
  {
    // The engine path was not set, so nothing was spawned.
    scopeId: SCOPE_ALPHA,
    attemptId: 1,
    requestId: "fixture-req-0001",
    engine: "cutright",
    startedAtUtc: "2026-10-08T09:05:00Z",
    finishedAtUtc: "2026-10-08T09:05:00Z",
    exitCode: null,
    outcome: "engine_missing",
    stderrTail: null,
    message: "Fixture: the engine path is not set, so nothing ran.",
    producedDeliverables: [],
    skipped: [],
  },
  {
    // The wall clock ran out and the process tree was killed, so there is no exit code.
    scopeId: SCOPE_ALPHA,
    attemptId: 2,
    requestId: "fixture-req-0001",
    engine: "cutright",
    startedAtUtc: "2026-10-08T09:10:00Z",
    finishedAtUtc: "2026-10-08T09:20:00Z",
    exitCode: null,
    outcome: "timed_out",
    stderrTail: "Fixture stderr: the render step was still running when the limit was reached.",
    message: "Fixture: the engine did not finish within 600000 ms. The process tree was ended.",
    producedDeliverables: [],
    skipped: [],
  },
  {
    scopeId: SCOPE_ALPHA,
    attemptId: 3,
    requestId: "fixture-req-0002",
    engine: "cutright",
    startedAtUtc: "2026-10-09T10:00:00Z",
    finishedAtUtc: "2026-10-09T10:04:30Z",
    exitCode: 0,
    outcome: "delivered",
    stderrTail: "",
    message: "Fixture: one envelope was named and accepted.",
    producedDeliverables: [],
    skipped: [],
  },
  {
    // Partial: the engine delivered reels and refused the paid-route square deliverable.
    scopeId: SCOPE_ALPHA,
    attemptId: 4,
    requestId: "fixture-req-0003",
    engine: "genright",
    startedAtUtc: "2026-10-10T09:10:00Z",
    finishedAtUtc: "2026-10-10T09:30:00Z",
    exitCode: 0,
    outcome: "partial",
    stderrTail: "",
    message: "Fixture: 1 envelope(s) accepted; the engine skipped or refused part of the request.",
    producedDeliverables: ["reels"],
    skipped: [
      {
        deliverable: "square",
        code: "spend_authorisation_required",
        message: "Fixture: this deliverable uses a paid route and no spend was authorised.",
      },
    ],
  },
  {
    scopeId: SCOPE_ALPHA,
    attemptId: 5,
    requestId: "fixture-req-0004",
    engine: "genright",
    startedAtUtc: "2026-10-11T09:05:00Z",
    finishedAtUtc: "2026-10-11T09:20:00Z",
    exitCode: 0,
    outcome: "delivered",
    stderrTail: "",
    message: "Fixture: 2 envelope(s) accepted; the engine produced everything requested.",
    producedDeliverables: ["reels", "square"],
    skipped: [],
  },
];
