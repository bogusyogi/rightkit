import type { CslEntry, CslName } from "./types";

export function formatName(name: CslName): string {
  if (name.literal) return name.literal;
  const parts: string[] = [];
  if (name.family) parts.push(name.family);
  if (name.given) parts.push(name.given);
  return parts.join(", ");
}

export function formatNames(names: CslName[] | undefined): string {
  if (!names || names.length === 0) return "";
  const parts = names.map(formatName);
  if (parts.length === 1) return parts[0] ?? "";
  if (parts.length === 2) return `${parts[0]} and ${parts[1]}`;
  return `${parts.slice(0, -1).join(", ")}, and ${parts[parts.length - 1]}`;
}

export function formatYear(entry: CslEntry): string {
  if (!entry.issued) return "";
  if (typeof entry.issued === "number") return String(entry.issued);
  const parts = entry.issued["date-parts"]?.[0];
  if (!parts || parts.length === 0) return "";
  return String(parts[0]);
}

export function formatCitation(entry: CslEntry, locator?: string): string {
  const author = formatNames(entry.author);
  const year = formatYear(entry);
  const parts: string[] = [];
  if (author) parts.push(author);
  if (year) parts.push(year);
  if (locator) parts.push(locator);
  return `(${parts.join(", ")})`;
}

export function formatBibliographyEntry(entry: CslEntry): string {
  const author = formatNames(entry.author);
  const year = formatYear(entry);
  const title = entry.title ?? "Untitled";
  const parts: string[] = [];

  if (author) parts.push(`${author}.`);
  if (year) parts.push(`(${year}).`);

  switch (entry.type) {
    case "book": {
      parts.push(`${_italic(title)}.`);
      if (entry.publisher) parts.push(entry.publisher);
      if (entry["publisher-place"]) parts.push(entry["publisher-place"]);
      break;
    }
    case "chapter": {
      parts.push(`${_italic(title)}.`);
      if (entry["container-title"]) parts.push(`In ${_italic(entry["container-title"])}`);
      if (entry.publisher) parts.push(entry.publisher);
      if (entry.page) parts.push(`pp. ${entry.page}`);
      break;
    }
    case "webpage": {
      parts.push(`${title}.`);
      if (entry.URL) parts.push(`Available at: ${entry.URL}`);
      break;
    }
    default: {
      // article-journal and fallback
      parts.push(`${title}.`);
      if (entry["container-title"]) parts.push(`${_italic(entry["container-title"])}`);
      if (entry.volume) parts.push(entry.volume);
      if (entry.issue) parts.push(`(${entry.issue})`);
      if (entry.page) parts.push(`:${entry.page}`);
      if (entry.DOI) parts.push(`https://doi.org/${entry.DOI}`);
      break;
    }
  }

  return parts.join(" ").replace(/\s+/g, " ");
}

function _italic(text: string): string {
  return text;
  // Rendering layer controls italics; this formatter returns plain text.
}

export function buildBibliographyMap(entries: CslEntry[]): Map<string, CslEntry> {
  const map = new Map<string, CslEntry>();
  for (const entry of entries) {
    if (entry.id) map.set(entry.id, entry);
  }
  return map;
}
