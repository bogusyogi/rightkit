import type { LegalStepConfig } from "@rightkit/onboarding";
import { assertLegalManifest, type LegalManifest } from "@rightkit/legal/browser";
import { videoAppHost } from "../../host.js";
import { logs } from "../lib/services.js";
import { quitNow } from "../lib/shell.js";
import { hostAppName } from "../appName.js";

/**
 * the host app facts for the suite legal gate, sourced from its legal/legal-manifest.json and generated
 * src/generated/legal-contract.json, never hardcoded, so a title, URL or document change cannot drift from the gate.
 * A published package cannot import files of the app that installs it: the host calls configureLegal() once at
 * startup. ES module exports are live bindings, so every importer sees the configured values.
 */
export interface LegalContract {
  acceptanceVersion: string;
  materialSha256: string;
  [key: string]: unknown;
}

/** Version the stored acceptance must match; a document change re-prompts. */
export let REQUIRED_LEGAL_ACCEPTANCE_VERSION: string;
/** Hash of the agreed (material) documents, recorded with the acceptance. */
export let LEGAL_MATERIAL_SHA256: string;

// Suite-wide master titles are long; the gate list shows short ones.
const SHORT_TITLES: Partial<Record<string, string>> = {
  license: "License",
  eula: "End User License Agreement",
  acceptable_use: "Acceptable Use Policy",
};

let TEXTS: Record<string, string> = {};
// The host's overrides of the gate's copy and look (`configureLegal({ step })`); the defaults are 0.1.x's.
let STEP: Partial<LegalStepConfig> = {};

export interface LegalDocumentText {
  title: string;
  text: string;
}

/** The bundled copy of a shipped legal document (offline, exactly what was hashed). */
export function legalDocumentText(file: string): string {
  return TEXTS[file.split("/").pop() ?? file] ?? "";
}

/** Every document the user agrees to or acknowledges (not the build-provenance notices). */
export let LEGAL_GATE_DOCUMENTS: { title: string; url: string; path: string }[] = [];

/**
 * Called once by the host at startup with its manifest, contract and the bundled legal texts keyed by file name
 * (a Vite host: `import.meta.glob("/legal/*.md", { query: "?raw", import: "default", eager: true })`). `step`
 * overrides any field of the gate's `LegalStepConfig` (trust line, agreement statement, theme, wordmark font,
 * onQuit); left out, the gate keeps the 0.1.x copy and look.
 */
export function configureLegal({
  manifest,
  contract,
  texts = {},
  step = {},
}: {
  manifest: unknown;
  contract: LegalContract;
  texts?: Record<string, string>;
  step?: Partial<LegalStepConfig>;
}): void {
  const m = assertLegalManifest(manifest as LegalManifest);
  if (typeof contract?.acceptanceVersion !== "string" || typeof contract?.materialSha256 !== "string") {
    throw new TypeError("configureLegal: the contract needs acceptanceVersion and materialSha256 strings");
  }
  REQUIRED_LEGAL_ACCEPTANCE_VERSION = contract.acceptanceVersion;
  LEGAL_MATERIAL_SHA256 = contract.materialSha256;
  STEP = step;
  TEXTS = Object.fromEntries(Object.entries(texts).map(([k, v]) => [k.split("/").pop() ?? k, v]));
  LEGAL_GATE_DOCUMENTS = m.documents
    .filter((d) => d.role !== "third_party_notices")
    .map((d) => ({ title: SHORT_TITLES[d.role] ?? d.title, url: d.publicUrl, path: String((d as { path?: string }).path ?? "") }));
}

export function legalStepConfig(): LegalStepConfig {
  const defaults: LegalStepConfig = {
    appName: hostAppName(),
    trustLine: `${hostAppName()} edits your footage on this computer. Your media is never uploaded by ${hostAppName()}.`,
    // Each link expands the bundled copy inline in the gate: it works offline and is the exact text agreed to.
    documents: LEGAL_GATE_DOCUMENTS.map((d) => ({
      title: d.title,
      url: d.url,
      content: legalDocumentText(d.path),
      contentFormat: "markdown" as const,
    })),
    agreementStatement:
      `I have read and agree to the ${hostAppName()} Desktop License, End User License Agreement, Acceptable Use Policy, and Product Schedule, and I acknowledge the Privacy Notice.`,
    // The suite gate is light-only: suite ivory base with the host app AA light accent.
    theme: {
      bg: "#FBF7EF",
      surface: "#FFFCF5",
      border: "#E6DECF",
      text: "#231F1A",
      muted: "#6B655C",
      accent: "#0C9A43",
      accentContrast: "#1A1208",
      fontDisplay: "Tanker, 'Arial Narrow', sans-serif",
      fontBody: "Author, -apple-system, BlinkMacSystemFont, sans-serif",
    },
    wordmarkFontUrl: "/fonts/Tanker-400.woff2",
    // Declining quits the app. The gate stays in front until the process is gone, so no
    // unaccepted UI is reachable. Both paths end in the host's RunEvent::Exit cleanup.
    onQuit: async () => {
      try {
        await quitNow();
      } catch {
        logs.error("legal_decline_quit_failed", { surface: "legal" });
        await logs.flush().catch(() => undefined);
        await videoAppHost().exit(0);
      }
    },
  };
  return { ...defaults, ...STEP };
}
