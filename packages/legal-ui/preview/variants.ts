import type { LegalGateDocument, LegalGateProps, LegalGateTheme } from "../src/index.js";

/**
 * Local-only preview data. NOT published (preview/ is excluded from the
 * package's `files`). Themes are transcribed from `.claude/rules/brands.md`
 * (Right Suite — App Identities) — LIGHT tokens only: Adrian locked the legal
 * gate to light mode for every app (2026-07-17), regardless of the app's own
 * theme setting.
 *
 * Fonts: the wordmark face is LOCKED to Tanker suite-wide; the preview loads
 * `preview/fonts/Tanker-400.woff2` (copied from viewright/public/fonts) via
 * @font-face in index.html. Body renders in Hanken Grotesk (loaded from
 * heardright's public fonts) as the fallback for each app's own body face,
 * which is acceptable for body only — never for the wordmarks.
 */

export interface AppVariant {
  appKey: string;
  appName: string;
  domain: string;
  theme: LegalGateTheme;
  trustLine: string;
}

const DISPLAY = "Tanker, 'Arial Narrow', sans-serif";
const bodyStack = (first?: string): string =>
  first ? `${first}, 'Hanken Grotesk', sans-serif` : "'Hanken Grotesk', sans-serif";

export const APPS: AppVariant[] = [
  {
    appKey: "heardright",
    appName: "HeardRight",
    domain: "heardright.app",
    trustLine:
      "HeardRight transcribes and listens for its wake word on your device — AI features run only if you add your own API key.",
    theme: {
      bg: "#F7F3EC",
      surface: "#FFFFFF",
      border: "#E4DCDA",
      text: "#272322",
      muted: "#686160",
      accent: "#DF3400",
      accentContrast: "#FFFFFF",
      fontDisplay: DISPLAY,
      fontBody: bodyStack(),
    },
  },
  {
    appKey: "viewright",
    appName: "ViewRight",
    domain: "viewright.cc",
    trustLine: "ViewRight works on the documents already on your disk. Nothing is uploaded to open or edit them.",
    theme: {
      bg: "#F7F3EC",
      surface: "#FFFDF8",
      border: "#E2DAD1",
      text: "#211C18",
      muted: "#6E665F",
      accent: "#0C6AA5",
      accentContrast: "#FFFFFF",
      fontDisplay: DISPLAY,
      fontBody: bodyStack("Geist"),
    },
  },
  {
    appKey: "scraperight",
    appName: "ScrapeRight",
    domain: "scraperight.app",
    trustLine: "ScrapeRight turns your files into text on your device. Fetching a page only reaches the address you give it.",
    theme: {
      bg: "#FBF7EF",
      surface: "#FFFCF5",
      border: "#E6DECF",
      text: "#231F1A",
      muted: "#6B655C",
      accent: "#845C00",
      accentContrast: "#FFFFFF",
      fontDisplay: DISPLAY,
      fontBody: bodyStack("Author"),
    },
  },
  {
    appKey: "mailright",
    appName: "MailRight",
    domain: "mailright.cc",
    trustLine: "MailRight stores your mail locally and talks directly to your Google account — no Orthic Labs server in between.",
    theme: {
      bg: "#F7F3EC",
      surface: "#FFF8F1",
      border: "#E7DED2",
      text: "#1A1B1C",
      muted: "#5E6164",
      accent: "#DF0022",
      accentContrast: "#FFFFFF",
      fontDisplay: DISPLAY,
      fontBody: bodyStack("Sentient"),
    },
  },
  {
    appKey: "coderight",
    appName: "CodeRight",
    domain: "coderight.cc",
    trustLine: "CodeRight reviews the code on your machine through the AI key you provide. Nothing else leaves your device.",
    theme: {
      bg: "#F4F6F2",
      surface: "#FCFBF4",
      border: "#D7DCD0",
      text: "#20242C",
      muted: "#626A73",
      accent: "#C92642",
      accentContrast: "#FFFFFF",
      fontDisplay: DISPLAY,
      fontBody: bodyStack("'General Sans'"),
    },
  },
];

function documentsFor(app: AppVariant): LegalGateDocument[] {
  return [
    { title: "License", url: `https://${app.domain}/legal/license/` },
    { title: "End User License Agreement", url: `https://${app.domain}/legal/eula/` },
    { title: "Acceptable Use Policy", url: `https://${app.domain}/legal/acceptable-use/` },
    { title: `${app.appName} Product Schedule`, url: `https://${app.domain}/legal/product-schedule/` },
    { title: `${app.appName} Privacy Notice`, url: `https://${app.domain}/legal/privacy/` },
  ];
}

function agreementStatementFor(app: AppVariant): string {
  return `I have read and agree to the ${app.appName} License, End User License Agreement, Acceptable Use Policy, and Product Schedule.`;
}

export interface PreviewVariant {
  id: string; // "<appKey>-light" (light-only; ids keep the -light suffix for stable paths)
  appKey: string;
  props: LegalGateProps;
}

/** The five first-run variants shown to Adrian for the taste gate: five apps, light-only. */
export const PREVIEW_VARIANTS: PreviewVariant[] = APPS.map((app) => ({
  id: `${app.appKey}-light`,
  appKey: app.appKey,
  props: {
    theme: app.theme,
    appName: app.appName,
    trustLine: app.trustLine,
    documents: documentsFor(app),
    agreementStatement: agreementStatementFor(app),
    variant: "first-run",
    // parentLogoSrc is intentionally unset: the Orthic mark is inlined by the
    // component so its frame can take each app's ink color. The on-disk assets
    // are dark-background variants and vanish against these cream bases.
    onAgree: () => {},
    onQuit: () => {},
  } satisfies LegalGateProps,
}));

export function findVariant(id: string): PreviewVariant | undefined {
  return PREVIEW_VARIANTS.find((variant) => variant.id === id);
}
