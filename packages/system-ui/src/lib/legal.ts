import { policyGateDocuments } from "@rightkit/legal-ui";
import { createNativeLegalReceiptPort, type LegalStepConfig } from "@rightkit/onboarding";
import { invoke } from "@tauri-apps/api/core";
import { createShell } from "@rightkit/tauri-shell";
import { LEGAL_DOCUMENTS, LEGAL_DOCUMENTS_HASH, LEGAL_RECEIPT_DOCUMENTS, LEGAL_VERSION } from "virtual:rightkit-legal";

export { LEGAL_DOCUMENTS_HASH, LEGAL_RECEIPT_DOCUMENTS, LEGAL_VERSION };

const documents = policyGateDocuments(LEGAL_DOCUMENTS);

/** The agreement on file, read and recorded by the native receipt service, which stamps the time (src-tauri/src/legal.rs). */
export const legalReceipt = createNativeLegalReceiptPort({ invoke });

/** The legal gate: suite default policies (rendered at build time), host colours from the shell tokens. */
export const legalStepConfig: LegalStepConfig = {
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
};
