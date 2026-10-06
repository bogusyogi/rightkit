import type { Root, RootContent, Text } from "mdast";

export interface WikiLinkNode {
  type: "wikiLink";
  target: string;
  hash: string;
  display: string | null;
  children: [];
}

const WIKI_LINK_RE = /\[\[([^\]\n]+?)\]\]/g;

/** Transform plain `[[target]]` / `[[target#hash|display]]` text segments into wiki-link nodes. */
export function remarkWikiLink() {
  return (tree: Root) => {
    tree.children = processChildren(tree.children) as RootContent[];
  };
}

function processChildren(children: (RootContent | WikiLinkNode)[]): (RootContent | WikiLinkNode)[] {
  const out: (RootContent | WikiLinkNode)[] = [];
  for (const child of children) {
    const node = child as { type?: string; value?: string; children?: unknown[] };
    if (node.type === "text" && typeof node.value === "string") {
      const split = splitWikiLink(node.value);
      out.push(...(split ?? [child]));
      continue;
    }
    if (Array.isArray(node.children)) {
      out.push({
        ...node,
        children: processChildren(node.children as (RootContent | WikiLinkNode)[]),
      } as RootContent);
      continue;
    }
    out.push(child);
  }
  return out;
}

function splitWikiLink(value: string): (Text | WikiLinkNode)[] | null {
  const parts: (Text | WikiLinkNode)[] = [];
  let lastIndex = 0;
  let match: RegExpExecArray | null = WIKI_LINK_RE.exec(value);
  while (match !== null) {
    if (match.index > lastIndex) {
      parts.push({ type: "text", value: value.slice(lastIndex, match.index) } as Text);
    }
    parts.push(parseWikiLink(match[1]));
    lastIndex = match.index + match[0].length;
    WIKI_LINK_RE.lastIndex = lastIndex;
    match = WIKI_LINK_RE.exec(value);
  }
  if (parts.length === 0) return null;
  if (lastIndex < value.length) {
    parts.push({ type: "text", value: value.slice(lastIndex) } as Text);
  }
  return parts;
}

function parseWikiLink(inner: string): WikiLinkNode {
  const pipeIndex = inner.lastIndexOf("|");
  const left = pipeIndex >= 0 ? inner.slice(0, pipeIndex) : inner;
  const display = pipeIndex >= 0 ? inner.slice(pipeIndex + 1) : null;
  const hashIndex = left.indexOf("#");
  let target = "";
  let hash = "";
  if (hashIndex >= 0) {
    target = left.slice(0, hashIndex);
    hash = left.slice(hashIndex + 1);
  } else {
    target = left;
  }
  return { type: "wikiLink", target, hash, display, children: [] };
}
