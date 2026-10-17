import type { LegalGateDocument } from "./LegalGate.js";

/** Structural match for `RenderedPolicyDocument` from `@rightkit/legal` (no package dependency). */
export interface DefaultPolicyDocumentLike {
  readonly title: string;
  readonly content: string;
}

/**
 * Turns the default policy texts rendered by `@rightkit/legal` (`renderDefaultPolicies`) into
 * `LegalGate` documents. Each renders inline as Markdown under its title, so an app passes
 * no URL and carries no copy of the text. Pass `urls` (keyed by title) to also link a public page.
 */
export function policyGateDocuments(
  documents: readonly DefaultPolicyDocumentLike[],
  urls: Readonly<Record<string, string>> = {},
): LegalGateDocument[] {
  return documents.map((doc) => ({
    title: doc.title,
    ...(urls[doc.title] ? { url: urls[doc.title] } : {}),
    content: doc.content,
    contentFormat: "markdown" as const,
  }));
}
