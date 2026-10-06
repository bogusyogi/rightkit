/**
 * Minimal CSL-JSON types used by the local citation formatter.
 * Keeps the dependency surface small while supporting the most
 * common reference types (book, article, chapter, web page).
 */

export interface CslName {
  family?: string;
  given?: string;
  literal?: string;
}

export interface CslDatePart {
  year?: number;
  month?: number;
  day?: number;
}

export interface CslEntry {
  id: string;
  type: string;
  title?: string;
  author?: CslName[];
  editor?: CslName[];
  "container-title"?: string;
  publisher?: string;
  "publisher-place"?: string;
  volume?: string;
  issue?: string;
  page?: string;
  DOI?: string;
  URL?: string;
  ISBN?: string;
  ISSN?: string;
  issued?: { "date-parts"?: number[][] } | number;
  accessed?: { "date-parts"?: number[][] } | number;
}

export interface BibliographyEntry {
  key: string;
  entry: CslEntry;
}

export interface CitationMatch {
  key: string;
  locator?: string;
}
