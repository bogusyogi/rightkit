// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented analytics for the mock. Every post, remote id, pull, import and number is fictional. The first channel has
// two posts, one unavailable metric and one manual export. The other channels have nothing, so they show the empty state.
// The latest value of each metric is derived from the observations in latestAnalytics (mockDataSource.ts), so the two
// cannot drift apart.
import type { AnalyticsObservation, AnalyticsPost, MetricImport } from "../types";
import { SCOPE_ALPHA } from "./fixtures-optimise";

const POST_ONE = "fixture-job-analytics-1";
const POST_TWO = "fixture-job-analytics-2";

/** The facts of each post, before its metrics. */
export const FIXTURE_ANALYTICS_POSTS: Omit<AnalyticsPost, "metrics">[] = [
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_ONE,
    platform: "instagram",
    remotePostId: "fixture-ig-0001",
    publishedAtUtc: "2026-09-02T10:00:00Z",
  },
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_TWO,
    platform: "youtube",
    remotePostId: "fixture-yt-0002",
    publishedAtUtc: "2026-09-20T18:00:00Z",
  },
];

/** Every stored observation. API rows come from a pull; the manual row comes from a fixture import. */
export const FIXTURE_ANALYTICS_OBSERVATIONS: AnalyticsObservation[] = [
  // Post one, first pull: views only.
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_ONE,
    metric: "views",
    state: "value",
    value: "1520",
    observedAtUtc: "2026-09-03T10:00:00Z",
    window: "lifetime",
    source: "api",
    pullId: "fixture-pull-0903",
    importId: null,
  },
  // Post one, second pull: views, saves, and shares, which this platform does not provide.
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_ONE,
    metric: "views",
    state: "value",
    value: "4210",
    observedAtUtc: "2026-10-01T10:00:00Z",
    window: "lifetime",
    source: "api",
    pullId: "fixture-pull-1001",
    importId: null,
  },
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_ONE,
    metric: "saves",
    state: "value",
    value: "88",
    observedAtUtc: "2026-10-01T10:00:00Z",
    window: "lifetime",
    source: "api",
    pullId: "fixture-pull-1001",
    importId: null,
  },
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_ONE,
    metric: "shares",
    state: "unavailable",
    value: null,
    observedAtUtc: "2026-10-01T10:00:00Z",
    window: "lifetime",
    source: "api",
    pullId: "fixture-pull-1001",
    importId: null,
  },
  // Post two: an API pull, then a manual export of a metric the API does not give.
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_TWO,
    metric: "views",
    state: "value",
    value: "512",
    observedAtUtc: "2026-09-25T09:00:00Z",
    window: "lifetime",
    source: "api",
    pullId: "fixture-pull-0925",
    importId: null,
  },
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_TWO,
    metric: "views",
    state: "value",
    value: "733",
    observedAtUtc: "2026-10-01T10:00:00Z",
    window: "lifetime",
    source: "api",
    pullId: "fixture-pull-1001",
    importId: null,
  },
  {
    scopeId: SCOPE_ALPHA,
    jobId: POST_TWO,
    metric: "avg_view_percent",
    state: "value",
    value: "41.5",
    observedAtUtc: "2026-10-05T09:00:00Z",
    window: null,
    source: "manual_export",
    pullId: null,
    importId: "fixture-import-retention-1",
  },
];

/** Every committed manual import in the first channel. */
export const FIXTURE_METRIC_IMPORTS: MetricImport[] = [
  {
    scopeId: SCOPE_ALPHA,
    importId: "fixture-import-retention-1",
    platform: "youtube",
    sourceNote: "Fixture export: an invented retention table, saved by hand",
    source: "manual_export",
    createdAtUtc: "2026-10-05T09:05:00Z",
    rowsWritten: 1,
    observationsWritten: 1,
  },
];
