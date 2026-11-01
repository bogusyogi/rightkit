// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented publish pacing for the first two channels. The job ids, entry ids, remote account ids and details are
// fictional, and no job here reached a platform. The third channel has one job that may run. The second channel
// has nothing queued. One row is deliberately unreadable, so the screen shows that a row is never dropped.
import type { PacingBlockedJob, PacingJob, PublishNextView, PublishQuota, UnreadableRow } from "../types";
import { MOCK_NOW_UTC } from "./fixtures";
import { SCOPE_ALPHA, SCOPE_BETA, SCOPE_GAMMA } from "./fixtures-optimise";

const DAY_RESETS_UTC = "2026-10-13T00:00:00Z";
const DEFAULT_MIN_GAP_SECS = 900;
const DEFAULT_MAX_PER_DAY = 10;

function job(input: Omit<PacingJob, "readable">): PacingJob {
  return { readable: true, ...input };
}

function blocked(input: Omit<PacingBlockedJob, "readable">): PacingBlockedJob {
  return { readable: true, ...input };
}

const ALPHA_READY: PacingJob[] = [
  job({
    jobId: "pn-fixture-0101",
    scopeId: SCOPE_ALPHA,
    platform: "instagram",
    remoteAccount: "fixture-ig-1",
    entryId: "fixture-entry-0401",
    state: "queued",
    phase: "create",
    scheduledAtUtc: "2026-10-12T18:00:00Z",
  }),
];

const ALPHA_BLOCKED: (PacingBlockedJob | UnreadableRow)[] = [
  blocked({
    jobId: "pn-fixture-0102",
    scopeId: SCOPE_ALPHA,
    platform: "instagram",
    remoteAccount: "fixture-ig-2",
    entryId: "fixture-entry-0402",
    state: "queued",
    phase: "create",
    scheduledAtUtc: "2026-10-12T19:00:00Z",
    reason: "not_due",
    reasons: ["not_due"],
    untilUtc: "2026-10-12T19:00:00Z",
    detail: "The scheduled time is 19:00:00 UTC.",
    behindJobId: null,
  }),
  blocked({
    jobId: "pn-fixture-0103",
    scopeId: SCOPE_ALPHA,
    platform: "instagram",
    remoteAccount: "fixture-ig-1",
    entryId: "fixture-entry-0403",
    state: "queued",
    phase: "create",
    scheduledAtUtc: "2026-10-12T18:10:00Z",
    reason: "account_gap",
    reasons: ["account_gap"],
    untilUtc: "2026-10-12T18:50:00Z",
    detail: "The last create on this account was at 17:00:00 UTC. The 1200-second gap ends at 18:50:00 UTC.",
    behindJobId: "pn-fixture-0101",
  }),
  blocked({
    jobId: "pn-fixture-0104",
    scopeId: SCOPE_ALPHA,
    platform: "facebook",
    remoteAccount: "fixture-fb-1",
    entryId: "fixture-entry-0404",
    state: "waiting",
    phase: "create",
    scheduledAtUtc: "2026-10-12T19:30:00Z",
    reason: "rate_limited_until",
    reasons: ["rate_limited_until", "not_due"],
    untilUtc: "2026-10-12T19:30:00Z",
    detail: "The platform asked us to wait until 19:30:00 UTC.",
    behindJobId: null,
  }),
  blocked({
    jobId: "pn-fixture-0105",
    scopeId: SCOPE_ALPHA,
    platform: "youtube",
    remoteAccount: "fixture-yt-1",
    entryId: "fixture-entry-0405",
    state: "queued",
    phase: "create",
    scheduledAtUtc: "2026-10-12T18:05:00Z",
    reason: "account_paused",
    reasons: ["account_paused"],
    untilUtc: "2026-10-14T00:00:00Z",
    detail: "A person paused this account until 2026-10-14 00:00:00 UTC.",
    behindJobId: null,
  }),
  blocked({
    jobId: "pn-fixture-0106",
    scopeId: SCOPE_ALPHA,
    platform: "instagram",
    remoteAccount: "fixture-ig-3",
    entryId: "fixture-entry-0406",
    state: "queued",
    phase: "create",
    scheduledAtUtc: "2026-10-12T17:00:00Z",
    reason: "outcome_unknown_on_account",
    reasons: ["outcome_unknown_on_account"],
    untilUtc: null,
    detail: "Another job on this account has an unknown outcome. A person must resolve it on the server.",
    behindJobId: null,
  }),
  blocked({
    jobId: "pn-fixture-0107",
    scopeId: SCOPE_ALPHA,
    platform: "linkedin",
    remoteAccount: "fixture-li-1",
    entryId: "fixture-entry-0407",
    state: "queued",
    phase: "create",
    scheduledAtUtc: "2026-10-12T17:30:00Z",
    reason: "account_not_ready",
    reasons: ["account_not_ready"],
    untilUtc: null,
    detail: "The accounts store says this account cannot publish: its token has expired.",
    behindJobId: null,
  }),
  blocked({
    jobId: "pn-fixture-0108",
    scopeId: SCOPE_ALPHA,
    platform: "threads",
    remoteAccount: "fixture-th-1",
    entryId: "fixture-entry-0408",
    state: "queued",
    phase: "create",
    scheduledAtUtc: "2026-10-12T16:00:00Z",
    reason: "daily_cap",
    reasons: ["daily_cap"],
    untilUtc: DAY_RESETS_UTC,
    detail: "The daily cap for this account is reached. It resets at the next UTC midnight.",
    behindJobId: null,
  }),
  // A state the desktop does not show: kept, unread, with its raw JSON.
  {
    readable: false,
    reason: 'publish state "paused_by_test" is not shown by the desktop yet',
    rawJson: JSON.stringify({ job_id: "pn-fixture-0109", state: "paused_by_test" }, null, 2),
  },
];

const ALPHA_IN_PROGRESS: PacingJob[] = [
  job({
    jobId: "pn-fixture-0110",
    scopeId: SCOPE_ALPHA,
    platform: "instagram",
    remoteAccount: "fixture-ig-1",
    entryId: "fixture-entry-0410",
    state: "running",
    phase: "poll",
    scheduledAtUtc: "2026-10-12T18:05:00Z",
  }),
];

const VIEWS = new Map<string, PublishNextView>([
  [
    SCOPE_ALPHA,
    { nowUtc: MOCK_NOW_UTC, ready: ALPHA_READY, readyTotal: ALPHA_READY.length, blocked: ALPHA_BLOCKED, inProgress: ALPHA_IN_PROGRESS },
  ],
  [SCOPE_BETA, { nowUtc: MOCK_NOW_UTC, ready: [], readyTotal: 0, blocked: [], inProgress: [] }],
  [
    SCOPE_GAMMA,
    {
      nowUtc: MOCK_NOW_UTC,
      ready: [
        job({
          jobId: "pn-fixture-0201",
          scopeId: SCOPE_GAMMA,
          platform: "bluesky",
          remoteAccount: "fixture-bsky-1",
          entryId: "fixture-entry-0501",
          state: "queued",
          phase: "create",
          scheduledAtUtc: "2026-10-12T18:00:00Z",
        }),
      ],
      readyTotal: 1,
      blocked: [],
      inProgress: [],
    },
  ],
]);

/** The pacing view of one channel. A channel with nothing queued has an empty view. */
export function pacingViewFor(scopeId: string): PublishNextView {
  return VIEWS.get(scopeId) ?? { nowUtc: MOCK_NOW_UTC, ready: [], readyTotal: 0, blocked: [], inProgress: [] };
}

/** Pacing of one account as stored. An account with no stored limits has the host app defaults. */
const QUOTAS: PublishQuota[] = [
  {
    scopeId: SCOPE_ALPHA,
    platform: "instagram",
    remoteAccount: "fixture-ig-1",
    optedIn: true,
    minGapSecs: 1200,
    maxPerDay: 6,
    defaultMinGapSecs: DEFAULT_MIN_GAP_SECS,
    defaultMaxPerDay: DEFAULT_MAX_PER_DAY,
    pausedUntilUtc: null,
    lastCreateAtUtc: "2026-10-12T17:00:00Z",
    createsToday: 4,
    dayResetsAtUtc: DAY_RESETS_UTC,
    rateLimitedAtUtc: null,
    rateLimitedUntilUtc: null,
  },
  {
    scopeId: SCOPE_ALPHA,
    platform: "instagram",
    remoteAccount: "fixture-ig-2",
    optedIn: false,
    minGapSecs: DEFAULT_MIN_GAP_SECS,
    maxPerDay: DEFAULT_MAX_PER_DAY,
    defaultMinGapSecs: DEFAULT_MIN_GAP_SECS,
    defaultMaxPerDay: DEFAULT_MAX_PER_DAY,
    pausedUntilUtc: null,
    lastCreateAtUtc: "2026-10-11T20:00:00Z",
    createsToday: 0,
    dayResetsAtUtc: DAY_RESETS_UTC,
    rateLimitedAtUtc: null,
    rateLimitedUntilUtc: null,
  },
  {
    scopeId: SCOPE_ALPHA,
    platform: "youtube",
    remoteAccount: "fixture-yt-1",
    optedIn: true,
    minGapSecs: DEFAULT_MIN_GAP_SECS,
    maxPerDay: DEFAULT_MAX_PER_DAY,
    defaultMinGapSecs: DEFAULT_MIN_GAP_SECS,
    defaultMaxPerDay: DEFAULT_MAX_PER_DAY,
    pausedUntilUtc: "2026-10-14T00:00:00Z",
    lastCreateAtUtc: null,
    createsToday: 0,
    dayResetsAtUtc: DAY_RESETS_UTC,
    rateLimitedAtUtc: null,
    rateLimitedUntilUtc: null,
  },
  {
    scopeId: SCOPE_ALPHA,
    platform: "facebook",
    remoteAccount: "fixture-fb-1",
    optedIn: false,
    minGapSecs: DEFAULT_MIN_GAP_SECS,
    maxPerDay: DEFAULT_MAX_PER_DAY,
    defaultMinGapSecs: DEFAULT_MIN_GAP_SECS,
    defaultMaxPerDay: DEFAULT_MAX_PER_DAY,
    pausedUntilUtc: null,
    lastCreateAtUtc: "2026-10-12T16:35:00Z",
    createsToday: 2,
    dayResetsAtUtc: DAY_RESETS_UTC,
    rateLimitedAtUtc: "2026-10-12T16:40:00Z",
    rateLimitedUntilUtc: "2026-10-12T19:30:00Z",
  },
  {
    scopeId: SCOPE_ALPHA,
    platform: "threads",
    remoteAccount: "fixture-th-1",
    optedIn: true,
    minGapSecs: DEFAULT_MIN_GAP_SECS,
    maxPerDay: DEFAULT_MAX_PER_DAY,
    defaultMinGapSecs: DEFAULT_MIN_GAP_SECS,
    defaultMaxPerDay: DEFAULT_MAX_PER_DAY,
    pausedUntilUtc: null,
    lastCreateAtUtc: "2026-10-12T15:00:00Z",
    createsToday: 10,
    dayResetsAtUtc: DAY_RESETS_UTC,
    rateLimitedAtUtc: null,
    rateLimitedUntilUtc: null,
  },
];

/** The pacing of one account. An account with no stored row reads as the defaults, not opted in. */
export function quotaFor(scopeId: string, platform: string, remoteAccount: string): PublishQuota {
  const stored = QUOTAS.find((q) => q.scopeId === scopeId && q.platform === platform && q.remoteAccount === remoteAccount);
  if (stored) return stored;
  return {
    scopeId,
    platform,
    remoteAccount,
    optedIn: false,
    minGapSecs: DEFAULT_MIN_GAP_SECS,
    maxPerDay: DEFAULT_MAX_PER_DAY,
    defaultMinGapSecs: DEFAULT_MIN_GAP_SECS,
    defaultMaxPerDay: DEFAULT_MAX_PER_DAY,
    pausedUntilUtc: null,
    lastCreateAtUtc: null,
    createsToday: 0,
    dayResetsAtUtc: DAY_RESETS_UTC,
    rateLimitedAtUtc: null,
    rateLimitedUntilUtc: null,
  };
}
