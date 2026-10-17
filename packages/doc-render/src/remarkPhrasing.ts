import type { Root } from "mdast";

type AnyNode = { type: string; value?: string; url?: string; children?: AnyNode[]; data?: Record<string, unknown>; position?: { start?: { offset?: number } } };

const HIGHLIGHT = /==(?=\S)([^=\n]+?)(?<=\S)==/g;
const LITERAL_CONTAINERS = new Set(["code", "inlineCode", "math", "inlineMath", "html", "yaml", "toml"]);

function walk(node: AnyNode, visit: (parent: AnyNode) => void): void {
  if (LITERAL_CONTAINERS.has(node.type) || !node.children) return;
  visit(node);
  for (const child of node.children) walk(child, visit);
}

/** `==text==` becomes a `highlight` node (rendered as `<mark>`). Plain text only: the marker pair
 * must sit inside one text node, so it never crosses emphasis, links or code. */
export function remarkHighlight() {
  return (tree: Root) => {
    walk(tree as AnyNode, (parent) => {
      if (parent.type === "link" || parent.type === "highlight") return;
      const next: AnyNode[] = [];
      for (const child of parent.children ?? []) {
        if (child.type !== "text" || !child.value?.includes("==")) { next.push(child); continue; }
        let last = 0;
        for (const match of child.value.matchAll(HIGHLIGHT)) {
          const index = match.index ?? 0;
          if (index > last) next.push({ type: "text", value: child.value.slice(last, index) });
          next.push({ type: "highlight", children: [{ type: "text", value: match[1] }] });
          last = index + match[0].length;
        }
        if (last === 0) next.push(child);
        else if (last < child.value.length) next.push({ type: "text", value: child.value.slice(last) });
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
