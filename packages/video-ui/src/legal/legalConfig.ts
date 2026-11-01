import type { LegalStepConfig } from "@rightkit/onboarding";
import { assertLegalManifest, type LegalManifest } from "@rightkit/legal/browser";
import { exit } from "@tauri-apps/plugin-process";
import { logs } from "../lib/services";
import { quitNow } from "../lib/shell";
import legalManifest from "@rightkit-host/legal/legal-manifest.json";
import legalContract from "@rightkit-host/src/generated/legal-contract.json";
import { hostAppName } from "../appName";

const manifest = assertLegalManifest(legalManifest as unknown as LegalManifest);

/**
 * the host app facts for the suite legal gate, sourced from legal/legal-manifest.json
 * (written by @rightkit/legal's legal-snapshot bin from RightKit's legal source), never
 * hardcoded, so a title, URL or document change cannot drift from the gate.
 */

/** Version the stored acceptance must match; a document change re-prompts. */
export const REQUIRED_LEGAL_ACCEPTANCE_VERSION = legalContract.acceptanceVersion;
/** Hash of the agreed (material) documents, recorded with the acceptance. */
export const LEGAL_MATERIAL_SHA256 = legalContract.materialSha256;

// Suite-wide master titles are long; the gate list shows short ones.
const SHORT_TITLES: Partial<Record<string, string>> = {
  license: "License",
  eula: "End User License Agreement",
  acceptable_use: "Acceptable Use Policy",
};

const TEXTS = import.meta.glob("/legal/*.md", { query: "?raw", import: "default", eager: true }) as Record<
  string,
  string
>;

export interface LegalDocumentText {
  title: string;
  text: string;
}

/** The bundled copy of a shipped legal document (offline, exactly what was hashed). */
export function legalDocumentText(file: string): string {
  return TEXTS[`../../legal/${file}`] ?? "";
}

/** Every document the user agrees to or acknowledges (not the build-provenance notices). */
export const LEGAL_GATE_DOCUMENTS = manifest.documents
  .filter((d) => d.role !== "third_party_notices")
  .map((d) => ({ title: SHORT_TITLES[d.role] ?? d.title, url: d.publicUrl, path: d.path }));

export function legalStepConfig(): LegalStepConfig {
  return {
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
        await exit(0);
      }
    },
  };
}
