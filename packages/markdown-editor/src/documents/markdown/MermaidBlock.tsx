import { useContext, useEffect, useRef, useState } from "react";
import { sanitizeMermaidSvg } from "./mermaidSvgSanitize.js";
import { MarkdownRenderTrackerContext } from "./export/MarkdownRenderTracker.js";
import { acquireMarkdownResource, createMarkdownWork } from "./markdownResourceLifecycle.js";
import {
  isMarkdownDocumentActive,
  subscribeMarkdownDocumentActivity,
} from "./markdownDocumentActivity.js";
import { hostAppName } from "../../appName.js";

// Lazy-load mermaid; render with strict security (no script/HTML injection from diagram source).
let mermaidPromise: Promise<typeof import("mermaid").default> | null = null;
function getMermaid() {
  if (!mermaidPromise) {
    mermaidPromise = import("mermaid").then((m) => m.default);
  }
  return mermaidPromise;
}

let counter = 0;
let mermaidRenderQueue: Promise<void> = Promise.resolve();

interface MermaidTheme {
  key: string;
  dark: boolean;
  background: string;
  panel: string;
  text: string;
  border: string;
  accent: string;
  fontFamily: string;
}

function cssToken(styles: CSSStyleDeclaration, name: string, fallback: string): string {
  return styles.getPropertyValue(name).trim() || fallback;
}

function readMermaidTheme(): MermaidTheme {
  const root = document.documentElement;
  const styles = getComputedStyle(root);
  const theme = {
    dark: root.getAttribute("data-theme") === "dark",
    background: cssToken(styles, "--reader-surface", "#f7f3ec"),
    panel: cssToken(styles, "--panel", "#fffdf8"),
    text: cssToken(styles, "--paper", "#211c18"),
    border: cssToken(styles, "--line", "#e2dad1"),
    accent: cssToken(styles, "--copper", "#0c6aa5"),
    fontFamily: cssToken(styles, "--doc-font", "Sentient, Georgia, serif"),
  };
  return { ...theme, key: JSON.stringify(theme) };
}

function useMermaidTheme(): MermaidTheme {
  const [theme, setTheme] = useState(readMermaidTheme);

  useEffect(() => {
    const root = document.documentElement;
    const refresh = () => {
      const next = readMermaidTheme();
      setTheme((current) => (current.key === next.key ? current : next));
    };
    const observer = new MutationObserver(refresh);
    const release = acquireMarkdownResource("observer");
    observer.observe(root, { attributes: true, attributeFilter: ["data-theme", "style"] });
    refresh();
    return () => {
      observer.disconnect();
      release();
    };
  }, []);

  return theme;
}

function renderMermaid(code: string, theme: MermaidTheme, isCancelled: () => boolean) {
  const task = mermaidRenderQueue.then(async () => {
    if (isCancelled()) throw new Error("Mermaid render cancelled.");
    const mermaid = await getMermaid();
    if (isCancelled()) throw new Error("Mermaid render cancelled.");
    // Mermaid configuration is global. Keep initialize + render in one queue so concurrently mounted
    // diagrams cannot borrow another tab's theme between those two calls.
    mermaid.initialize({
      startOnLoad: false,
      securityLevel: "strict",
      theme: "base",
      darkMode: theme.dark,
      // Mermaid's math labels require XHTML foreignObject output so its bundled KaTeX can emit
      // MathML. sanitizeMermaidSvg narrows that XHTML surface before it reaches the DOM.
      htmlLabels: true,
      flowchart: { htmlLabels: true, padding: 18, useMaxWidth: false },
      themeVariables: {
        darkMode: theme.dark,
        background: theme.background,
        primaryColor: theme.panel,
        primaryTextColor: theme.text,
        primaryBorderColor: theme.border,
        lineColor: theme.accent,
        secondaryColor: theme.background,
        tertiaryColor: theme.panel,
        textColor: theme.text,
        titleColor: theme.text,
        edgeLabelBackground: theme.background,
        noteBkgColor: theme.panel,
        noteTextColor: theme.text,
        noteBorderColor: theme.border,
        clusterBkg: theme.panel,
        clusterBorder: theme.border,
        fontFamily: theme.fontFamily,
      },
      themeCSS: `.label text, .nodeLabel, .edgeLabel { color: ${theme.text}; fill: ${theme.text}; } .edgePath .path { stroke: ${theme.accent}; }`,
    });
    if (isCancelled()) throw new Error("Mermaid render cancelled.");
    return mermaid.render(`mmd-${counter++}`, code);
  });
  mermaidRenderQueue = task.then(
    () => undefined,
    () => undefined,
  );
  return task;
}

type SvgBounds = { x: number; y: number; width: number; height: number };
const MAX_MERMAID_SVG_DIMENSION = 100_000;

function parseViewBoxBounds(svg: SVGSVGElement): SvgBounds | null {
  const parts = (svg.getAttribute("viewBox") ?? "")
    .trim()
    .split(/[\s,]+/)
    .map(Number);
  if (
    parts.length !== 4 ||
    parts.some((part) => !Number.isFinite(part) || Math.abs(part) > MAX_MERMAID_SVG_DIMENSION)
  )
    return null;
  const [x, y, width, height] = parts;
  return width > 0 &&
    height > 0 &&
    width <= MAX_MERMAID_SVG_DIMENSION &&
    height <= MAX_MERMAID_SVG_DIMENSION
    ? { x, y, width, height }
    : null;
}

function applySvgBox(
  svg: SVGSVGElement,
  bounds: SvgBounds,
  pad: number,
  mode: "measured" | "fallback",
) {
  const x = Math.floor(bounds.x - pad);
  const y = Math.floor(bounds.y - pad);
  const width = Math.ceil(bounds.width + pad * 2);
  const height = Math.ceil(bounds.height + pad * 2);
  svg.setAttribute("viewBox", `${x} ${y} ${width} ${height}`);
  svg.setAttribute("width", String(width));
  svg.setAttribute("height", String(height));
  svg.setAttribute("preserveAspectRatio", "xMidYMid meet");
  svg.setAttribute("data-vr-mermaid-layout", mode);
  svg.style.maxWidth = "none";
  svg.style.overflow = "visible";
}

function fitMermaidSvg(svg: SVGSVGElement) {
  svg.style.maxWidth = "none";
  svg.style.overflow = "visible";
  const fallback = parseViewBoxBounds(svg);
  try {
    const bb = svg.getBBox();
    if (bb.width > 0 && bb.height > 0) {
      applySvgBox(svg, bb, 18, "measured");
      return;
    }
  } catch {
    // Some browser/test layouts cannot synchronously measure an injected SVG.
  }
  if (fallback) {
    applySvgBox(svg, fallback, 0, "fallback");
    return;
  }
  svg.setAttribute("data-vr-mermaid-layout", "unmeasured");
  if (svg.getAttribute("data-vr-mermaid-warned") !== "true") {
    svg.setAttribute("data-vr-mermaid-warned", "true");
    console.warn(
      `${hostAppName()} could not measure Mermaid SVG bounds; leaving sanitized SVG dimensions intact.`,
    );
  }
}

/** ```mermaid fenced blocks → rendered diagram. SVG is sanitized before injection. */
export function MermaidBlock({ code, documentPath }: { code: string; documentPath?: string }) {
  const tracker = useContext(MarkdownRenderTrackerContext);
  const [svg, setSvg] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const theme = useMermaidTheme();
  const [active, setActive] = useState(() => isMarkdownDocumentActive(documentPath));
  const completedKeyRef = useRef<string | null>(null);

  useEffect(() => {
    const sync = () => setActive(isMarkdownDocumentActive(documentPath));
    sync();
    return subscribeMarkdownDocumentActivity(documentPath, sync);
  }, [documentPath]);

  useEffect(() => acquireMarkdownResource("mermaid"), []);

  useEffect(() => {
    if (!active) return;
    const renderKey = `${code}\u0000${theme.key}`;
    if (completedKeyRef.current === renderKey) return;
    const finish = tracker?.begin("Mermaid");
    const work = createMarkdownWork();
    setSvg(null);
    setErr(null);
    renderMermaid(code, theme, work.isCancelled)
      .then(({ svg }) => {
        // Keep the <style> block Mermaid injects (node fills, stroke, label colors) — without it
        // the SVG profile strips it and every node renders as a solid black box. Safe: the SVG is
        // our own mermaid output rendered with securityLevel:"strict" (no diagram-sourced HTML/JS).
        if (!work.isCancelled()) {
          const clean = sanitizeMermaidSvg(svg);
          setSvg(clean);
          completedKeyRef.current = renderKey;
        }
      })
      .catch((e) => {
        if (!work.isCancelled()) {
          tracker?.fail("Mermaid", e);
          setErr(String(e?.message ?? e));
        }
      })
      .finally(() => {
        work.complete();
        finish?.();
      });
    return () => {
      work.cancel();
      finish?.();
    };
  }, [active, code, theme, tracker]);

  // Mermaid can emit bounds that are too tight for its own labels/markers. After the sanitized SVG
  // is in the DOM, re-measure the rendered content and give the SVG a natural pixel box inside the
  // scrollable wrapper. That matches Typora's figure/panel behavior for wide LR diagrams: preserve
  // the diagram instead of squeezing it until labels or arrowheads clip.
  useEffect(() => {
    if (!svg) return;
    const el = hostRef.current?.querySelector("svg");
    if (!el) return;
    fitMermaidSvg(el);
    const frame = requestAnimationFrame(() => fitMermaidSvg(el));
    return () => cancelAnimationFrame(frame);
  }, [svg]);

  if (err) {
    return (
      <pre
        data-vr-export-state="error"
        role="alert"
        className="my-4 overflow-auto rounded border border-line bg-panel p-3 font-mono text-xs text-danger"
      >
        Diagram could not render: {err}
        {"\n\n"}
        {code}
      </pre>
    );
  }
  if (svg == null) {
    return (
      <div data-vr-export-state="loading" className="my-4 font-mono text-xs text-muted-2">
        rendering diagram…
      </div>
    );
  }
  // The wrapper owns horizontal overflow; the inner SVG keeps its natural measured size.
  return (
    <div
      ref={hostRef}
      data-vr-export-state="ready"
      className="vr-mermaid my-4 overflow-auto rounded-lg border border-line bg-[var(--reader-surface)] p-4"
      // biome-ignore lint/security/noDangerouslySetInnerHtml: Mermaid returns SVG markup and the app injects it only into this isolated diagram host.
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}
