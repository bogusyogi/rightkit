import type { CslEntry, CslName } from "@rightkit/doc-render";

/**
 * Parse a CSL-JSON bibliography file.
 * Supports both an array of entries and a `{ references: [...] }` wrapper.
 */
export function parseCslJson(text: string): CslEntry[] {
  const parsed = JSON.parse(text) as unknown;
  if (Array.isArray(parsed)) return parsed as CslEntry[];
  if (parsed && typeof parsed === "object" && "references" in parsed) {
    return ((parsed as { references: CslEntry[] }).references ?? []) as CslEntry[];
  }
  return [];
}

/**
 * Minimal BibTeX → CSL-JSON converter.
 * Handles the most common entry types and fields; braces are stripped
 * and simple LaTeX accents are transliterated.
 */
export function parseBibTeX(text: string): CslEntry[] {
  const entries: CslEntry[] = [];
  const entryRe = /@(?<type>\w+)\s*\{\s*(?<key>[^,\s]+)\s*,(?<body>.*?)\n?\}\s*$/gms;
  let match: RegExpExecArray | null = entryRe.exec(text);
  while (match !== null) {
    const groups = match.groups ?? {};
    const type = (groups.type ?? "misc").toLowerCase();
    const key = (groups.key ?? "").trim();
    const body = groups.body ?? "";
    if (!key) continue;
    const fields = parseBibTeXFields(body);
    const entry = bibTeXToCsl(key, type, fields);
    if (entry) entries.push(entry);
    match = entryRe.exec(text);
  }
  return entries;
}

function parseBibTeXFields(body: string): Map<string, string> {
  const fields = new Map<string, string>();
  const re = /\s*(\w+)\s*=\s*/g;
  let match: RegExpExecArray | null = re.exec(body);
  while (match !== null) {
    const start = match.index + match[0].length;
    const fieldName = match[1] ?? "";
    let depth = 0;
    let inQuote = false;
    let i = start;
    while (i < body.length) {
      const ch = body[i];
      if (ch === '"' && body[i - 1] !== "\\") inQuote = !inQuote;
      if (!inQuote) {
        if (ch === "{" || ch === "(") depth++;
        if (ch === "}" || ch === ")") {
          if (depth === 0) {
            // reached end of entry, stop
            break;
          }
          depth--;
        }
        if (ch === "," && depth === 0) break;
      }
      i++;
    }
    const value = body.slice(start, i).trim();
    // advance regex to keep parsing after this field
    re.lastIndex = i;
    fields.set(fieldName.toLowerCase(), stripBibTeXBraces(value).trim());
    match = re.exec(body);
  }
  return fields;
}

function stripBibTeXBraces(value: string): string {
  let out = "";
  let depth = 0;
  for (let i = 0; i < value.length; i++) {
    const ch = value[i];
    if (ch === "{") {
      depth++;
      continue;
    }
    if (ch === "}" && depth > 0) {
      depth--;
      continue;
    }
    if (ch === '"' && (i === 0 || i === value.length - 1)) continue;
    out += ch;
  }
  return out.replace(/\\([&#%$#_{}~^])/g, "$1");
}

const TYPE_MAP: Record<string, string> = {
  article: "article-journal",
  book: "book",
  incollection: "chapter",
  inbook: "chapter",
  inproceedings: "paper-conference",
  conference: "paper-conference",
  proceedings: "book",
  misc: "article",
  online: "webpage",
  techreport: "report",
  phdthesis: "thesis",
  masterthesis: "thesis",
  mastersthesis: "thesis",
};

function bibTeXToCsl(key: string, type: string, fields: Map<string, string>): CslEntry | null {
  const entry: CslEntry = {
    id: key,
    type: TYPE_MAP[type] ?? "article",
    title: get(fields, "title"),
    author: parseNames(get(fields, "author")),
    editor: parseNames(get(fields, "editor")),
    "container-title": get(fields, "journal") ?? get(fields, "booktitle"),
    publisher: get(fields, "publisher"),
    "publisher-place": get(fields, "address"),
    volume: get(fields, "volume"),
    issue: get(fields, "number") ?? get(fields, "issue"),
    page: get(fields, "pages"),
    DOI: get(fields, "doi"),
    URL: get(fields, "url"),
    ISBN: get(fields, "isbn"),
    ISSN: get(fields, "issn"),
  };
  const year = get(fields, "year");
  const month = get(fields, "month");
  if (year) {
    const y = Number(year);
    const parts: number[] = [Number.isFinite(y) ? y : 0];
    if (month) {
      const m = monthToNumber(month);
      if (m > 0) parts.push(m);
    }
    entry.issued = { "date-parts": [parts] };
  }
  return entry;
}

function get(fields: Map<string, string>, key: string): string | undefined {
  const value = fields.get(key);
  return value ? value.trim() : undefined;
}

function parseNames(raw: string | undefined): CslName[] | undefined {
  if (!raw) return undefined;
  return raw.split(" and ").map((part) => {
    const trimmed = part.trim();
    const comma = trimmed.indexOf(",");
    if (comma >= 0) {
      return { family: trimmed.slice(0, comma).trim(), given: trimmed.slice(comma + 1).trim() };
    }
    const space = trimmed.lastIndexOf(" ");
    if (space >= 0) {
      return { family: trimmed.slice(space + 1).trim(), given: trimmed.slice(0, space).trim() };
    }
    return { literal: trimmed };
  });
}

function monthToNumber(month: string): number {
  const lower = month.toLowerCase().trim();
  const map: Record<string, number> = {
    jan: 1,
    feb: 2,
    mar: 3,
    apr: 4,
    may: 5,
    jun: 6,
    jul: 7,
    aug: 8,
    sep: 9,
    oct: 10,
    nov: 11,
    dec: 12,
  };
  const n = Number(lower);
  if (Number.isFinite(n) && n >= 1 && n <= 12) return n;
  return map[lower] ?? 0;
}
