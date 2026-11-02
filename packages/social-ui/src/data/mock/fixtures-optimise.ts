// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented suggestions, allowances, reports and prices for the mock. Channel names, job ids, hosts and
// providers are fictional. Amounts are invented integers in micro-units of the fictional currency EUR.
// Qualification is set as the engine would compute it at the mock clock (MOCK_NOW_UTC, 2026-10-12).
import type { OperatorScope, OptimiseProposal, SpendAllowance, SpendPrice, SpendReport } from "../types.js";

export const SCOPE_ALPHA = "fixture-alpha/alpha-shorts/alpha-main";
export const SCOPE_BETA = "fixture-beta/beta-studio/beta-main";
export const SCOPE_GAMMA = "fixture-gamma/gamma-news/gamma-main";

export const MOCK_MONTH = "2026-10";

export const FIXTURE_SCOPES: OperatorScope[] = [
  { id: SCOPE_ALPHA, label: "fixture-alpha / alpha-shorts / alpha-main" },
  // Empty-suggestions scope: an allowance is set, but no published post has enough evidence yet.
  { id: SCOPE_BETA, label: "fixture-beta / beta-studio / beta-main" },
  // No allowance for the month, so the spend report is missing.
  { id: SCOPE_GAMMA, label: "fixture-gamma / gamma-news / gamma-main" },
];

const ALPHA_LINEAGE_JOBS = ["pub-0118", "pub-0121", "pub-0127", "pub-0133", "pub-0141"];

export const FIXTURE_PROPOSALS: OptimiseProposal[] = [
  {
    // Waiting for a decision. Due 14 days after the evaluation instant.
    id: "opt_3f9a0c1d2e4b5a60",
    scopeId: SCOPE_ALPHA,
    kind: "remake",
    state: "proposed",
    revision: 1,
    sourceJobId: "pub-0141",
    platform: "instagram",
    metric: "views",
    window: "lifetime",
    observedAtUtc: "2026-10-11T06:00:00Z",
    value: "18450",
    threshold: "16200",
    sampleSize: 30,
    decileSize: 3,
    dueAtUtc: "2026-10-25T06:00:00Z",
    lineageJobIds: ALPHA_LINEAGE_JOBS,
    lineagePullIds: ["pull-0412", "pull-0418"],
    decisionNote: null,
    decidedAtUtc: null,
    calendarEntryId: null,
    createdAtUtc: "2026-10-11T06:05:00Z",
    updatedAtUtc: "2026-10-11T06:05:00Z",
  },
  {
    // Accepted: its draft calendar entry exists and still needs a caption and approval.
    id: "opt_77b1e0a9c2d43f15",
    scopeId: SCOPE_ALPHA,
    kind: "remake",
    state: "accepted",
    revision: 2,
    sourceJobId: "pub-0127",
    platform: "youtube",
    metric: "views",
    window: "lifetime",
    observedAtUtc: "2026-10-08T06:00:00Z",
    value: "9120",
    threshold: "8800",
    sampleSize: 40,
    decileSize: 4,
    dueAtUtc: "2026-10-22T06:00:00Z",
    lineageJobIds: ["pub-0110", "pub-0115", "pub-0127"],
    lineagePullIds: ["pull-0401"],
    decisionNote: null,
    decidedAtUtc: "2026-10-10T09:30:00Z",
    calendarEntryId: "remake_entry_opt_77b1e0a9c2d43f15",
    createdAtUtc: "2026-10-08T06:10:00Z",
    updatedAtUtc: "2026-10-10T09:30:00Z",
  },
  {
    // Dismissed with a note. Creates no calendar entry.
    id: "opt_c04d82e6f1a7b390",
    scopeId: SCOPE_ALPHA,
    kind: "remake",
    state: "dismissed",
    revision: 2,
    sourceJobId: "pub-0102",
    platform: "instagram",
    metric: "saves",
    window: "lifetime",
    observedAtUtc: "2026-10-05T06:00:00Z",
    value: "310",
    threshold: "280",
    sampleSize: 20,
    decileSize: 2,
    dueAtUtc: "2026-10-19T06:00:00Z",
    lineageJobIds: ["pub-0098", "pub-0102"],
    lineagePullIds: ["pull-0377"],
    decisionNote: "Same idea as a post last week.",
    decidedAtUtc: "2026-10-09T16:10:00Z",
    calendarEntryId: null,
    createdAtUtc: "2026-10-05T06:12:00Z",
    updatedAtUtc: "2026-10-09T16:10:00Z",
  },
];

export interface FixtureAllowance {
  scopeId: string;
  allowance: SpendAllowance;
}

// Limit, reserved, committed and remaining are consistent: reserved equals the one open estimate, and
// committed equals the report's measured actuals.
export const FIXTURE_ALLOWANCES: FixtureAllowance[] = [
  {
    scopeId: SCOPE_ALPHA,
    allowance: {
      month: MOCK_MONTH,
      currency: "EUR",
      limitMicro: 50_000_000,
      reservedMicro: 3_000_000,
      committedMicro: 12_400_000,
      remainingMicro: 34_600_000,
      revision: 4,
      updatedAtUtc: "2026-10-12T14:00:00Z",
    },
  },
  {
    scopeId: SCOPE_BETA,
    allowance: {
      month: MOCK_MONTH,
      currency: "EUR",
      limitMicro: 5_000_000,
      reservedMicro: 0,
      committedMicro: 0,
      remainingMicro: 5_000_000,
      revision: 1,
      updatedAtUtc: "2026-10-01T08:00:00Z",
    },
  },
];

export interface FixtureReport {
  scopeId: string;
  report: SpendReport;
}

export const FIXTURE_REPORTS: FixtureReport[] = [
  {
    scopeId: SCOPE_ALPHA,
    report: {
      month: MOCK_MONTH,
      currency: "EUR",
      limitMicro: 50_000_000,
      reservedMicro: 3_000_000,
      committedMicro: 12_400_000,
      remainingMicro: 34_600_000,
      reconciles: true,
      routes: [
        {
          routeId: "example.text.model-a.input",
          countReserved: 1,
          countCommitted: 2,
          countReleased: 0,
          countExpired: 0,
          openMicro: 3_000_000,
          forecastMicro: 12_000_000,
          measuredMicro: 12_400_000,
          varianceMicro: 400_000,
          overReservations: 0,
        },
        {
          routeId: "example.image.render",
          countReserved: 0,
          countCommitted: 0,
          countReleased: 1,
          countExpired: 1,
          openMicro: 0,
          forecastMicro: 0,
          measuredMicro: 0,
          varianceMicro: 0,
          overReservations: 0,
        },
      ],
    },
  },
  {
    scopeId: SCOPE_BETA,
    report: {
      month: MOCK_MONTH,
      currency: "EUR",
      limitMicro: 5_000_000,
      reservedMicro: 0,
      committedMicro: 0,
      remainingMicro: 5_000_000,
      reconciles: true,
      routes: [],
    },
  },
];

// Latest version of each route. The first is qualified. The second has no verifier, so it is unqualified
// ("unverified"). The third was fetched more than 60 days before the mock clock, so it is unqualified.
export const FIXTURE_PRICES: SpendPrice[] = [
  {
    routeId: "example.text.model-a.input",
    version: 2,
    provider: "Example Model Co",
    modelSlug: "model-a",
    unit: "per_1M_input",
    amountMicro: 2_500_000,
    currency: "EUR",
    sourceUrl: "https://pricing.example/mock/model-a",
    sourcePathLine: null,
    fetchedAtUtc: "2026-10-10T09:00:00Z",
    verifiedBy: "fixture reviewer",
    qualified: true,
    reason: null,
  },
  {
    routeId: "example.image.render",
    version: 1,
    provider: "Example Render Co",
    modelSlug: "render-b",
    unit: "per_asset",
    amountMicro: 40_000,
    currency: "EUR",
    sourceUrl: null,
    sourcePathLine: "fixtures/prices.toml:12",
    fetchedAtUtc: "2026-10-09T09:00:00Z",
    verifiedBy: null,
    qualified: false,
    reason: "unverified: no verified_by",
  },
  {
    routeId: "example.video.audio",
    version: 1,
    provider: "Example Audio Co",
    modelSlug: "audio-c",
    unit: "per_minute_audio",
    amountMicro: 600_000,
    currency: "EUR",
    sourceUrl: "https://pricing.example/mock/audio-c",
    sourcePathLine: null,
    fetchedAtUtc: "2026-07-01T09:00:00Z",
    verifiedBy: "fixture reviewer",
    qualified: false,
    reason: "fetched_at is more than 60 days before now",
  },
];
