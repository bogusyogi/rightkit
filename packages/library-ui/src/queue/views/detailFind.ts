// Pure helpers behind the Detail reader's find bar, SRT tab and timestamped
// transcript lines. Kept free of React and CSS imports so they can be unit
// tested directly (tests/detail-find-interaction.test.ts).
import type { TranscriptDetail } from "../contract.js";

/** Shorter queries match nearly every character; they neither count nor highlight. */
export const FIND_MIN_CHARS = 2;
/** Highlights rendered per transcript. The count still reports every match. */
export const FIND_HIGHLIGHT_CAP = 2000;

export function normalizeFindQuery(query: string): string {
  const q = query.trim();
  return q.length >= FIND_MIN_CHARS ? q : "";
}

export function countMatches(text: string, query: string): number {
  const q = normalizeFindQuery(query).toLowerCase();
  if (!q) return 0;
  const haystack = text.toLowerCase();
  let count = 0;
  let from = 0;
  for (;;) {
    const idx = haystack.indexOf(q, from);
    if (idx === -1) break;
    count += 1;
    from = idx + q.length;
  }
  return count;
}

export type FindSegment =
  | { type: "text"; value: string }
  | { type: "match"; value: string; index: number };

/** One pass over the text. Matches past `cap` stay plain text. */
export function findSegments(
  text: string,
  query: string,
  cap: number = FIND_HIGHLIGHT_CAP,
): FindSegment[] {
  const q = normalizeFindQuery(query).toLowerCase();
  if (!q) return [];
  const segs: FindSegment[] = [];
  const lower = text.toLowerCase();
  let from = 0;
  let matchCounter = 0;
  for (;;) {
    const idx = matchCounter < cap ? lower.indexOf(q, from) : -1;
    if (idx === -1) {
      if (from < text.length) segs.push({ type: "text", value: text.slice(from) });
      break;
    }
    if (idx > from) segs.push({ type: "text", value: text.slice(from, idx) });
    segs.push({ type: "match", value: text.slice(idx, idx + q.length), index: matchCounter++ });
    from = idx + q.length;
  }
  return segs;
}

/** Strip markdown syntax for readable display. Handles headings, bold, italic,
 *  links, lists, and horizontal rules. Not a full parser — good enough for
 *  transcript exports which use simple formatting. */
export function stripMarkdown(md: string): string {
  return md
    .replace(/^#{1,6}\s+/gm, "") // headings
    .replace(/\*\*(.+?)\*\*/g, "$1") // bold
    .replace(/__(.+?)__/g, "$1") // bold alt
    .replace(/\*(.+?)\*/g, "$1") // italic
    .replace(/_(.+?)_/g, "$1") // italic alt
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1") // links
    .replace(/^[-*+]\s+/gm, "") // unordered lists
    .replace(/^\d+\.\s+/gm, "") // ordered lists
    .replace(/^>\s+/gm, "") // blockquotes
    .replace(/^---+$/gm, "") // horizontal rules
    .replace(/`([^`]+)`/g, "$1") // inline code
    .trim();
}

/** The text the find bar searches on a markdown tab: exactly what MarkdownBody renders. */
export function findTextForMarkdown(text: string): string {
  return stripMarkdown(text);
}

export interface TranscriptEntry {
  time?: string;
  seconds?: number;
  text: string;
}

/** Lines of the desktop transcript view. With timestamps, `[MM:SS] text` lines
 *  become seekable entries; otherwise blank-line separated blocks. */
export function transcriptEntries(
  detail: Pick<TranscriptDetail, "text" | "timed_text">,
  timestamps: boolean,
): TranscriptEntry[] {
  const source = timestamps ? (detail.timed_text?.trim() ?? "") : detail.text.trim();
  const list: TranscriptEntry[] = [];
  if (timestamps && source) {
    for (const line of source.split(/\r?\n/)) {
      const found = line.match(/^\s*(?:[-*+]\s*)?\[(\d{1,2}:\d{2}(?::\d{2})?)\]\s*(.*)$/);
      if (found) {
        const seconds = stampToSeconds(found[1]);
        list.push({ time: found[1], seconds: seconds ?? undefined, text: found[2] });
      } else if (line.trim()) {
        list.push({ text: line.trim() });
      }
    }
  } else {
    for (const block of source.split(/\n\s*\n/)) {
      const text = block.trim();
      if (text) list.push({ text });
    }
  }
  return list;
}

/** Match count over the entries, computed per entry the way they render. */
export function countEntryMatches(entries: TranscriptEntry[], query: string): number {
  let total = 0;
  for (const entry of entries) total += countMatches(entry.text, query);
  return total;
}

/** `MM:SS` / `H:MM:SS` line stamps from timed_text → seconds (WS-7). */
export function stampToSeconds(stamp: string): number | null {
  const parts = stamp.trim().split(":");
  if (parts.length < 2 || parts.length > 3) return null;
  const nums = parts.map((part) => Number(part));
  if (nums.some((value) => !Number.isFinite(value) || value < 0)) return null;
  return parts.length === 3 ? nums[0] * 3600 + nums[1] * 60 + nums[2] : nums[0] * 60 + nums[1];
}

export interface SrtCue {
  timecode: string;
  startS?: number;
  endS?: number;
  text: string;
}

/** `HH:MM:SS,mmm` (or `.mmm`) SRT timecode → seconds. */
export function srtTimeToSeconds(value: string): number | undefined {
  const m = value.trim().match(/(\d{1,3}):(\d{2}):(\d{2})[,.](\d{1,3})/);
  if (!m) return undefined;
  return Number(m[1]) * 3600 + Number(m[2]) * 60 + Number(m[3]) + Number(m[4]) / 1000;
}

export function parseSrt(srt: string): SrtCue[] {
  const blocks = srt.trim().split(/\n\n+/);
  const cues: SrtCue[] = [];
  for (const block of blocks) {
    const lines = block.trim().split("\n");
    if (lines.length < 2) continue;
    const tcLine = lines.find((line) => line.includes("-->"));
    if (!tcLine) continue;
    const tcIdx = lines.indexOf(tcLine);
    const text = lines
      .slice(tcIdx + 1)
      .join(" ")
      .trim();
    const [start, end] = tcLine.split("-->");
    cues.push({
      timecode: tcLine,
      startS: start ? srtTimeToSeconds(start) : undefined,
      endS: end ? srtTimeToSeconds(end) : undefined,
      text,
    });
  }
  return cues;
}
