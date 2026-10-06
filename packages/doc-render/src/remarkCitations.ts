import type { Root, RootContent, Text } from "mdast";

export interface CitationNode {
  type: "citation";
  key: string;
  locator?: string;
  children: [];
}

export interface BibliographyNode {
  type: "bibliography";
  children: [];
}

const CITATION_RE = /\[(@([^\]\s]+)(?:\s+([^\]]+))?)\]/g;
const BIBLIOGRAPHY_RE = /^\s*\[bibliography\]\s*$/i;

/** Transform `[@key]` / `[@key p.12]` into citation nodes and `[bibliography]` markers into bibliography blocks. */
export function remarkCitations() {
  return (tree: Root) => {
    tree.children = processTopLevel(tree.children) as RootContent[];
  };
}

function processTopLevel(
  children: (RootContent | BibliographyNode)[],
): (RootContent | BibliographyNode)[] {
  const out: (RootContent | BibliographyNode)[] = [];
  for (const child of children) {
    if (child.type === "paragraph" && isBibliographyMarker(child)) {
      out.push({
        type: "bibliography",
        children: [],
        position: child.position,
      } as BibliographyNode);
      continue;
    }
    if ("children" in child && Array.isArray(child.children)) {
      out.push({
        ...child,
        children: processChildren(child.children as RootContent[]),
      } as RootContent);
      continue;
    }
    out.push(child);
  }
  return out;
}

function isBibliographyMarker(node: { children?: RootContent[] }): boolean {
  const text = (node.children ?? [])
    .map((child) => (child.type === "text" ? (child as Text).value : ""))
    .join("");
  return BIBLIOGRAPHY_RE.test(text);
}

function processChildren(children: (RootContent | CitationNode)[]): (RootContent | CitationNode)[] {
  const out: (RootContent | CitationNode)[] = [];
  for (const child of children) {
    if (child.type === "text" && typeof (child as Text).value === "string") {
      const split = splitCitations((child as Text).value);
      if (split) {
        out.push(...split);
        continue;
      }
    }
    if ("children" in child && Array.isArray(child.children)) {
      out.push({
        ...child,
        children: processChildren(child.children as RootContent[]),
      } as RootContent);
      continue;
    }
    out.push(child);
  }
  return out;
}

function splitCitations(value: string): (Text | CitationNode)[] | null {
  const parts: (Text | CitationNode)[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null = CITATION_RE.exec(value);
  while (match !== null) {
    if (match.index > lastIndex) {
      parts.push({ type: "text", value: value.slice(lastIndex, match.index) } as Text);
    }
    const locator = match[3]?.trim();
    parts.push({ type: "citation", key: match[2] ?? "", locator, children: [] } as CitationNode);
    lastIndex = match.index + match[0].length;
    CITATION_RE.lastIndex = lastIndex;
    match = CITATION_RE.exec(value);
  }
  if (parts.length === 0) return null;
  if (lastIndex < value.length) {
    parts.push({ type: "text", value: value.slice(lastIndex) } as Text);
  }
  return parts;
}
