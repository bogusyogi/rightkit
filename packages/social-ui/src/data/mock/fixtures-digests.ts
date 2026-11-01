// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented research digest and its applications for the mock. Topics, findings, sources and ids are
// fictional. Sources use .example hosts, and the path:line source names no real file. The applied entry
// carries the draft calendar entry id the engine would return; that draft has a placeholder caption.
import type { DigestApplication, ResearchDigest } from "../types.js";
import { SCOPE_ALPHA } from "./fixtures-optimise.js";

export const FIXTURE_DIGESTS: ResearchDigest[] = [
  {
    id: "fixture-digest-2026-10-05",
    scopeId: SCOPE_ALPHA,
    digestDate: "2026-10-05",
    entries: [
      {
        // Applied: became one draft calendar entry.
        id: "fixture-finding-a",
        topic: "Fixture topic: bedtime routines",
        finding: "Fixture finding: a short audio cue before bed is re-watched more often than a longer intro.",
        source: "https://research.example/fixture-bedtime-study",
        fetchedOn: "2026-10-04",
        platform: "instagram",
        format: "reel",
      },
      {
        // Not applied yet: no draft calendar entry exists for it.
        id: "fixture-finding-b",
        topic: "Fixture topic: puddle physics",
        finding: "Fixture finding: a slow reveal of the puddle ripple holds attention past the first seconds.",
        source: "notes/fixture-digest.md:14",
        fetchedOn: "2026-10-04",
        platform: "youtube",
        format: "short",
      },
    ],
    createdAtUtc: "2026-10-05T07:00:00Z",
  },
];

export const FIXTURE_DIGEST_APPLICATIONS: DigestApplication[] = [
  {
    digestId: "fixture-digest-2026-10-05",
    digestEntryId: "fixture-finding-a",
    briefId: "digest_brief.fixture-digest-2026-10-05.fixture-finding-a",
    calendarEntryId: "digest_entry.fixture-digest-2026-10-05.fixture-finding-a",
    appliedAtUtc: "2026-10-05T07:30:00Z",
  },
];
