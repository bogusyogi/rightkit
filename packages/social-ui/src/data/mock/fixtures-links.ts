// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented campaigns, tracked links and visit counts for the mock. Ids, names, destinations and the
// calendar entry ids are fictional. The final URLs follow the engine's UTM policy (docs/LINKS.md): the
// UTM parts are appended in order, and a fragment stays last. Visit counts are invented integers.
import type { Campaign, TrackedLink } from "../types";
import { SCOPE_ALPHA, SCOPE_BETA } from "./fixtures-optimise";

export const FIXTURE_CAMPAIGNS: Campaign[] = [
  {
    // Two links, both with visits.
    id: "fixture-spring-launch",
    scopeId: SCOPE_ALPHA,
    name: "Fixture spring launch",
    utmCampaign: "fixture-spring-launch",
    revision: 3,
    createdAtUtc: "2026-10-01T09:00:00Z",
    updatedAtUtc: "2026-10-09T10:00:00Z",
  },
  {
    // One link, with zero visits: a normal state.
    id: "fixture-autumn-recap",
    scopeId: SCOPE_ALPHA,
    name: "Fixture autumn recap",
    utmCampaign: "fixture-autumn-recap",
    revision: 1,
    createdAtUtc: "2026-10-02T09:00:00Z",
    updatedAtUtc: "2026-10-02T09:00:00Z",
  },
  {
    // No links and no visits: the empty state of a campaign.
    id: "fixture-beta-empty",
    scopeId: SCOPE_BETA,
    name: "Fixture beta empty",
    utmCampaign: "fixture-beta-empty",
    revision: 1,
    createdAtUtc: "2026-10-10T09:00:00Z",
    updatedAtUtc: "2026-10-10T09:00:00Z",
  },
];

export const FIXTURE_LINKS: TrackedLink[] = [
  {
    id: "lnk_1f2e3d4c5b6a79881a2b3c4d5e6f7081",
    scopeId: SCOPE_ALPHA,
    campaignId: "fixture-spring-launch",
    variantId: "v-a",
    platform: "instagram",
    destination: "https://www.example.com/fixture-landing",
    calendarEntryId: "fixture-entry-0141",
    utmSource: "instagram",
    utmMedium: "social",
    utmCampaign: "fixture-spring-launch",
    utmContent: "v-a",
    finalUrl:
      "https://www.example.com/fixture-landing?utm_source=instagram&utm_medium=social&utm_campaign=fixture-spring-launch&utm_content=v-a",
    createdAtUtc: "2026-10-02T08:00:00Z",
  },
  {
    id: "lnk_9a8b7c6d5e4f30211a0b9c8d7e6f5041",
    scopeId: SCOPE_ALPHA,
    campaignId: "fixture-spring-launch",
    variantId: "v-b",
    platform: "instagram",
    destination: "https://www.example.com/fixture-landing",
    calendarEntryId: null,
    utmSource: "instagram",
    utmMedium: "social",
    utmCampaign: "fixture-spring-launch",
    utmContent: "v-b",
    finalUrl:
      "https://www.example.com/fixture-landing?utm_source=instagram&utm_medium=social&utm_campaign=fixture-spring-launch&utm_content=v-b",
    createdAtUtc: "2026-10-02T08:05:00Z",
  },
  {
    // A destination with a query and a fragment: the UTM parts join the query and the fragment stays last.
    // It sits in the second campaign, so the channel has three links across two campaigns.
    id: "lnk_0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f",
    scopeId: SCOPE_ALPHA,
    campaignId: "fixture-autumn-recap",
    variantId: "v-a",
    platform: "youtube",
    destination: "https://www.example.com/fixture-landing?ref=fixture#top",
    calendarEntryId: "fixture-entry-0127",
    utmSource: "youtube",
    utmMedium: "social",
    utmCampaign: "fixture-autumn-recap",
    utmContent: "v-a",
    finalUrl:
      "https://www.example.com/fixture-landing?ref=fixture&utm_source=youtube&utm_medium=social&utm_campaign=fixture-autumn-recap&utm_content=v-a#top",
    createdAtUtc: "2026-10-03T08:00:00Z",
  },
];

/** Visits recorded per link id. A link missing here has no visits, which is a count of zero. */
export const FIXTURE_LINK_VISITS: Record<string, number> = {
  "lnk_1f2e3d4c5b6a79881a2b3c4d5e6f7081": 412,
  "lnk_9a8b7c6d5e4f30211a0b9c8d7e6f5041": 97,
  // Zero visits: a normal state, shown as 0.
  "lnk_0c1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f": 0,
};
