import type { ArchiveProgress } from "./ipc";

/**
 * Search-coverage honesty indicator (Lane C2).
 *
 * Local search only ever sees mail that has already been downloaded with a
 * body. While the background "download all" archive is incomplete, a "no
 * results" can really mean "not downloaded yet" — this computes the banner
 * copy that makes that limitation visible instead of silent. Server-side
 * search fallback is intentionally off the table (see
 * docs/LOCAL_SEARCH_ARCHITECTURE.md); this is the sanctioned honesty fix.
 */
export function searchCoverageBannerText(
  progress: ArchiveProgress | null,
): string | null {
  if (!progress || progress.full_archive) return null;
  const { with_body, total, active } = progress;
  const progressPart = active
    ? `${with_body.toLocaleString()} of ${total.toLocaleString()} messages downloaded, download in progress`
    : `${with_body.toLocaleString()} of ${total.toLocaleString()} messages downloaded`;
  return `Searched the local archive — ${progressPart}. Older mail may not appear yet.`;
}
