import type { Paragraph, Root, RootContent, Text } from "mdast";

export interface DescriptionTerm {
  type: "descriptionTerm";
  children: RootContent[];
}

export interface DescriptionDetails {
  type: "descriptionDetails";
  children: RootContent[];
}

export interface DescriptionItem {
  type: "descriptionItem";
  terms: DescriptionTerm[];
  details: DescriptionDetails[];
}

export interface DescriptionList {
  type: "descriptionList";
  children: DescriptionItem[];
}

export interface AbbreviationDefinition {
  type: "abbreviationDefinition";
  short: string;
  long: string;
}

export interface Abbr {
  type: "abbr";
  title: string;
  children: Text[];
}

const DEF_RE = /^:(?:\s+(.*))?$/;
const ABBR_DEF_RE = /^\*\[([^\]]+)\]:\s*(.+)$/;

/** Transform Pandoc-style definition lists and abbreviation definitions into custom mdast nodes. */
export function remarkDefListAbbr() {
  return (tree: Root) => {
    const abbreviations = new Map<string, string>();
    tree.children = transformTopLevel(tree.children, abbreviations) as RootContent[];
    if (abbreviations.size > 0) {
      expandAbbreviations(tree.children as (RootContent | DescriptionList)[], abbreviations);
    }
  };
}

function transformTopLevel(
  children: RootContent[],
  abbreviations: Map<string, string>,
): (RootContent | DescriptionList)[] {
  const out: (RootContent | DescriptionList)[] = [];
  let i = 0;
  while (i < children.length) {
    const child = children[i];
    if (child && child.type === "paragraph") {
      const abbrSplit = splitAbbreviationDefinition(child, abbreviations);
      if (abbrSplit) {
        out.push(abbrSplit.abbr as unknown as RootContent);
        if (abbrSplit.remainder) {
          out.push(transformTopLevel([abbrSplit.remainder], abbreviations)[0] as RootContent);
        }
        i += 1;
        continue;
      }
      const inlineList = parseInlineDefinitionList(child);
      if (inlineList) {
        out.push({
          ...inlineList,
          position: child.position,
        } as DescriptionList as unknown as RootContent);
        i += 1;
        continue;
      }
    }
    const result = consumeDefinitionList(children, i);
    if (result) {
      out.push(result.list);
      i = result.nextIndex;
      continue;
    }
    if (child && "children" in child && Array.isArray(child.children)) {
      out.push(transformNode(child, abbreviations) as RootContent);
      i += 1;
      continue;
    }
    out.push(child);
    i += 1;
  }
  return out;
}

function transformNode(node: RootContent, abbreviations: Map<string, string>): RootContent {
  if (node.type === "paragraph" || node.type === "heading" || node.type === "blockquote") {
    const typed = node as Extract<RootContent, { type: "paragraph" | "heading" | "blockquote" }> & {
      children: RootContent[];
    };
    const copy = { ...typed, children: [...typed.children] } as typeof typed;
    copy.children = transformInline(
      copy.children,
      abbreviations,
    ) as unknown as typeof typed.children;
    return copy as RootContent;
  }
  if (node.type === "list") {
    const typed = node as Extract<RootContent, { type: "list" }> & { children: RootContent[] };
    const copy = { ...typed, children: [...typed.children] } as typeof typed;
    copy.children = copy.children.map((item) =>
      transformNode(item, abbreviations),
    ) as unknown as typeof typed.children;
    return copy as RootContent;
  }
  if (node.type === "listItem") {
    const typed = node as Extract<RootContent, { type: "listItem" }> & { children: RootContent[] };
    const copy = { ...typed, children: [...typed.children] } as typeof typed;
    copy.children = transformTopLevel(
      copy.children,
      abbreviations,
    ) as unknown as typeof typed.children;
    return copy as RootContent;
  }
  return node;
}

function splitAbbreviationDefinition(
  node: Paragraph,
  abbreviations: Map<string, string>,
): { abbr: AbbreviationDefinition; remainder: Paragraph | null } | null {
  const text = paragraphText(node);
  const lines = text.split("\n");
  const first = lines[0] ?? "";
  const m = ABBR_DEF_RE.exec(first);
  if (!m) return null;
  const short = m[1] ?? "";
  const long = m[2]?.trim() ?? "";
  abbreviations.set(short, long);
  const remainder: Paragraph | null =
    lines.length > 1
      ? ({
          type: "paragraph",
          children: [{ type: "text", value: lines.slice(1).join("\n") }],
        } as Paragraph)
      : null;
  return { abbr: { type: "abbreviationDefinition", short, long }, remainder };
}

function parseInlineDefinitionList(node: Paragraph): DescriptionList | null {
  if (node.children.some((child) => child.type !== "text")) return null;
  const text = paragraphText(node);
  const lines = text.split("\n");
  const items: DescriptionItem[] = [];
  let i = 0;
  while (i < lines.length) {
    const terms: string[] = [];
    while (i < lines.length && !DEF_RE.exec(lines[i] ?? "")) {
      terms.push(lines[i] ?? "");
      i += 1;
    }
    if (terms.length === 0) break;
    const details: string[] = [];
    while (i < lines.length) {
      const m = DEF_RE.exec(lines[i] ?? "");
      if (!m) break;
      details.push(m[1] ?? "");
      i += 1;
    }
    if (details.length === 0) break;
    items.push({
      type: "descriptionItem",
      terms: terms.map(
        (t) =>
          ({ type: "descriptionTerm", children: [{ type: "text", value: t }] }) as DescriptionTerm,
      ),
      details: details.map(
        (d) =>
          ({
            type: "descriptionDetails",
            children: [{ type: "text", value: d }],
          }) as DescriptionDetails,
      ),
    });
  }
  if (items.length === 0) return null;
  // Only treat the whole paragraph as a definition list if every line was consumed.
  const consumed = items.reduce((sum, item) => sum + item.terms.length + item.details.length, 0);
  if (consumed !== lines.length) return null;
  return { type: "descriptionList", children: items };
}

function consumeDefinitionList(
  children: RootContent[],
  start: number,
): { list: DescriptionList; nextIndex: number } | null {
  const items: DescriptionItem[] = [];
  let firstTermParagraph: Paragraph | null = null;
  let lastDetailParagraph: Paragraph | null = null;
  let i = start;
  while (i < children.length) {
    const termResult = consumeTerms(children, i);
    if (!termResult) break;
    const detailsResult = consumeDetails(children, termResult.nextIndex);
    if (detailsResult.details.length === 0) break;
    if (!firstTermParagraph) firstTermParagraph = termResult.terms[0] ?? null;
    lastDetailParagraph = detailsResult.details[detailsResult.details.length - 1] ?? null;
    items.push({
      type: "descriptionItem",
      terms: termResult.terms.map(
        (t) => ({ type: "descriptionTerm", children: t.children }) as DescriptionTerm,
      ),
      details: detailsResult.details.map(
        (d) => ({ type: "descriptionDetails", children: d.children }) as DescriptionDetails,
      ),
    });
    i = detailsResult.nextIndex;
  }
  if (items.length === 0) return null;
  const position =
    firstTermParagraph?.position && lastDetailParagraph?.position
      ? { start: firstTermParagraph.position.start, end: lastDetailParagraph.position.end }
      : undefined;
  return {
    list: { type: "descriptionList", children: items, position } as DescriptionList,
    nextIndex: i,
  };
}

function consumeTerms(
  children: RootContent[],
  start: number,
): { terms: Paragraph[]; nextIndex: number } | null {
  const terms: Paragraph[] = [];
  let i = start;
  while (i < children.length) {
    const child = children[i];
    if (child?.type !== "paragraph") break;
    const text = paragraphText(child);
    if (DEF_RE.exec(text) || ABBR_DEF_RE.test(text)) break;
    terms.push(child);
    i += 1;
  }
  if (terms.length === 0) return null;
  return { terms, nextIndex: i };
}

function consumeDetails(
  children: RootContent[],
  start: number,
): { details: Paragraph[]; nextIndex: number } {
  const details: Paragraph[] = [];
  let i = start;
  while (i < children.length) {
    const child = children[i];
    if (child?.type !== "paragraph") break;
    const text = paragraphText(child);
    const m = DEF_RE.exec(text);
    if (!m) break;
    const inner = m[1] ?? "";
    const stripped: Paragraph = {
      ...child,
      children: inner ? [{ type: "text", value: inner }] : [],
    };
    details.push(stripped);
    i += 1;
  }
  return { details, nextIndex: i };
}

function paragraphText(node: Paragraph): string {
  return node.children
    .filter((c): c is Text => c.type === "text")
    .map((c) => c.value)
    .join("");
}

function transformInline(
  children: RootContent[],
  abbreviations: Map<string, string>,
): (RootContent | Abbr)[] {
  const out: (RootContent | Abbr)[] = [];
  for (const child of children) {
    if (child.type === "text" && abbreviations.size > 0) {
      out.push(...splitAbbreviations(child as Text, abbreviations));
      continue;
    }
    if ((child as { type?: string }).type === "abbr") {
      out.push(child);
      continue;
    }
    if ("children" in child && Array.isArray(child.children)) {
      out.push({
        ...child,
        children: transformInline(child.children as RootContent[], abbreviations),
      } as unknown as RootContent);
      continue;
    }
    out.push(child);
  }
  return out;
}

function splitAbbreviations(text: Text, abbreviations: Map<string, string>): (Text | Abbr)[] {
  const sorted = Array.from(abbreviations.entries()).sort((a, b) => b[0].length - a[0].length);
  let parts: (Text | Abbr)[] = [text];
  for (const [short, long] of sorted) {
    const next: (Text | Abbr)[] = [];
    const pattern = new RegExp(`(^|[^A-Za-z0-9])(${escapeRegExp(short)})(?=[^A-Za-z0-9]|$)`, "g");
    for (const part of parts) {
      if (part.type !== "text") {
        next.push(part);
        continue;
      }
      const value = part.value;
      const matches = Array.from(value.matchAll(pattern));
      let lastIndex = 0;
      for (const match of matches) {
        const index = match.index ?? 0;
        const leading = match[1];
        const before = value.slice(lastIndex, index + leading.length);
        if (before) {
          next.push({ type: "text", value: before });
        }
        next.push({
          type: "abbr",
          title: long,
          children: [{ type: "text", value: short }],
        } as Abbr);
        lastIndex = index + leading.length + short.length;
      }
      const remaining = value.slice(lastIndex);
      if (remaining) {
        next.push({ type: "text", value: remaining });
      }
      lastIndex = 0;
    }
    parts = next;
  }
  return parts;
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function expandAbbreviations(
  children: (RootContent | DescriptionList)[],
  abbreviations: Map<string, string>,
): void {
  for (let i = 0; i < children.length; i++) {
    const child = children[i];
    if (child.type === "paragraph" || child.type === "heading" || child.type === "blockquote") {
      const typed = child as Extract<
        RootContent,
        { type: "paragraph" | "heading" | "blockquote" }
      > & {
        children: RootContent[];
      };
      typed.children = transformInline(
        typed.children,
        abbreviations,
      ) as unknown as typeof typed.children;
    } else if (child.type === "descriptionList") {
      for (const item of child.children) {
        for (const term of item.terms) {
          term.children = transformInline(term.children, abbreviations) as unknown as RootContent[];
        }
        for (const detail of item.details) {
          detail.children = transformInline(
            detail.children,
            abbreviations,
          ) as unknown as RootContent[];
        }
      }
    }
  }
}
