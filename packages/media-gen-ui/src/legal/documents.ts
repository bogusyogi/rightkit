import { assertLegalManifest, type LegalDocumentDescriptor, type LegalManifest } from '@rightkit/legal/browser';
import manifestJson from '@rightkit-host/legal/legal-manifest.json';
import contract from '@rightkit-host/src/generated/legal-contract.json';
import licenseSource from '@rightkit-host/legal/LICENSE.md?raw';
import eulaSource from '@rightkit-host/legal/EULA.md?raw';
import acceptableUseSource from '@rightkit-host/legal/ACCEPTABLE_USE.md?raw';
import productScheduleSource from '@rightkit-host/legal/PRODUCT_SCHEDULE.md?raw';
import privacyNoticeSource from '@rightkit-host/legal/PRIVACY_NOTICE.md?raw';
import thirdPartyNoticesSource from '@rightkit-host/legal/THIRD_PARTY_NOTICES.md?raw';
import { hostAppName } from "../appName";

/**
 * the host app legal documents. The document list, versions, treatments and hashes come from the
 * @rightkit/legal manifest that `legal-snapshot` writes (legal/legal-manifest.json, configured in
 * legal-snapshot.config.json); this module only pairs each manifest entry with its bundled text.
 */
export const LEGAL_MANIFEST: LegalManifest = assertLegalManifest(manifestJson as unknown as LegalManifest);

export type LegalDocumentId = 'genright-license' | 'genright-eula' | 'genright-acceptable-use' | 'genright-product-schedule' | 'genright-privacy-notice' | 'third-party-notices';

export interface LegalDocument {
  id: LegalDocumentId;
  title: string;
  version: string;
  treatment: LegalDocumentDescriptor['treatment'];
  content: string;
}

/** Bundled text by manifest path. The text's hash is verified against the manifest by `legal-snapshot --final`. */
const TEXT: Readonly<Record<string, string>> = {
  'LICENSE.md': licenseSource,
  'EULA.md': eulaSource,
  'ACCEPTABLE_USE.md': acceptableUseSource,
  'PRODUCT_SCHEDULE.md': productScheduleSource,
  'PRIVACY_NOTICE.md': privacyNoticeSource,
  'THIRD_PARTY_NOTICES.md': thirdPartyNoticesSource,
};

function toDocument(d: LegalDocumentDescriptor): LegalDocument {
  const content = TEXT[d.path];
  if (content === undefined) throw new Error(`no bundled text for legal document ${d.id} (${d.path})`);
  return { id: d.id as LegalDocumentId, title: d.title, version: d.version, treatment: d.treatment, content };
}

/** Documents listed on the first-run gate, in manifest order: agreements and the acknowledged privacy notice. */
export const LEGAL_DOCUMENT_SOURCES: readonly LegalDocument[] = LEGAL_MANIFEST.documents.filter((d) => d.treatment !== 'notice').map(toDocument);

/** Third-party notices: shown in Settings > Legal only, never part of the acceptance. */
export const THIRD_PARTY_NOTICES: LegalDocument = toDocument(LEGAL_MANIFEST.documents.find((d) => d.role === 'third_party_notices')!);

/**
 * materialAcceptanceDigest of the manifest, precomputed by `legal-snapshot` into the gate contract so
 * the webview needs no crypto. Changes only when an agreed (material) document changes.
 */
export const LEGAL_DOCUMENTS_HASH: string = contract.materialSha256;

export const LEGAL_AGREEMENT_STATEMENT =
  `I have read and agree to the ${hostAppName()} Desktop License, End User License Agreement, Acceptable Use Policy, and Product Schedule, and I acknowledge the Privacy Notice.`;

export function legalDocument(id: LegalDocumentId): LegalDocument {
  const found = [...LEGAL_DOCUMENT_SOURCES, THIRD_PARTY_NOTICES].find((d) => d.id === id);
  if (!found) throw new Error(`unknown legal document: ${id}`);
  return found;
}
