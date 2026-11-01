export const MARKDOWN_EXPORT_STYLE_PROPERTIES = [
  "align-items",
  "background-color",
  "border-bottom-color",
  "border-bottom-style",
  "border-bottom-width",
  "border-left-color",
  "border-left-style",
  "border-left-width",
  "border-radius",
  "border-right-color",
  "border-right-style",
  "border-right-width",
  "border-top-color",
  "border-top-style",
  "border-top-width",
  "box-shadow",
  "color",
  "display",
  "flex-direction",
  "font-family",
  "font-size",
  "font-style",
  "font-weight",
  "gap",
  "grid-template-columns",
  "height",
  "justify-content",
  "letter-spacing",
  "line-height",
  "list-style-position",
  "list-style-type",
  "margin-bottom",
  "margin-left",
  "margin-right",
  "margin-top",
  "max-width",
  "min-width",
  "opacity",
  "overflow-wrap",
  "padding-bottom",
  "padding-left",
  "padding-right",
  "padding-top",
  "text-align",
  "text-decoration-color",
  "text-decoration-line",
  "text-decoration-style",
  "text-transform",
  "vertical-align",
  "white-space",
  "width",
  "word-break",
] as const;

export interface ExportDomResult {
  fragment: string;
  warnings: readonly string[];
}

const isRemote = (src: string) => /^https?:\/\//i.test(src);
const isAllowedClass = (name: string) => /^(katex|katex-|shiki|language-|mermaid)/.test(name);

export function snapshotMarkdownDom(
  liveArticle: HTMLElement,
  options: { remoteImages: "preserve" | "omit" },
): ExportDomResult {
  const clone = liveArticle.cloneNode(true) as HTMLElement;
  const warnings: string[] = [];

  const liveElements = [liveArticle, ...liveArticle.querySelectorAll<HTMLElement>("*")];
  const originalCloneElements = [clone, ...clone.querySelectorAll<HTMLElement>("*")];
  const liveByClone = new WeakMap<Element, HTMLElement>();
  originalCloneElements.forEach((element, index) => {
    const live = liveElements[index];
    if (live) liveByClone.set(element, live);
  });

  clone
    .querySelectorAll(
      "[data-vr-export-ignore],.no-print,script,style,template,button,select,textarea",
    )
    .forEach((node) => node.remove());
  clone.querySelectorAll("input[type=checkbox]").forEach((node) => {
    const input = node as HTMLInputElement;
    const replacement = document.createElement("span");
    replacement.textContent = input.checked ? "☑" : "☐";
    replacement.setAttribute("role", "img");
    replacement.setAttribute("aria-label", input.checked ? "Completed task" : "Incomplete task");
    input.replaceWith(replacement);
  });
  clone.querySelectorAll("details").forEach((node) => node.setAttribute("open", ""));

  for (const target of [clone, ...clone.querySelectorAll<HTMLElement>("*")]) {
    const live = liveByClone.get(target);
    if (!target || !live) continue;
    for (const attribute of [...target.attributes]) {
      const name = attribute.name.toLowerCase();
      if (
        name.startsWith("on") ||
        name === "contenteditable" ||
        name === "spellcheck" ||
        name === "tabindex" ||
        name.startsWith("data-vr-")
      ) {
        target.removeAttribute(attribute.name);
      }
    }
    if (target instanceof SVGElement || target.closest("svg")) continue;
    const styles = getComputedStyle(live);
    const inline: string[] = [];
    for (const property of MARKDOWN_EXPORT_STYLE_PROPERTIES) {
      const value = styles.getPropertyValue(property).trim();
      if (value) inline.push(`${property}:${value}`);
    }
    if (inline.length) target.setAttribute("style", inline.join(";"));
    const allowed = [...target.classList].filter(isAllowedClass);
    if (allowed.length) target.className = allowed.join(" ");
    else target.removeAttribute("class");
  }

  clone.querySelectorAll("img").forEach((node) => {
    const src = node.getAttribute("src") ?? "";
    if (src.startsWith("data:") || src.startsWith("blob:")) return;
    if (isRemote(src)) {
      warnings.push(`Remote image retained: ${src}`);
      if (options.remoteImages === "preserve") return;
      const replacement = document.createElement("span");
      replacement.textContent = `[Image omitted: ${node.getAttribute("alt") || "image"}]`;
      node.replaceWith(replacement);
      return;
    }
    throw new Error(`Markdown export contains unresolved local image: ${src || "<empty>"}`);
  });

  clone.querySelectorAll("svg").forEach((svg) => {
    if (!svg.querySelector("title")) {
      const title = document.createElementNS("http://www.w3.org/2000/svg", "title");
      title.textContent = "Diagram";
      svg.prepend(title);
    }
  });
  clone.querySelectorAll(".katex-html").forEach((node) => node.setAttribute("aria-hidden", "true"));
  clone.style.maxWidth = "60rem";
  clone.style.margin = "0 auto";
  clone.style.padding = "2rem 1.25rem";
  clone.style.animation = "none";
  clone.style.transition = "none";
  return { fragment: clone.outerHTML, warnings };
}
