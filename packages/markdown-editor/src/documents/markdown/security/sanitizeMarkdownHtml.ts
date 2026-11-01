import { sanitizeMarkdownHtml as sanitizeRightKitHtml } from "@rightkit/doc-render";

/** Marker attribute carrying a brokered local image path; only this module may set it. */
export const LOCAL_IMAGE_ATTR = "data-vr-local-src";

/** Private scheme the sanitizer's `resolveImageSrc` hook stamps on relative image sources. An
 * authored value with this scheme never survives the sanitizer's own URL policy. */
const LOCAL_IMAGE_SCHEME = "viewright-local:";

function brokerLocalImage(src: string): string | undefined {
  // Only plain document-relative paths are brokered through readRelativeAsset.
  if (src.startsWith("/") || src.includes("\\")) return undefined;
  return `${LOCAL_IMAGE_SCHEME}${src}`;
}

/**
 * Sanitize author HTML embedded in Markdown for the Safe HTML preview. Output never
 * replaces source. RightKit owns the tag/attribute/URL policy; the host app only turns relative
 * image sources into `data-vr-local-src` (no `src`) and removes every other image.
 */
export function sanitizeMarkdownHtml(raw: string): string {
  const clean = sanitizeRightKitHtml(raw, {
    allowRelative: true,
    allowTel: true,
    resolveImageSrc: brokerLocalImage,
  });
  const template = document.createElement("template");
  template.innerHTML = clean;
  for (const image of Array.from(template.content.querySelectorAll("img"))) {
    const src = image.getAttribute("src");
    if (src?.startsWith(LOCAL_IMAGE_SCHEME)) {
      image.setAttribute(LOCAL_IMAGE_ATTR, src.slice(LOCAL_IMAGE_SCHEME.length).trim());
      image.removeAttribute("src");
    } else {
      image.remove();
    }
  }
  return template.innerHTML;
}
