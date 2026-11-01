import documentFormatsTsv from "../shared/document_formats.tsv?raw";
import type { DocFormat } from "./types";

export interface DocumentFormatEntry {
  extension: string;
  format: Exclude<DocFormat, "web" | "unknown">;
  mime?: string;
}

export const documentFormatEntries: readonly DocumentFormatEntry[] = documentFormatsTsv
  .split(/\r?\n/)
  // Skip blank lines exactly as the Rust parser does (documents.rs parse_format_table), so the
  // two layers never disagree about which rows exist.
  .filter((line) => line.trim() !== "" && !line.startsWith("#"))
  .map((line) => {
    // TSV imports preserve CRLF line endings. Trim every field so rows without
    // a MIME column (including html/htm) do not become an invalid `"html\r"`
    // format and get routed as a separate document tab.
    const [extension, format, mime] = line.split("\t").map((field) => field.trim());
    return { extension, format: format as DocumentFormatEntry["format"], mime };
  });

const formatByExtension = new Map(documentFormatEntries.map((entry) => [entry.extension, entry]));

export function formatEntryForExtension(extension: string): DocumentFormatEntry | undefined {
  return formatByExtension.get(extension.toLowerCase());
}
