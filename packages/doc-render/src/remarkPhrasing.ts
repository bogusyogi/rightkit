import type { Root } from "mdast";

type Point = { line: number; column: number; offset?: number };
type AnyNode = { type: string; value?: string; url?: string; children?: AnyNode[]; data?: Record<string, unknown>; position?: { start?: Point; end?: Point } };

const HIGHLIGHT = /==(?=\S)([^=\n]+?)(?<=\S)==/g;
const LITERAL_CONTAINERS = new Set(["code", "inlineCode", "math", "inlineMath", "html", "yaml", "toml"]);

function walk(node: AnyNode, visit: (parent: AnyNode) => void): void {
  if (LITERAL_CONTAINERS.has(node.type) || !node.children) return;
  visit(node);
  for (const child of node.children) walk(child, visit);
}

/** The point `index` characters into `text`, which starts at `origin` in the source. */
function pointAt(origin: Point, text: string, index: number): Point {
  const before = text.slice(0, index);
  const newlines = before.split("\n").length - 1;
  const offset = origin.offset === undefined ? undefined : origin.offset + index;
  if (newlines === 0) return { line: origin.line, column: origin.column + index, offset };
  return { line: origin.line + newlines, column: index - before.lastIndexOf("\n"), offset };
}

/** `==text==` becomes a `highlight` node (rendered as `<mark>`). Plain text only: the marker pair
 * must sit inside one text node, so it never crosses emphasis, links or code. Every node the pass
 * makes from a positioned text node carries its source position (#812): the highlight node spans
 * the markers too, its text child spans the text between them. */
export function remarkHighlight() {
  return (tree: Root) => {
    walk(tree as AnyNode, (parent) => {
      if (parent.type === "link" || parent.type === "highlight") return;
      const next: AnyNode[] = [];
      for (const child of parent.children ?? []) {
        if (child.type !== "text" || !child.value?.includes("==")) { next.push(child); continue; }
        const value = child.value;
        const origin = child.position?.start;
        const span = (from: number, to: number): { start: Point; end: Point } | undefined =>
          origin ? { start: pointAt(origin, value, from), end: pointAt(origin, value, to) } : undefined;
        const text = (from: number, to: number): AnyNode => {
          const position = span(from, to);
          return position ? { type: "text", value: value.slice(from, to), position } : { type: "text", value: value.slice(from, to) };
        };
        let last = 0;
        for (const match of value.matchAll(HIGHLIGHT)) {
          const index = match.index ?? 0;
          if (index > last) next.push(text(last, index));
          const highlight: AnyNode = { type: "highlight", children: [{ type: "text", value: match[1] }] };
          const whole = span(index, index + match[0].length);
          const inner = span(index + 2, index + 2 + match[1].length);
          if (whole) highlight.position = whole;
          if (inner && highlight.children?.[0]) highlight.children[0].position = inner;
          next.push(highlight);
          last = index + match[0].length;
        }
        if (last === 0) next.push(child);
        else if (last < value.length) next.push(text(last, value.length));
      }
      parent.children = next;
    });
  };
}

/** Single newlines inside a paragraph become hard `break` nodes instead of collapsing. */
export function remarkPreserveSoftBreaks() {
  return (tree: Root) => {
    walk(tree as AnyNode, (parent) => {
      const next: AnyNode[] = [];
      for (const child of parent.children ?? []) {
        if (child.type !== "text" || !child.value?.includes("\n")) { next.push(child); continue; }
        child.value.split("\n").forEach((part, index) => {
          if (index > 0) next.push({ type: "break" });
          if (part) next.push({ type: "text", value: part });
        });
      }
      parent.children = next;
    });
  };
}

/** Flags bare (literal) autolinks with `data.autolink = true`; an explicit `[x](mailto:...)` or
 * `<a@b.c>` link keeps no flag. `keepEmail: false` turns bare email autolinks into plain text. */
export function remarkAutolinkFlag(options: { keepEmail?: boolean } = {}) {
  return (tree: Root, file: { value?: unknown }) => {
    const source = typeof file.value === "string" ? file.value : "";
    walk(tree as AnyNode, (parent) => {
      parent.children = (parent.children ?? []).flatMap((child) => {
        if (child.type !== "link") return [child];
        const offset = child.position?.start?.offset;
        const lead = typeof offset === "number" ? source[offset] : undefined;
        if (lead === "[" || lead === "<") return [child];
        child.data = { ...child.data, autolink: true };
        if (options.keepEmail === false && child.url?.toLowerCase().startsWith("mailto:")) return child.children ?? [];
        return [child];
      });
    });
  };
}
