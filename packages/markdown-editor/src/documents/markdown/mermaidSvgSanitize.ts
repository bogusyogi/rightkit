import DOMPurify from "dompurify";

const MERMAID_SVG_NS = "http://www.w3.org/2000/svg";
const MERMAID_XHTML_NS = "http://www.w3.org/1999/xhtml";
const MERMAID_LABEL_TAGS = [
  "div",
  "span",
  "p",
  "br",
  "math",
  "semantics",
  "mrow",
  "mi",
  "mo",
  "mn",
  "ms",
  "mtext",
  "mfrac",
  "msup",
  "msub",
  "msubsup",
  "msqrt",
  "mroot",
  "mstyle",
  "mspace",
  "mpadded",
  "mtable",
  "mtr",
  "mtd",
  "mlabeledtr",
  "mmultiscripts",
  "mprescripts",
  "none",
  "mover",
  "munder",
  "munderover",
  "menclose",
  "annotation",
] as const;
const MERMAID_LABEL_ATTRIBUTES = [
  "xmlns",
  "class",
  "style",
  "aria-hidden",
  "encoding",
  "display",
  "mathvariant",
  "mathsize",
  "stretchy",
  "fence",
  "separator",
  "accent",
  "accentunder",
  "bevelled",
  "largeop",
  "movablelimits",
  "linethickness",
  "rowalign",
  "columnalign",
  "columnlines",
  "rowlines",
  "columnspacing",
  "rowspacing",
  "scriptlevel",
  "displaystyle",
  "scriptminsize",
  "scriptsizemultiplier",
  "width",
  "height",
  "depth",
  "lspace",
  "voffset",
  "subscriptshift",
  "superscriptshift",
] as const;
const MERMAID_LABEL_STYLE_PROPERTIES = new Set([
  "display",
  "white-space",
  "line-height",
  "max-width",
  "min-width",
  "width",
  "height",
  "top",
  "bottom",
  "left",
  "right",
  "padding",
  "padding-top",
  "padding-right",
  "padding-bottom",
  "padding-left",
  "margin",
  "margin-top",
  "margin-right",
  "margin-bottom",
  "margin-left",
  "text-align",
  "vertical-align",
  "font-size",
  "border-bottom-width",
  "border-bottom-style",
]);

function safeMermaidStyleValue(property: string, value: string): string | null {
  const clean = value.trim();
  if (!clean || /url\s*\(|expression\s*\(|@import|[<>]/i.test(clean)) return null;
  if (property === "display") {
    return /^(?:block|inline(?:-block)?|table-cell|none)$/i.test(clean) ? clean : null;
  }
  if (property === "white-space") {
    return /^(?:normal|nowrap|pre|pre-wrap|pre-line)$/i.test(clean) ? clean : null;
  }
  if (property === "text-align") {
    return /^(?:left|center|right|justify|start|end)$/i.test(clean) ? clean : null;
  }
  if (property === "border-bottom-style") {
    return /^(?:none|solid|dashed|dotted)$/i.test(clean) ? clean : null;
  }
  if (property === "vertical-align") {
    return /^(?:baseline|sub|super|top|text-top|middle|bottom|text-bottom|-?(?:\d+(?:\.\d*)?|\.\d+)(?:px|em|rem|ex|%)?)$/i.test(
      clean,
    )
      ? clean
      : null;
  }
  return /^(?:normal|auto|-?(?:\d+(?:\.\d*)?|\.\d+)(?:px|em|rem|ex|ch|%|pt|vh|vw)?)$/i.test(clean)
    ? clean
    : null;
}

function sanitizeMermaidLabelStyle(style: string): string {
  return style
    .split(";")
    .map((declaration) => {
      const separator = declaration.indexOf(":");
      if (separator <= 0) return null;
      const property = declaration.slice(0, separator).trim().toLowerCase();
      if (!MERMAID_LABEL_STYLE_PROPERTIES.has(property)) return null;
      const value = safeMermaidStyleValue(property, declaration.slice(separator + 1));
      return value ? `${property}: ${value}` : null;
    })
    .filter((declaration): declaration is string => declaration !== null)
    .join("; ");
}

function sanitizeMermaidLabelHtml(html: string): string {
  const clean = DOMPurify.sanitize(`<div>${html}</div>`, {
    ALLOWED_TAGS: [...MERMAID_LABEL_TAGS],
    ALLOWED_ATTR: [...MERMAID_LABEL_ATTRIBUTES],
    ALLOW_DATA_ATTR: false,
  });
  const parsed = new DOMParser().parseFromString(clean, "text/html");
  for (const element of Array.from(parsed.querySelectorAll("[style]"))) {
    const style = sanitizeMermaidLabelStyle(element.getAttribute("style") ?? "");
    if (style) element.setAttribute("style", style);
    else element.removeAttribute("style");
  }
  const wrapper = parsed.body.firstElementChild;
  return wrapper ? wrapper.innerHTML : "";
}

function safeMermaidSvgLength(value: string): string | null {
  return /^-?(?:\d+(?:\.\d*)?|\.\d+)(?:px|%)?$/i.test(value.trim()) ? value.trim() : null;
}

/**
 * Keep Mermaid's XHTML foreignObject labels narrowly scoped so KaTeX MathML survives. The outer
 * SVG is still DOMPurify-sanitized; label HTML allows only text/layout tags, safe MathML, and a
 * small CSS subset with URL-bearing declarations rejected.
 */
function stripExternalMermaidSvgResources(svg: string): string {
  const document = new DOMParser().parseFromString(svg, "image/svg+xml");
  if (document.querySelector("parsererror")) return "";
  const unsafeCss = (value: string) =>
    /\\|@import/i.test(value) ||
    Array.from(value.matchAll(/url\s*\(([^)]*)\)/gi)).some(
      (match) => !/^['"]?#[a-z0-9_.:-]+['"]?$/i.test(match[1]?.trim() ?? ""),
    );
  for (const element of Array.from(document.querySelectorAll("*"))) {
    // XHTML labels have their own strict tag/attribute & per-declaration style sanitizer.
    if (element.closest("foreignObject")) continue;
    if (element.localName === "style" && unsafeCss(element.textContent ?? "")) {
      element.remove();
      continue;
    }
    for (const attr of Array.from(element.attributes)) {
      if (
        (attr.localName === "href" && !/^#[a-z0-9_.:-]+$/i.test(attr.value)) ||
        unsafeCss(attr.value)
      ) {
        element.removeAttributeNode(attr);
      }
    }
  }
  return new XMLSerializer().serializeToString(document.documentElement);
}

export function sanitizeMermaidSvg(svg: string): string {
  svg = stripExternalMermaidSvgResources(svg);
  if (!/<foreignObject\b/i.test(svg)) {
    return DOMPurify.sanitize(svg, {
      ADD_TAGS: ["style"],
      USE_PROFILES: { svg: true, svgFilters: true },
    });
  }
  const parser = new DOMParser();
  const document = parser.parseFromString(svg, "image/svg+xml");
  if (document.querySelector("parsererror") || !document.documentElement) {
    return DOMPurify.sanitize(svg, {
      ADD_TAGS: ["style"],
      USE_PROFILES: { svg: true, svgFilters: true },
    });
  }
  const serializer = new XMLSerializer();
  const labels = Array.from(document.getElementsByTagName("foreignObject")).map(
    (foreignObject, index) => {
      const inner = Array.from(foreignObject.childNodes)
        .map((child) => serializer.serializeToString(child))
        .join("");
      const attrs = ["x", "y", "width", "height"].flatMap((name) => {
        const value = foreignObject.getAttribute(name);
        const safe = value ? safeMermaidSvgLength(value) : null;
        return safe ? [[name, safe] as const] : [];
      });
      const placeholder = document.createElementNS(MERMAID_SVG_NS, "g");
      placeholder.setAttribute("data-vr-mermaid-label", String(index));
      foreignObject.replaceWith(placeholder);
      return { id: String(index), attrs, html: sanitizeMermaidLabelHtml(inner) };
    },
  );
  const outer = DOMPurify.sanitize(serializer.serializeToString(document.documentElement), {
    ADD_TAGS: ["style"],
    ADD_ATTR: ["data-vr-mermaid-label"],
    USE_PROFILES: { svg: true, svgFilters: true },
  });
  const cleanDocument = parser.parseFromString(outer, "image/svg+xml");
  if (cleanDocument.querySelector("parsererror") || !cleanDocument.documentElement) return outer;
  for (const label of labels) {
    const placeholder = cleanDocument.querySelector(`g[data-vr-mermaid-label="${label.id}"]`);
    if (!placeholder) continue;
    const foreignObject = cleanDocument.createElementNS(MERMAID_SVG_NS, "foreignObject");
    for (const [name, value] of label.attrs) foreignObject.setAttribute(name, value);
    const labelDocument = parser.parseFromString(`<body>${label.html}</body>`, "text/html");
    const labelRoot = labelDocument.body.firstElementChild;
    const wrapper = cleanDocument.createElementNS(MERMAID_XHTML_NS, "div");
    wrapper.setAttribute("xmlns", MERMAID_XHTML_NS);
    if (labelRoot) {
      for (const attribute of Array.from(labelRoot.attributes)) {
        if (
          attribute.name !== "xmlns" &&
          (MERMAID_LABEL_ATTRIBUTES as readonly string[]).includes(attribute.name)
        ) {
          wrapper.setAttribute(attribute.name, attribute.value);
        }
      }
      wrapper.innerHTML = labelRoot.innerHTML;
    }
    foreignObject.appendChild(wrapper);
    placeholder.replaceWith(foreignObject);
  }
  return serializer.serializeToString(cleanDocument.documentElement);
}
