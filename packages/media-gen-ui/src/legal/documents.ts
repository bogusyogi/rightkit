import { assertLegalManifest, type LegalDocumentDescriptor, type LegalManifest } from '@rightkit/legal/browser';
import { hostAppName } from "../appName.js";

/**
 * the host app legal documents. The document list, versions, treatments and hashes come from the
 * @rightkit/legal manifest that `legal-snapshot` writes (legal/legal-manifest.json, configured in
 * legal-snapshot.config.json); this module only pairs each manifest entry with its bundled text.
 */
// Set once by the host at startup (configureLegal): a published package cannot import files of the app that installs it.
// ES module exports are live bindings, so every importer sees the configured values.
export let LEGAL_MANIFEST: LegalManifest;

export type LegalDocumentId = 'genright-license' | 'genright-eula' | 'genright-acceptable-use' | 'genright-product-schedule' | 'genright-privacy-notice' | 'third-party-notices';

export interface LegalDocument {
  id: LegalDocumentId;
  title: string;
  version: string;
  treatment: LegalDocumentDescriptor['treatment'];
  content: string;
}

/** Bundled text by manifest path. The text's hash is verified against the manifest by `legal-snapshot --final`. */
let TEXT: Readonly<Record<string, string>> = {};

function toDocument(d: LegalDocumentDescriptor): LegalDocument {
  const content = TEXT[d.path];
  if (content === undefined) throw new Error(`no bundled text for legal document ${d.id} (${d.path})`);
  return { id: d.id as LegalDocumentId, title: d.title, version: d.version, treatment: d.treatment, content };
}

/** Documents listed on the first-run gate, in manifest order: agreements and the acknowledged privacy notice. */
export let LEGAL_DOCUMENT_SOURCES: readonly LegalDocument[] = [];

/** Third-party notices: shown in Settings > Legal only, never part of the acceptance. */
export let THIRD_PARTY_NOTICES: LegalDocument;

/**
 * materialAcceptanceDigest of the manifest, precomputed by `legal-snapshot` into the gate contract so
 * the webview needs no crypto. Changes only when an agreed (material) document changes.
 */
export let LEGAL_DOCUMENTS_HASH: string;

/**
 * Called once by the host at startup with its legal/legal-manifest.json, src/generated/legal-contract.json and the bundled
 * text of each manifest document keyed by file name ('LICENSE.md', 'EULA.md', ...; a Vite host imports them with ?raw).
 */
export function configureLegal({ manifest, contract, texts }: { manifest: unknown; contract: { materialSha256: string }; texts: Record<string, string> }): void {
  LEGAL_MANIFEST = assertLegalManifest(manifest as LegalManifest);
  if (typeof contract?.materialSha256 !== 'string') throw new TypeError('configureLegal: the contract needs materialSha256');
  TEXT = { ...texts };
  LEGAL_DOCUMENT_SOURCES = LEGAL_MANIFEST.documents.filter((d) => d.treatment !== 'notice').map(toDocument);
  THIRD_PARTY_NOTICES = toDocument(LEGAL_MANIFEST.documents.find((d) => d.role === 'third_party_notices')!);
  LEGAL_DOCUMENTS_HASH = contract.materialSha256;
}

export const LEGAL_AGREEMENT_STATEMENT =
  `I have read and agree to the ${hostAppName()} Desktop License, End User License Agreement, Acceptable Use Policy, and Product Schedule, and I acknowledge the Privacy Notice.`;

export function legalDocument(id: LegalDocumentId): LegalDocument {
  const found = [...LEGAL_DOCUMENT_SOURCES, THIRD_PARTY_NOTICES].find((d) => d.id === id);
  if (!found) throw new Error(`unknown legal document: ${id}`);
  return found;
}
