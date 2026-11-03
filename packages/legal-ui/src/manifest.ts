import type { LegalGateDocument } from "./LegalGate.js";

/** One document of a legal manifest, structurally: the fields `@rightkit/legal`'s `LegalManifest` documents carry. */
export interface LegalManifestDocumentLike {
  role: string;
  title: string;
  publicUrl: string;
}

/** A legal manifest, structurally (`@rightkit/legal` `LegalManifest` fits). Only `documents` is read. */
export interface LegalManifestLike {
  documents: readonly LegalManifestDocumentLike[];
}

export interface ManifestGateDocumentsOptions {
  /** Display title per document role, shown instead of the manifest title. Roles not listed keep the manifest title. */
  shortTitles?: Readonly<Record<string, string | undefined>>;
  /** Document roles left out (for example `third_party_notices` from the first-run gate). */
  excludeRoles?: readonly string[];
}

/**
 * The gate's document links for a legal manifest: one `{ title, url }` per document, in manifest order, with the
 * hosted `publicUrl`. `shortTitles` maps a role to its display title and `excludeRoles` drops roles, so an app keeps
 * only its title maps. Pure: the manifest is not modified.
 */
export function manifestGateDocuments(manifest: LegalManifestLike, options: ManifestGateDocumentsOptions = {}): LegalGateDocument[] {
  const excluded = new Set(options.excludeRoles ?? []);
  return manifest.documents
    .filter((document) => !excluded.has(document.role))
    .map((document) => ({
      title: options.shortTitles?.[document.role] ?? document.title,
      url: document.publicUrl,
    }));
}
