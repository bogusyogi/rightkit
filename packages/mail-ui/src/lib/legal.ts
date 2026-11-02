// First-run legal acceptance contract — the Right Suite Desktop License v1.1
// two-basis model (individual | enterprise; see `@rightkit/legal` schema 2). This
// module owns the version/hash the app requires, the document links, the document set the gate
// declares to the native receipt, and the "is the stored receipt current" predicate. All of it
// derives from `legal/legal-manifest.json`: regenerating that manifest for a material change
// changes the required version (its `acceptanceVersion`) and hash together, so an
// acceptance version is never reused for different document bytes. The native side
// (src-tauri/src/commands/legal.rs) embeds the same file and refuses a declaration that differs.

import { sha256HexAsync, type LegalManifest } from "@rightkit/legal/browser";
import { isLegalAccepted, type LegalAcceptanceRecord } from "@rightkit/onboarding";
export type LegalReceiptDocument = {
  id: string; role: string; title: string; url: string; version?: string; sha256?: string; treatment?: string; material?: boolean;
};

// Set once by the host at startup (configureLegalManifest): a published package cannot import a file of the app that installs
// it. ES module exports are live bindings, so every importer sees the configured values.
export let LEGAL_MANIFEST: LegalManifest;
export let REQUIRED_LEGAL_ACCEPTANCE_VERSION: string;
/** SHA-256 of the manifest file's bytes — the documents hash the native receipt service binds. */
export let LEGAL_MANIFEST_SHA256: string;
/** The acceptance-gate/Settings legal documents, in display order: [title, url]. */
export let LEGAL_DOCUMENT_LINKS: ReadonlyArray<readonly [string, string]> = [];
/**
 * The document set the gate declares to `legalReceipt.create`, in manifest order. The native
 * service compares it with its own manifest field for field, so this is every document with
 * `publicUrl` declared as `url` and nothing else.
 */
export let LEGAL_RECEIPT_DOCUMENTS: LegalReceiptDocument[] = [];

/**
 * The host passes the exact bytes of its legal/legal-manifest.json (a Vite host: `import text from ".../legal-manifest.json?raw"`);
 * the documents hash is the SHA-256 of those bytes, as the native receipt service computes it.
 */
export async function configureLegalManifest(manifestText: string): Promise<void> {
  if (typeof manifestText !== "string") throw new TypeError("configureLegalManifest: pass the manifest file's exact text");
  const manifest = JSON.parse(manifestText) as LegalManifest;
  LEGAL_MANIFEST = manifest;
  REQUIRED_LEGAL_ACCEPTANCE_VERSION = manifest.acceptanceVersion;
  LEGAL_MANIFEST_SHA256 = await sha256HexAsync(manifestText);
  LEGAL_DOCUMENT_LINKS = manifest.documents.map((document) => [document.title, document.publicUrl] as const);
  LEGAL_RECEIPT_DOCUMENTS = manifest.documents.map((document) => ({
    id: document.id,
    role: document.role,
    title: document.title,
    url: document.publicUrl,
    version: document.version,
    sha256: document.sha256,
    treatment: document.treatment,
    material: document.material,
  }));
}

/** True when the stored receipt is for the version and documents hash this build requires. */
export function legalAcceptanceCurrent(record: LegalAcceptanceRecord | null): boolean {
  if (!record) return false;
  return isLegalAccepted(
    {
      version: 1,
      currentStep: 0,
      completed: [],
      skipped: false,
      legalAccepted: true,
      legalAcceptance: record,
    },
    REQUIRED_LEGAL_ACCEPTANCE_VERSION,
    undefined,
    LEGAL_MANIFEST_SHA256,
  );
}
