export function detectBlankDocxRender(root: HTMLElement | null, sourceText: string): string | null {
  const text = sourceText.trim();
  if (!text) return null;
  if (containsCjk(text)) return text;
  const pages = root?.querySelectorAll(".layout-page");
  if (!pages || pages.length === 0) return null;
  const rendered = Array.from(pages)
    .map((page) => page.textContent ?? "")
    .join("")
    .trim();
  return rendered ? null : text;
}

function containsCjk(text: string): boolean {
  return /[\u3400-\u9FFF\uF900-\uFAFF]/u.test(text);
}
