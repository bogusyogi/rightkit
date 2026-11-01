/**
 * Local URL classification for Markdown analysis.
 *
 * The single owner of local-vs-remote detection for Markdown links
 * and images. The renderer (and any other consumer) imports these
 * helpers instead of inlining its own regex.
 */

/**
 * Returns `true` when `rawUrl` resolves to a file inside the
 * document tree (no scheme, no `//`, no `mailto:`, no `tel:`, no
 * absolute URL).
 */
export function isLocalMarkdownReference(rawUrl: string): boolean {
  if (rawUrl.length === 0) return false;
  if (rawUrl.startsWith("//")) return false;
  if (/^[a-z][a-z0-9+.-]*:/i.test(rawUrl)) return false;
  if (rawUrl.startsWith("mailto:") || rawUrl.startsWith("tel:")) {
    return false;
  }
  return true;
}

/**
 * Split `rawUrl` into the file path and the optional `#fragment`.
 * The fragment is returned as-is (including leading `#` when
 * present); the path is URL-decoded.
 */
export function splitMarkdownUrl(rawUrl: string): {
  pathPart: string;
  fragment: string | null;
} {
  const hashIndex = rawUrl.indexOf("#");
  if (hashIndex === -1) {
    return { pathPart: decodeURIComponent(rawUrl), fragment: null };
  }
  const pathPart = decodeURIComponent(rawUrl.slice(0, hashIndex));
  const fragment = rawUrl.slice(hashIndex + 1);
  return { pathPart, fragment: fragment.length === 0 ? null : fragment };
}
