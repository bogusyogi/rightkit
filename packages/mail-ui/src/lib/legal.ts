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
import manifestRaw from "@rightkit-host/legal/legal-manifest.json?raw";

// Vite delivers the file's exact bytes. The tsx unit tests resolve the same specifier to the
// parsed JSON, so they get a stand-in text (the hash is only meaningful in the Vite build).
const manifestSource: unknown = manifestRaw;
const manifestText =
  typeof manifestSource === "string" ? manifestSource : JSON.stringify(manifestSource, null, 2);

export const LEGAL_MANIFEST = JSON.parse(manifestText) as LegalManifest;
export const REQUIRED_LEGAL_ACCEPTANCE_VERSION = LEGAL_MANIFEST.acceptanceVersion;
/** SHA-256 of the manifest file's bytes — the documents hash the native receipt service binds. */
export const LEGAL_MANIFEST_SHA256 = await sha256HexAsync(manifestText);

/** The acceptance-gate/Settings legal documents, in display order: [title, url]. */
export const LEGAL_DOCUMENT_LINKS: ReadonlyArray<readonly [string, string]> =
  LEGAL_MANIFEST.documents.map((document) => [document.title, document.publicUrl] as const);

/**
 * The document set the gate declares to `legalReceipt.create`, in manifest order. The native
 * service compares it with its own manifest field for field, so this is every document with
 * `publicUrl` declared as `url` and nothing else.
 */
export const LEGAL_RECEIPT_DOCUMENTS = LEGAL_MANIFEST.documents.map((document) => ({
  id: document.id,
  role: document.role,
  title: document.title,
  url: document.publicUrl,
  version: document.version,
  sha256: document.sha256,
  treatment: document.treatment,
  material: document.material,
}));

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
