// RIGHTKIT-PENDING #383: ported version saved in /Volumes/D/scratch/postright/port-wip/full-port.patch
// Invented typed briefs, platform variants and platform rules for the mock. Every brief, hook, caption, link and
// rule value is fictional. The rule values are fixtures: their sources are fixture paths and an example host, and
// they claim no published platform limit. Only the first channel has briefs. The second channel has none.
import type { BriefVariant, TypedBrief } from "../types";
import { SCOPE_ALPHA } from "./fixtures-optimise";

/** Two typed briefs in the first channel: one explainer and one product demo. */
export const FIXTURE_TYPED_BRIEFS: TypedBrief[] = [
  {
    // Explainer at revision 2. Its instagram variant from revision 1 is kept as it was made, so the screen says so.
    scopeId: SCOPE_ALPHA,
    briefId: "explainer-puddle-edges",
    briefType: "explainer",
    hook: "Fixture hook: puddles freeze at the edges first.",
    body: "Fixture body: a short outdoor explainer about why a shallow puddle skins over at its rim before its middle.",
    explainer: {
      question: {
        teaches: "The viewer asks why the rim freezes first.",
        frame: "A shallow puddle on grey paving, wide shot.",
        motion: "Slow push in on the rim.",
        words: "Ever notice the edge goes first?",
        sound: "Light wind and a single drip.",
      },
      model: {
        teaches: "Thin water at the edge cools faster.",
        frame: "A cross-section sketch of a puddle.",
        motion: "Arrows drawn over the sketch.",
        words: "The edge holds less water to keep warm.",
        sound: "Soft pencil scratch.",
      },
      proof: {
        teaches: "A probe reads colder at the rim.",
        frame: "Two probes, one at the rim and one at the middle.",
        motion: "Static, then a cut to the readings.",
        words: "The rim reads colder by the same morning.",
        sound: "Clicks from a timer.",
      },
      turn: {
        teaches: "Freezing starts where heat leaves fastest.",
        frame: "The sketch again, with the rim shaded.",
        motion: "The shading fades in.",
        words: "So the rim is where the ice starts.",
        sound: "Quiet ice cracking.",
      },
      payoff: {
        teaches: "Next time, check the rim first.",
        frame: "The puddle with a thin skin at its edge.",
        motion: "Hold, then fade.",
        words: "Watch the edge before you step on it.",
        sound: "Wind again.",
      },
    },
    demoSteps: [],
    revision: 2,
    createdAtUtc: "2026-09-20T09:00:00Z",
    updatedAtUtc: "2026-10-05T10:00:00Z",
  },
  {
    // Product demo at revision 1. Its steps are the text said while each step is recorded.
    scopeId: SCOPE_ALPHA,
    briefId: "demo-kettle-setup",
    briefType: "product_demo",
    hook: "Fixture hook: set up the fixture kettle in three steps.",
    body: "Fixture body: a product demo of a fictional kettle, filmed on one bench.",
    explainer: null,
    demoSteps: [
      { say: "Fill the fixture tank to the line." },
      { say: "Lock the fixture lid until it clicks." },
      { say: "Press the fixture button and wait for the light." },
    ],
    revision: 1,
    createdAtUtc: "2026-10-03T09:00:00Z",
    updatedAtUtc: "2026-10-03T09:00:00Z",
  },
];

/** Variants of the two briefs, on two platforms each. The explainer also has one instagram variant from revision 1. */
export const FIXTURE_BRIEF_VARIANTS: BriefVariant[] = [
  {
    // Explainer, instagram, revision 1: an older variant, kept as it was made.
    scopeId: SCOPE_ALPHA,
    variantId: "var_3c1e0a9b7d5f4e21a6b08c9d7e5f4a32",
    briefId: "explainer-puddle-edges",
    briefRevision: 1,
    platform: "instagram",
    ruleRevision: "fixture-rules-1",
    linkPlacement: "bio",
    caption: "Fixture hook: puddles skin over early.\n\nFixture body: an earlier, shorter explainer about puddle rims.\n\n#fixturepuddles",
    link: null,
    createdAtUtc: "2026-09-22T09:00:00Z",
  },
  {
    // Explainer, instagram, revision 2: the link goes in the bio, so the caption carries only the hashtags.
    scopeId: SCOPE_ALPHA,
    variantId: "var_3c1e0a9b7d5f4e21a6b08c9d7e5f4a31",
    briefId: "explainer-puddle-edges",
    briefRevision: 2,
    platform: "instagram",
    ruleRevision: "fixture-rules-1",
    linkPlacement: "bio",
    caption:
      "Fixture hook: puddles freeze at the edges first.\n\nFixture body: a short outdoor explainer about why a shallow puddle skins over at its rim.\n\n#fixturepuddles #fixturescience",
    link: "https://links.example/fixture-puddles",
    createdAtUtc: "2026-10-05T10:10:00Z",
  },
  {
    // Explainer, youtube, revision 2: the link goes in the first comment, so the caption does not carry it.
    scopeId: SCOPE_ALPHA,
    variantId: "var_9f2a4d6c8e0b1a3c5f7d9b2e4a6c8d10",
    briefId: "explainer-puddle-edges",
    briefRevision: 2,
    platform: "youtube",
    ruleRevision: "fixture-rules-1",
    linkPlacement: "first_comment",
    caption:
      "Fixture hook: puddles freeze at the edges first.\n\nFixture body: a short outdoor explainer about why a shallow puddle skins over at its rim. Watch the full fixture episode.\n\n#fixturepuddles #fixturescience",
    link: "https://links.example/fixture-puddles",
    createdAtUtc: "2026-10-05T10:12:00Z",
  },
  {
    // Demo, instagram, revision 1: no link was given, so the bio placement has no link to show.
    scopeId: SCOPE_ALPHA,
    variantId: "var_0d4b8f2a6c1e3b5d7f9a2c4e6b8d0f13",
    briefId: "demo-kettle-setup",
    briefRevision: 1,
    platform: "instagram",
    ruleRevision: "fixture-rules-1",
    linkPlacement: "bio",
    caption:
      "Fixture hook: set up the fixture kettle in three steps.\n\nFixture body: a product demo of a fictional kettle, filmed on one bench.\n\n#fixturekettle",
    link: null,
    createdAtUtc: "2026-10-03T10:00:00Z",
  },
  {
    // Demo, linkedin, revision 1: the link sits in the caption, which the caption text shows.
    scopeId: SCOPE_ALPHA,
    variantId: "var_0d4b8f2a6c1e3b5d7f9a2c4e6b8d0f12",
    briefId: "demo-kettle-setup",
    briefRevision: 1,
    platform: "linkedin",
    ruleRevision: "fixture-rules-1",
    linkPlacement: "caption",
    caption:
      "Fixture hook: set up the fixture kettle in three steps.\n\nFixture body: a product demo of a fictional kettle, filmed on one bench.\n\nhttps://links.example/fixture-kettle\n\n#fixturekettle",
    link: "https://links.example/fixture-kettle",
    createdAtUtc: "2026-10-03T10:05:00Z",
  },
];
