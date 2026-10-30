// The one definition of a SUCCESSFUL publication row (GB-45; GATE E9, same-bytes retry reconciliation), shared by its producer (scripts/ci/upload.mjs
// writes the rows) and its consumer (unit-gate.mjs reads them back). A leaf module: node builtins only.
//
// Two outcomes are success, and both mean the registry serves the sealed bytes: `published` (this attempt sent them and read them back) and
// `published-by-earlier-attempt` (an earlier attempt sent them; this attempt found the exact version AND hash on the registry before sending
// anything, and `publishedBy: "earlier-attempt"` says so). A generic "already exists" answer is neither: the producer only records the second after
// reconcileUnit proved the same bytes, and the consumer still reads the registry back itself against the row's hash.
export const PUBLISHED = "published";
export const PUBLISHED_BY_EARLIER_ATTEMPT = "published-by-earlier-attempt";
export const SUCCESSFUL_PUBLICATION_OUTCOMES = Object.freeze([PUBLISHED, PUBLISHED_BY_EARLIER_ATTEMPT]);

/** Why `row` is not a verified successful publication row, or null when it is. */
export function publicationRowProblem(row) {
  if (!row || typeof row !== "object") return "no row";
  if (row.published !== true) return "it is not recorded as published";
  if (row.registryVerified !== true) return "it is not registry-verified";
  if (!SUCCESSFUL_PUBLICATION_OUTCOMES.includes(row.outcome)) return `its outcome is ${JSON.stringify(row.outcome ?? null)}, not one of ${SUCCESSFUL_PUBLICATION_OUTCOMES.join(" or ")}`;
  if (!/^[0-9a-f]{64}$/.test(row.sha256 ?? "")) return "it carries no sha256";
  if (row.outcome === PUBLISHED_BY_EARLIER_ATTEMPT && row.publishedBy !== "earlier-attempt") return "its provenance is ambiguous (a reconciled prior publication must name publishedBy: earlier-attempt)";
  return null;
}
