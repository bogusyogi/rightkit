import { toast } from "@rightkit/platform-ui/headless";
import { readRelativeAsset } from "@rightkit/document-core/state/tauriBridge";

// Cap how many sibling assets one preview will inline, so a pathological page can't fan out into
// thousands of backend reads.
const MAX_INLINE = 250;
/** Aggregate cap on inlined base64 characters across one preview build (assets are also capped at
 *  8 MiB each in Rust). 250 assets x 8 MiB could otherwise build a ~2 GB srcdoc string. 64 MiB of
 *  base64 (~48 MiB of files) covers a page with dozens of full-size photos; past it the remaining
 *  assets stay unavailable and the user is told. */
export const MAX_INLINE_TOTAL_BASE64_CHARS = 64 * 1024 * 1024;
/** Concurrent asset reads in flight; bounds IPC fan-out and how much can overshoot the cap. */
export const INLINE_CONCURRENCY = 8;

/** The app's current color scheme (from <html data-theme>), so an iframe's empty document canvas
 *  matches the app instead of defaulting to white on mount. */
export function appColorScheme(): "dark" | "light" {
  if (typeof document === "undefined") return "light";
  return document.documentElement.getAttribute("data-theme") === "dark" ? "dark" : "light";
}

/** Boot document for a freshly-mounted preview iframe: a blank page painted the app's `--ink` so the
 *  iframe's default-white compositor surface doesn't flash before the real srcDoc commits. SSR-safe.
 *  Shared by the interactive (HtmlDocument) and Safe (HtmlViewer) preview panes. */
export function previewBootHtml(): string {
  const ink =
    typeof window === "undefined"
      ? "#f7f4ee"
      : getComputedStyle(document.documentElement).getPropertyValue("--ink").trim() || "#f7f4ee";
  return `<!doctype html><html><head><meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'none'; style-src 'unsafe-inline'"><meta name="viewport" content="width=device-width, initial-scale=1"></head><body style="margin:0;min-height:100vh;background:${ink}"></body></html>`;
}

/** A URL we can resolve to a local sibling file: relative, with no scheme (http:/data:/mailto:…),
 *  not protocol-relative (`//`), root-absolute (`/`), a fragment (`#`) or a bare query (`?`). */
export function isLocalRelative(url: string | null | undefined): boolean {
  if (!url) return false;
  const u = url.trim();
  if (!u) return false;
  if (/^[a-z][a-z0-9+.-]*:/i.test(u)) return false; // has a scheme
  if (u.startsWith("//") || u.startsWith("/") || u.startsWith("#") || u.startsWith("?"))
    return false;
  return true;
}

/** Directory portion of a relative path: "assets/site.css" → "assets"; "site.css" → "". */
export function dirOf(rel: string): string {
  const clean = rel.split(/[?#]/)[0];
  const i = clean.lastIndexOf("/");
  return i === -1 ? "" : clean.slice(0, i);
}

/** Join a base dir with a relative ref (the Rust side canonicalizes + containment-checks). */
export function joinRel(dir: string, rel: string): string {
  const r = rel.split(/[?#]/)[0]; // drop any ?query / #frag before hitting the filesystem
  return dir ? `${dir}/${r}` : r;
}

/** A granted site root for one HTML tab: the opaque protocol token plus the directory it covers. */
export interface SiteRootGrant {
  token: string;
  root: string;
  /** False when no project marker was found and the root fell back to the document's folder. */
  markerFound?: boolean;
}

/** Split an absolute path on either separator, dropping empty segments. */
function pathSegments(path: string): string[] {
  return path.split(/[\\/]+/).filter(Boolean);
}

/** Convert a web-root reference (`/styles.css`) into a path relative to the opened document.
 * Browsers resolve that URL against an HTTP origin; a local preview must resolve it against the
 * inferred site root instead. */
export function siteRootRelativeReadPath(
  ref: string,
  docPath: string,
  grant: SiteRootGrant,
): string | null {
  const value = ref.trim();
  if (!value.startsWith("/") || value.startsWith("//")) return null;
  const rootSegments = pathSegments(grant.root);
  const docSegments = pathSegments(docPath);
  if (docSegments.length <= rootSegments.length) return null;
  const caseFold = (segment: string) =>
    isWindowsStylePath(docPath) ? segment.toLowerCase() : segment;
  for (let i = 0; i < rootSegments.length; i += 1) {
    if (caseFold(rootSegments[i]) !== caseFold(docSegments[i])) return null;
  }
  const parentDepth = docSegments.length - rootSegments.length - 1;
  const withinRoot = value.replace(/^\/+/, "");
  return `${"../".repeat(parentDepth)}${withinRoot || "."}`;
}

function rewriteWebRootAttribute(
  element: Element,
  attribute: string,
  docPath: string,
  grant: SiteRootGrant,
): void {
  const value = element.getAttribute(attribute);
  const relative = value ? siteRootRelativeReadPath(value, docPath, grant) : null;
  if (relative) element.setAttribute(attribute, relative);
}

function rewriteWebRootSrcset(value: string, docPath: string, grant: SiteRootGrant): string {
  return value
    .split(",")
    .map((candidate) => {
      const match = candidate.match(/^(\s*)(\S+)(.*)$/s);
      if (!match) return candidate;
      const relative = siteRootRelativeReadPath(match[2], docPath, grant);
      return relative ? `${match[1]}${relative}${match[3]}` : candidate;
    })
    .join(",");
}

/** Keep web-root URLs inside opaque site-root grant. A custom-scheme `<base>` cannot preserve its
 * path token for `/asset` URLs because browser URL resolution resets to host root. */
export function rewriteWebRootResourceUrls(
  doc: Document,
  docPath: string,
  grant: SiteRootGrant,
): void {
  for (const element of Array.from(doc.querySelectorAll("[href]"))) {
    if (element.tagName.toLowerCase() === "base") continue;
    rewriteWebRootAttribute(element, "href", docPath, grant);
  }
  for (const element of Array.from(doc.querySelectorAll("[src]")))
    rewriteWebRootAttribute(element, "src", docPath, grant);
  for (const element of Array.from(doc.querySelectorAll("[poster]")))
    rewriteWebRootAttribute(element, "poster", docPath, grant);
  for (const element of Array.from(doc.querySelectorAll("[srcset]"))) {
    const srcset = element.getAttribute("srcset");
    if (srcset) element.setAttribute("srcset", rewriteWebRootSrcset(srcset, docPath, grant));
  }
}

/**
 * Build the `<base href>` for a document inside a granted site root.
 *
 * The base must point at the DOCUMENT'S OWN directory (with a trailing slash), not at the root:
 * that is what makes `src="a.png"` and `src="../b.png"` resolve exactly as they would in a browser
 * opening the same file. Returns null when the document is not actually inside the root, so a
 * mismatched grant degrades to the inlining path rather than producing a wrong base.
 */
export function siteBaseHref(docPath: string, grant: SiteRootGrant): string | null {
  const rootSegments = pathSegments(grant.root);
  const docSegments = pathSegments(docPath);
  if (docSegments.length <= rootSegments.length) return null;
  const caseFold = (s: string) => (isWindowsStylePath(docPath) ? s.toLowerCase() : s);
  for (let i = 0; i < rootSegments.length; i += 1) {
    if (caseFold(rootSegments[i]) !== caseFold(docSegments[i])) return null;
  }
  // Drop the file name: the base is a directory URL and must end in "/".
  const relDir = docSegments.slice(rootSegments.length, -1);
  const encoded = relDir.map((segment) => encodeURIComponent(segment)).join("/");
  return `viewright-site://localhost/${grant.token}/${encoded}${encoded ? "/" : ""}`;
}

function isWindowsStylePath(path: string): boolean {
  return /^[a-z]:[\\/]/i.test(path) || path.startsWith("\\\\");
}

/** Insert (or replace) the preview's `<base>` so the WebView resolves every relative subresource —
 *  `src`, `srcset`, `poster`, CSS `url()`, `@import`, `<track>`, fonts — through the site protocol,
 *  the same way a browser resolves them against a `file://` document URL. */
export function applySiteBase(doc: Document, baseHref: string): void {
  const head = doc.head ?? doc.documentElement;
  // A page-authored <base> would otherwise redirect our relative refs somewhere we did not grant.
  for (const existing of Array.from(doc.querySelectorAll("base"))) existing.remove();
  const base = doc.createElement("base");
  base.setAttribute("href", baseHref);
  head.insertBefore(base, head.firstChild);
}

function decodeBase64Utf8(b64: string): string {
  const bin = atob(b64);
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

interface Budget {
  n: number;
  bytes: number;
  exceeded: boolean;
}

/** Read one asset against the aggregate budget. Once the cap is hit every later read short-circuits
 *  to null without touching IPC (the cancellation), and the over-cap asset itself is discarded. */
async function readBudgeted(
  budget: Budget,
  docPath: string,
  rel: string,
  rootPath?: string,
  siteToken?: string,
): Promise<{ mime: string; base64: string } | null> {
  if (budget.exceeded) return null;
  const asset = await readRelativeAsset(docPath, rel, rootPath, siteToken);
  if (!asset || budget.exceeded) return null;
  budget.bytes += asset.base64.length;
  if (budget.bytes > MAX_INLINE_TOTAL_BASE64_CHARS) {
    budget.exceeded = true;
    return null;
  }
  return asset;
}

/** Map with bounded concurrency; stops dispatching new items once the budget is exceeded. */
async function mapBudgeted<T, R>(
  items: T[],
  budget: Budget,
  fn: (item: T) => Promise<R>,
): Promise<(R | undefined)[]> {
  const out: (R | undefined)[] = new Array(items.length);
  let next = 0;
  const worker = async () => {
    while (next < items.length && !budget.exceeded) {
      const i = next++;
      out[i] = await fn(items[i]);
    }
  };
  await Promise.all(Array.from({ length: Math.min(INLINE_CONCURRENCY, items.length) }, worker));
  return out;
}

function escapeSvgText(text: string): string {
  return text.replace(/[&<>"']/g, (c) => {
    switch (c) {
      case "&":
        return "&amp;";
      case "<":
        return "&lt;";
      case ">":
        return "&gt;";
      case '"':
        return "&quot;";
      case "'":
        return "&apos;";
      default:
        return c;
    }
  });
}

/** Placeholder for an image the inlining fallback could not read. Deliberately says "unavailable",
 *  not "not found": the read can also fail because the file is outside the granted root or exceeds
 *  the inline cap, and asserting the file is missing sent people looking for the wrong bug. */
function brokenImageDataUri(src: string, alt: string | null): string {
  const label = (alt || src.split(/[\\/]/).pop() || "missing image").slice(0, 48);
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="320" height="96" viewBox="0 0 320 96" role="img" aria-label="Missing image">
  <rect width="319" height="95" x=".5" y=".5" fill="#f8f8f8" stroke="#cfcfcf"/>
  <path d="M22 58l20-22 16 17 11-12 31 33H22z" fill="#d7d7d7"/>
  <circle cx="72" cy="31" r="8" fill="#d7d7d7"/>
  <text x="116" y="42" fill="#555" font-family="system-ui, -apple-system, Segoe UI, sans-serif" font-size="15" font-weight="700">Image unavailable</text>
  <text x="116" y="64" fill="#777" font-family="system-ui, -apple-system, Segoe UI, sans-serif" font-size="12">${escapeSvgText(label)}</text>
</svg>`;
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

// Rewrite `url(...)` references inside a CSS string to inlined `data:` URIs. `baseDir` is the CSS
// file's own directory (so its relative urls resolve correctly against the document folder).
async function inlineCssUrls(
  css: string,
  baseDir: string,
  docPath: string,
  budget: Budget,
  rootPath?: string,
  siteToken?: string,
  siteRoot?: SiteRootGrant | null,
): Promise<string> {
  const urlRe = /url\(\s*(['"]?)([^'")]+)\1\s*\)/gi;
  const seen = new Map<string, string>(); // raw ref → data URI (dedupe repeated refs)
  // Sync pass: decide, in document order, which unique refs are within the remaining shared
  // budget — identical decision to the old sequential loop. The fetch pass runs them concurrently; only
  // the IPC round-trips are parallelized, not the budget/dedupe bookkeeping.
  const toFetch: { raw: string; readPath: string }[] = [];
  const queued = new Set<string>();
  for (const m of [...css.matchAll(urlRe)]) {
    if (budget.n + toFetch.length >= MAX_INLINE) break;
    const raw = m[2].trim();
    const readPath = isLocalRelative(raw)
      ? joinRel(baseDir, raw)
      : siteRoot
        ? siteRootRelativeReadPath(raw, docPath, siteRoot)
        : null;
    if (queued.has(raw) || !readPath) continue;
    queued.add(raw);
    toFetch.push({ raw, readPath });
  }
  budget.n += toFetch.length;
  await mapBudgeted(toFetch, budget, async ({ raw, readPath }) => {
    const asset = await readBudgeted(budget, docPath, readPath, rootPath, siteToken);
    if (asset) seen.set(raw, `data:${asset.mime};base64,${asset.base64}`);
  });
  if (seen.size === 0) return css;
  return css.replace(urlRe, (whole, _q, ref) => {
    const data = seen.get(String(ref).trim());
    return data ? `url("${data}")` : whole;
  });
}

/**
 * Make an opened HTML document mostly self-contained for the sandboxed preview: inline local
 * `<img src>` and `url()` assets as `data:` URIs and fold external local stylesheets into `<style>`
 * (resolving their own `url()`s). Remote images/styles/fonts/scripts and network are left alone and
 * allowed by the interactive preview CSP (browser parity); Rust still refuses local reads outside the
 * granted root, and the opaque-origin sandbox keeps remote code from reaching the app or OS.
 *
 * Returns the raw HTML unchanged when there is no docPath (browser/dev) or nothing to inline.
 */
export async function buildPreviewHtml(
  rawHtml: string,
  docPath?: string,
  rootPath?: string,
  siteRoot?: SiteRootGrant | null,
): Promise<string> {
  if (!docPath || typeof DOMParser === "undefined") return rawHtml;
  const budget: Budget = { n: 0, bytes: 0, exceeded: false };
  const doc = new DOMParser().parseFromString(rawHtml, "text/html");
  for (const image of Array.from(doc.querySelectorAll("img[src]"))) {
    image.setAttribute("data-vr-authored-src", image.getAttribute("src") ?? "");
  }

  // A granted site root gets a <base>, so the WebView resolves everything attribute rewriting
  // cannot reach — `srcset`, `poster`, `@import`, inline `style="url()"`, `<track>`, fonts — and
  // does it for files above the document's own folder, with no size ceiling and with HTTP ranges
  // so video seeks.
  //
  // Inlining still runs underneath it, deliberately. The two layers are complementary, not
  // redundant: whichever refs inlining rewrites become `data:` URIs and never hit the protocol,
  // and everything it leaves alone falls through to the base. That keeps images working even if
  // the protocol is unavailable, which matters because a preview whose every asset 404s is a far
  // worse failure than one that merely misses `srcset` variants.
  const baseHref = siteRoot ? siteBaseHref(docPath, siteRoot) : null;
  if (baseHref && siteRoot) {
    applySiteBase(doc, baseHref);
    rewriteWebRootResourceUrls(doc, docPath, siteRoot);
  }
  const siteToken = baseHref ? siteRoot?.token : undefined;

  // Each eligible <img> consumes its own budget unit (no dedupe — matches the original loop, which
  // never deduped by src). The sync pass decides which elements are in-budget (document
  // order, identical to the sequential version); the fetch pass runs them concurrently and mutates each
  // element independently, so execution order can't change the result.
  {
    const jobs: { el: Element; src: string; readPath: string }[] = [];
    for (const img of Array.from(doc.querySelectorAll("img[src]"))) {
      if (budget.n + jobs.length >= MAX_INLINE) break;
      const src = img.getAttribute("src");
      const readPath = src
        ? isLocalRelative(src)
          ? src
          : siteRoot
            ? siteRootRelativeReadPath(src, docPath, siteRoot)
            : null
        : null;
      if (!src || !readPath) continue;
      jobs.push({ el: img, src, readPath });
    }
    budget.n += jobs.length;
    await mapBudgeted(jobs, budget, async ({ el, src, readPath }) => {
        const asset = await readBudgeted(budget, docPath, readPath, rootPath, siteToken);
        if (asset) el.setAttribute("src", `data:${asset.mime};base64,${asset.base64}`);
        else if (!baseHref) {
          // Only claim failure when nothing else can resolve this ref. With a base in place the
          // original relative src is still live, so overwriting it with a placeholder would
          // manufacture a broken image out of one the WebView could have loaded.
          el.setAttribute("src", brokenImageDataUri(src, el.getAttribute("alt")));
          el.setAttribute("data-vr-missing-src", src);
        }
    });
  }

  // Media is streamed, never inlined, whenever a base exists: base64 in a `data:` URI cannot be
  // range-requested (so video will not seek) and the whole file must sit in memory at once, which
  // is exactly what the 8 MiB inline cap was rejecting.
  {
    const jobs: { el: Element; src: string }[] = [];
    const mediaEls = baseHref
      ? []
      : Array.from(doc.querySelectorAll("audio[src], video[src], source[src]"));
    for (const media of mediaEls) {
      if (budget.n + jobs.length >= MAX_INLINE) break;
      const src = media.getAttribute("src");
      if (!src || !isLocalRelative(src)) continue;
      jobs.push({ el: media, src });
    }
    budget.n += jobs.length;
    await mapBudgeted(jobs, budget, async ({ el, src }) => {
      const asset = await readBudgeted(budget, docPath, joinRel("", src), rootPath, siteToken);
      if (asset) el.setAttribute("src", `data:${asset.mime};base64,${asset.base64}`);
    });
  }

  // Stylesheets: phase 1 decides (sync, document order) which <link>s are in-budget for their OWN
  // fetch; phase 2 fetches every stylesheet's raw bytes concurrently (the dominant IPC cost this fix
  // targets); phase 3 resolves each stylesheet's nested url()s IN DOCUMENT ORDER so the shared budget
  // is consumed identically to the fully-sequential version (nested fetches within one stylesheet
  // still run concurrently, via the now-parallel inlineCssUrls).
  {
    const jobs: { el: Element; href: string; readPath: string }[] = [];
    for (const link of Array.from(doc.querySelectorAll('link[rel~="stylesheet"][href]'))) {
      if (budget.n + jobs.length >= MAX_INLINE) break;
      const href = link.getAttribute("href");
      const readPath = href
        ? isLocalRelative(href)
          ? href
          : siteRoot
            ? siteRootRelativeReadPath(href, docPath, siteRoot)
            : null
        : null;
      if (!href || !readPath) continue;
      jobs.push({ el: link, href, readPath });
    }
    budget.n += jobs.length;
    const fetchedRaw = await mapBudgeted(jobs, budget, async (job) => ({
      ...job,
      asset: await readBudgeted(budget, docPath, job.readPath, rootPath, siteToken),
    }));
    for (const entry of fetchedRaw) {
      if (!entry) continue; // never dispatched: cap hit first, so the <link> is left as authored
      const { el, readPath, asset } = entry;
      if (!asset) {
        // Same reasoning as images: with a base, the <link> can still resolve through the protocol,
        // so removing it would strip styling the WebView was about to load.
        if (!baseHref) el.remove();
        continue;
      }
      let css = decodeBase64Utf8(asset.base64);
      css = await inlineCssUrls(
        css,
        dirOf(readPath),
        docPath,
        budget,
        rootPath,
        siteToken,
        siteRoot,
      );
      const style = doc.createElement("style");
      style.setAttribute("data-vr-inlined", "");
      style.textContent = css;
      el.replaceWith(style);
    }
  }

  // Resolve url()s inside the page's own inline <style> blocks too (skip the ones we just created).
  for (const st of Array.from(doc.querySelectorAll("style"))) {
    if (st.hasAttribute("data-vr-inlined") || !st.textContent) continue;
    st.textContent = await inlineCssUrls(
      st.textContent,
      "",
      docPath,
      budget,
      rootPath,
      siteToken,
      siteRoot,
    );
  }

  if (budget.exceeded) {
    try {
      toast(
        "This page's local assets exceed the preview size budget; remaining images and styles were not loaded.",
        "error",
      );
    } catch {
      /* toast host unavailable (tests / early boot) */
    }
  }
  return `<!doctype html>${doc.documentElement.outerHTML}`;
}
