import type { TranscriptMessage } from "./types";

/** Keep find-in-transcript work bounded even for a very large session. */
export const MAX_TRANSCRIPT_SEARCH_MATCHES = 20_000;

export interface ByteRange {
  start: number;
  end: number;
}

/** One match in a rendered markdown/plain-text element. */
export interface TextSearchMatch {
  /** Stable renderer element ordinal, matching the message's block order. */
  ordinal: number;
  /** UTF-8 byte offsets in the source message content. */
  range: ByteRange;
  /** UTF-16 offsets for DOM Range/highlight APIs. */
  utf16Range: ByteRange;
}

export interface TranscriptSearchMatch {
  messageIndex: number;
  rowIndex: number;
  messageId: string;
  text: TextSearchMatch;
}

export interface TranscriptSearchResult {
  matches: TranscriptSearchMatch[];
  matchesByMessage: Map<number, TextSearchMatch[]>;
  current: number | null;
  limited: boolean;
}

export interface TranscriptSearchTarget {
  generation: number;
  current: number;
  rowIndex: number;
  key: string;
  ordinal: number;
  range: ByteRange;
  utf16Range: ByteRange;
}

export interface TranscriptSearchOptions {
  /** Map a transcript index to the row index used by a virtualized viewport. */
  rowIndexForMessage?: (messageIndex: number, message: TranscriptMessage) => number;
  /** Start selection at the first hit at or after this virtual row. */
  originRow?: number;
  maxMatches?: number;
}

/** Escape a query before making the donor's case-insensitive literal regex. */
export function literalFindRegex(query: string): RegExp {
  const escaped = query.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return new RegExp(escaped, "giu");
}

/**
 * Search markdown while retaining the renderer's block ordinal and both byte
 * and DOM offsets. The ordinal is block-based because MarkdownText mounts one
 * block element per paragraph/list/table/code block.
 */
export function markdownSearchMatches(
  source: string,
  regex: RegExp,
  limit: number,
): [TextSearchMatch[], boolean] {
  return collectSearchMatches(source, regex, limit, markdownOrdinalLookup(source));
}

/** Search non-markdown transcript rows with one stable plain-text element. */
export function plainSearchMatches(
  source: string,
  ordinal: number,
  regex: RegExp,
  limit: number,
): [TextSearchMatch[], boolean] {
  return collectSearchMatches(source, regex, limit, () => ordinal);
}

function collectSearchMatches(
  source: string,
  regex: RegExp,
  limit: number,
  ordinalAt: (offset: number) => number,
): [TextSearchMatch[], boolean] {
  const matches: TextSearchMatch[] = [];
  const boundedLimit = Math.max(0, limit);
  const flags = regex.flags.includes("g") ? regex.flags : `${regex.flags}g`;
  const scanner = new RegExp(regex.source, flags);
  let limited = false;
  let found = scanner.exec(source);
  let utf16Cursor = 0;
  let byteCursor = 0;

  while (found !== null) {
    if (matches.length >= boundedLimit) {
      limited = true;
      break;
    }
    const utf16Start = found.index;
    const utf16End = utf16Start + found[0].length;
    // Advance the byte cursor from the previous match instead of re-measuring
    // the whole prefix each time; slicing from 0 per match is quadratic.
    const startBytes = byteCursor + utf8ByteLength(source.slice(utf16Cursor, utf16Start));
    const endBytes = startBytes + utf8ByteLength(source.slice(utf16Start, utf16End));
    utf16Cursor = utf16End;
    byteCursor = endBytes;
    matches.push({
      ordinal: ordinalAt(utf16Start),
      range: {
        start: startBytes,
        end: endBytes,
      },
      utf16Range: { start: utf16Start, end: utf16End },
    });
    // RegExp.exec otherwise loops forever on a zero-width expression.
    if (found[0].length === 0) scanner.lastIndex += 1;
    found = scanner.exec(source);
  }

  return [matches, limited];
}

/**
 * Search the same visible message classes as Waku: user/assistant prose uses
 * markdown block ordinals, while tool/compact rows use a single plain element.
 */
export function searchTranscript(
  messages: readonly TranscriptMessage[],
  query: string,
  options: TranscriptSearchOptions = {},
): TranscriptSearchResult {
  const matches: TranscriptSearchMatch[] = [];
  const matchesByMessage = new Map<number, TextSearchMatch[]>();
  const maxMatches = options.maxMatches ?? MAX_TRANSCRIPT_SEARCH_MATCHES;
  const originRow = options.originRow ?? 0;
  const regex = literalFindRegex(query);
  let limited = false;

  if (query.length === 0 || maxMatches <= 0) {
    return { matches, matchesByMessage, current: null, limited: false };
  }

  for (let messageIndex = 0; messageIndex < messages.length; messageIndex += 1) {
    const message = messages[messageIndex];
    // System rows are not mounted by TranscriptGroupView and cannot be a
    // stable reveal target, so leave them out of the visible search index.
    if (message.$kind === "system") continue;

    const source = transcriptMessageText(message);
    const remaining = maxMatches - matches.length;
    const [textMatches, messageLimited] =
      message.$kind === "user" || message.$kind === "assistant"
        ? markdownSearchMatches(source, regex, remaining)
        : plainSearchMatches(source, 0, regex, remaining);

    if (textMatches.length > 0) {
      matchesByMessage.set(messageIndex, textMatches);
      const rowIndex = options.rowIndexForMessage?.(messageIndex, message) ?? messageIndex;
      for (const text of textMatches) {
        matches.push({ messageIndex, rowIndex, messageId: message.id, text });
      }
    }
    if (messageLimited) {
      limited = true;
      break;
    }
  }

  const firstAtOrAfterOrigin = matches.findIndex((match) => match.rowIndex >= originRow);
  const current = matches.length
    ? firstAtOrAfterOrigin >= 0
      ? firstAtOrAfterOrigin
      : 0
    : null;
  return { matches, matchesByMessage, current, limited };
}

function transcriptMessageText(
  message: Exclude<TranscriptMessage, { $kind: "system" }>,
): string {
  switch (message.$kind) {
    case "user":
    case "assistant":
      return message.content;
    case "tool":
    case "compact":
      return message.summary;
  }
}

/** Assign one stable 16-bit slot to each markdown block, like Waku's ordinals. */
/**
 * Build the block-ordinal lookup once per source instead of rescanning the
 * whole string for every match. A 20k-match transcript otherwise costs
 * O(matches x length), which pushed find-in-transcript past five seconds.
 */
function markdownOrdinalLookup(source: string): (offset: number) => number {
  const lineEnds: number[] = [];
  const lineBlocks: number[] = [];
  let cursor = 0;
  let block = -1;
  let inFence = false;
  let previousNonEmpty = false;

  for (const line of source.split("\n")) {
    const lineEnd = cursor + line.length;
    const trimmed = line.trim();
    const fence = /^\s*(```|~~~)/.test(line);
    if (!trimmed) {
      if (!inFence) previousNonEmpty = false;
    } else if (!inFence && (!previousNonEmpty || fence)) {
      block += 1;
      previousNonEmpty = true;
    } else {
      previousNonEmpty = true;
    }

    if (fence) inFence = !inFence;
    lineEnds.push(lineEnd);
    lineBlocks.push(Math.max(0, block) << 16);
    cursor = lineEnd + 1;
  }

  const fallback = Math.max(0, block) << 16;
  let index = 0;
  return (offset: number): number => {
    // Match offsets arrive in non-decreasing order, so advance the cursor
    // rather than restarting the scan.
    if (index >= lineEnds.length || offset < (index === 0 ? 0 : lineEnds[index - 1] + 1)) index = 0;
    while (index < lineEnds.length && offset > lineEnds[index]) index += 1;
    return index < lineEnds.length ? lineBlocks[index] : fallback;
  };
}

function utf8ByteLength(value: string): number {
  let bytes = 0;
  for (let index = 0; index < value.length; index += 1) {
    const codePoint = value.codePointAt(index) ?? 0;
    if (codePoint <= 0x7f) bytes += 1;
    else if (codePoint <= 0x7ff) bytes += 2;
    else if (codePoint <= 0xffff) bytes += 3;
    else {
      bytes += 4;
      index += 1;
    }
  }
  return bytes;
}

/** Renderer-facing state machine: refresh on query change, then cycle hits. */
export class TranscriptSearch {
  private openState = false;
  private result: TranscriptSearchResult = {
    matches: [],
    matchesByMessage: new Map(),
    current: null,
    limited: false,
  };
  private generationState = 0;

  get open(): boolean {
    return this.openState;
  }

  get matches(): readonly TranscriptSearchMatch[] {
    return this.result.matches;
  }

  get current(): number | null {
    return this.result.current;
  }

  get limited(): boolean {
    return this.result.limited;
  }

  get generation(): number {
    return this.generationState;
  }

  openSearch(): void {
    this.openState = true;
  }

  closeSearch(): void {
    this.openState = false;
    this.result = { matches: [], matchesByMessage: new Map(), current: null, limited: false };
    this.bumpGeneration();
  }

  resetForSession(): void {
    this.closeSearch();
  }

  refresh(
    messages: readonly TranscriptMessage[],
    query: string,
    options?: TranscriptSearchOptions,
  ): void {
    this.openState = true;
    this.result = searchTranscript(messages, query, options);
    this.bumpGeneration();
  }

  navigate(backwards = false): void {
    if (!this.openState || this.result.matches.length === 0) return;
    const count = this.result.matches.length;
    const current = this.result.current;
    this.result.current = current === null
      ? backwards ? count - 1 : 0
      : backwards ? (current + count - 1) % count : (current + 1) % count;
    this.bumpGeneration();
  }

  highlights(messageIndex: number): readonly TextSearchMatch[] | undefined {
    if (!this.openState) return undefined;
    return this.result.matchesByMessage.get(messageIndex);
  }

  target(): TranscriptSearchTarget | undefined {
    if (!this.openState || this.result.current === null) return undefined;
    const found = this.result.matches[this.result.current];
    if (!found) return undefined;
    return {
      generation: this.generationState,
      current: this.result.current,
      rowIndex: found.rowIndex,
      key: `message-${found.messageId}`,
      ordinal: found.text.ordinal,
      range: found.text.range,
      utf16Range: found.text.utf16Range,
    };
  }

  private bumpGeneration() {
    this.generationState = (this.generationState + 1) >>> 0;
  }
}
