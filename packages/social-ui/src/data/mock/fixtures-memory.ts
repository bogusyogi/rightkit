// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented channel memory revisions and proofs for the mock. Every value is fictional: the brand, pillars,
// phrases, answers and proofs are made up. Content hashes are placeholders in the engine's form. Only the first
// channel has a memory document and proofs; the other channels have none.
import type { ChannelMemoryDetail, ProofRecord } from "../types.js";
import { SCOPE_ALPHA } from "./fixtures-optimise.js";

const SCOPE = { venture: "fixture-alpha", channel: "alpha-shorts", account: "alpha-main" };

export const FIXTURE_MEMORY: ChannelMemoryDetail[] = [
  {
    // First revision: no cadence and no banned moves yet, so the screen says those sections are absent.
    scopeId: SCOPE_ALPHA,
    revision: 1,
    supersedes: null,
    author: "adrian",
    createdAtUtc: "2026-09-01T09:00:00Z",
    contentHash: "fnv1a128:1f0e2d3c4b5a69788796a5b4c3d2e1f0",
    document: {
      contract: "channel-memory",
      contractVersion: "1.0.0",
      memoryId: "fixture-memory-alpha",
      scope: SCOPE,
      brandCode: "fixture-brand",
      voice: {
        tone: "plain and warm",
        audience: "fixture viewers aged 8 to 12 and their parents",
        narration: "first person, one speaker",
      },
      pillars: [
        { name: "Fixture pillar: puddle science", description: "Short experiments done outdoors." },
        { name: "Fixture pillar: bedtime routines", description: "Calm endings for the day." },
      ],
    },
  },
  {
    // Latest revision: adds cadence and banned moves, and tightens the voice.
    scopeId: SCOPE_ALPHA,
    revision: 2,
    supersedes: 1,
    author: "adrian",
    createdAtUtc: "2026-10-06T10:30:00Z",
    contentHash: "fnv1a128:9a8b7c6d5e4f30211f2e3d4c5b6a7980",
    document: {
      contract: "channel-memory",
      contractVersion: "1.0.0",
      memoryId: "fixture-memory-alpha",
      scope: SCOPE,
      brandCode: "fixture-brand",
      voice: {
        tone: "plain, warm and brief",
        audience: "fixture viewers aged 8 to 12 and their parents",
        narration: "first person, one speaker, no questions to the viewer",
        personaNotes: "Fixture note: keep every sentence under twelve words.",
      },
      pillars: [
        { name: "Fixture pillar: puddle science", description: "Short experiments done outdoors." },
        { name: "Fixture pillar: bedtime routines", description: "Calm endings for the day." },
        { name: "Fixture pillar: fixture crafts", description: "Paper and tape projects." },
      ],
      cadence: {
        postsPerDay: 1,
        timezone: "Europe/Lisbon",
        batch: "weekly",
        batchWindow: "Sunday 18:00 to 20:00",
      },
      bannedMoves: [
        {
          id: "fixture-no-guarantees",
          appliesTo: "any",
          description: "No promise of results.",
          phrases: ["guarantee", "guaranteed"],
        },
      ],
      bannedPhrases: ["guarantee", "guaranteed"],
    },
  },
];

export const FIXTURE_PROOFS: ProofRecord[] = [
  {
    // Approved: a script can cite it.
    scopeId: SCOPE_ALPHA,
    answerId: "ans.alpha-01",
    proofId: "proof.alpha.episodes-published",
    status: "approved",
    subject: "subject.episodes-published",
    value: "96",
    units: "episodes",
    origin: "interview",
    observedFrom: "2026-01-05",
    observedTo: "2026-10-01",
    supersededBy: null,
    withdrawalNote: null,
  },
  {
    // Approved, with a decimal value kept exactly as stored.
    scopeId: SCOPE_ALPHA,
    answerId: "ans.alpha-02",
    proofId: "proof.alpha.average-length",
    status: "approved",
    subject: "subject.average-length",
    value: "14.5",
    units: "minutes",
    origin: "interview",
    observedFrom: "2026-06-01",
    observedTo: "2026-09-30",
    supersededBy: null,
    withdrawalNote: null,
  },
  {
    // Collected: not approved, so no script can cite it yet.
    scopeId: SCOPE_ALPHA,
    answerId: "ans.alpha-03",
    proofId: "proof.alpha.shorts-per-week",
    status: "collected",
    subject: "subject.shorts-per-week",
    value: "3",
    units: "shorts",
    origin: "interview",
    observedFrom: "2026-10-01",
    observedTo: "",
    supersededBy: null,
    withdrawalNote: null,
  },
  {
    // Superseded: replaced by the approved proof named here.
    scopeId: SCOPE_ALPHA,
    answerId: "ans.alpha-04",
    proofId: "proof.alpha.episodes-published-v1",
    status: "superseded",
    subject: "subject.episodes-published",
    value: "91",
    units: "episodes",
    origin: "interview",
    observedFrom: "2025-12-01",
    observedTo: "2026-06-30",
    supersededBy: "proof.alpha.episodes-published",
    withdrawalNote: null,
  },
  {
    // Withdrawn: the note the engine holds is shown.
    scopeId: SCOPE_ALPHA,
    answerId: "ans.alpha-05",
    proofId: "proof.alpha.studio-hours",
    status: "withdrawn",
    subject: "subject.studio-hours",
    value: "40",
    units: "hours",
    origin: "interview",
    observedFrom: "2026-03-01",
    observedTo: "2026-03-31",
    supersededBy: null,
    withdrawalNote: "Fixture note: the answer was withdrawn by the owner.",
  },
];
