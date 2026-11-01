import { policyGateDocuments } from "@rightkit/legal-ui";
import { createNativeLegalReceiptPort, type LegalStepConfig } from "@rightkit/onboarding";
import { invoke } from "@tauri-apps/api/core";
import { createShell } from "@rightkit/tauri-shell";
/** What the host's `virtual:rightkit-legal` build module provides (@rightkit/legal's Vite plugin). */
export interface HostLegal {
  LEGAL_DOCUMENTS: readonly { title: string; content: string }[];
  LEGAL_VERSION: string;
  LEGAL_DOCUMENTS_HASH: string;
  LEGAL_RECEIPT_DOCUMENTS: readonly { id: string; role: string; version: string; sha256: string; treatment: string; material: boolean }[];
}

// Set once by the host at startup (configureLegal(await import("virtual:rightkit-legal"))): a published package cannot import a
// build-time module of the app that installs it. ES module exports are live bindings, so importers see the configured values.
export let LEGAL_VERSION: string;
export let LEGAL_DOCUMENTS_HASH: string;
export let LEGAL_RECEIPT_DOCUMENTS: HostLegal["LEGAL_RECEIPT_DOCUMENTS"] = [];
let documents: ReturnType<typeof policyGateDocuments> = [];

export function configureLegal(legal: HostLegal): void {
  if (!legal || typeof legal.LEGAL_VERSION !== "string" || !Array.isArray(legal.LEGAL_DOCUMENTS)) {
    throw new TypeError("configureLegal: pass the host's virtual:rightkit-legal module");
  }
  LEGAL_VERSION = legal.LEGAL_VERSION;
  LEGAL_DOCUMENTS_HASH = legal.LEGAL_DOCUMENTS_HASH;
  LEGAL_RECEIPT_DOCUMENTS = legal.LEGAL_RECEIPT_DOCUMENTS;
  documents = policyGateDocuments(legal.LEGAL_DOCUMENTS);
}

/** The agreement on file, read and recorded by the native receipt service, which stamps the time (src-tauri/src/legal.rs). */
export const legalReceipt = createNativeLegalReceiptPort({ invoke });

/** The legal gate: suite default policies (rendered at build time), host colours from the shell tokens. */
export const legalStepConfig = (): LegalStepConfig => ({
  appName: "Pulse",
  trustLine: "Pulse watches your computer's storage and your agent chats locally. Nothing leaves this computer unless you turn a feature on.",
  documents,
  agreementStatement: `I have read and agree to the ${documents.map((d) => d.title).join(", ")}.`,
  theme: {
    bg: "var(--rk-plane)",
    surface: "var(--rk-panel)",
    border: "var(--rk-line-strong)",
    text: "var(--rk-ink)",
    muted: "var(--rk-ink-2)",
    accent: "var(--rk-accent)",
    accentContrast: "var(--rk-accent-ink)",
  },
  // The hub window closes; the notch keeps running (the hub's lifecycle is fixed).
  onQuit: () => void createShell().window.close().catch(() => undefined),
});
