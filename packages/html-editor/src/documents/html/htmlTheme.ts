function cssVar(name: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

export function htmlThemeStyleCss(): string {
  const line = cssVar("--line", "#3a372f");
  const copper = cssVar("--copper", "#c46a3d");
  const paper = cssVar("--paper", "#faf8f4");
  const panel = cssVar("--panel", "#161513");

  return `
    *{scrollbar-width:thin;scrollbar-color:${line} ${panel}}
    *::-webkit-scrollbar{width:10px;height:10px}
    *::-webkit-scrollbar-track{background:${panel}}
    *::-webkit-scrollbar-thumb{background:${line};border-radius:8px;border:2px solid ${panel};background-clip:content-box}
    *::-webkit-scrollbar-thumb:hover{background:${copper};background-clip:content-box}
    *::-webkit-scrollbar-corner{background:${panel}}
    .vr-find-span{background-color:color-mix(in srgb, ${copper} 40%, transparent);border-radius:2px}
    .vr-find-span-active{background-color:${copper};color:${paper};border-radius:2px}
    ::highlight(search-match){background-color:color-mix(in srgb, ${copper} 40%, transparent)}
    ::highlight(search-match-active){background-color:${copper};color:${paper}}
  `;
}

export function htmlThemeStyleHtml(): string {
  return `<style data-vr-theme-style>${htmlThemeStyleCss()}</style>`;
}

export function applyHtmlThemeStyle(
  doc: Document | null | undefined,
  css = htmlThemeStyleCss(),
): void {
  if (!doc?.head) return;
  let style = doc.head.querySelector<HTMLStyleElement>("style[data-vr-theme-style]");
  if (!style) {
    style = doc.createElement("style");
    style.setAttribute("data-vr-theme-style", "");
    doc.head.appendChild(style);
  }
  style.textContent = css;
}
