import type { TranscriptDensity, TranscriptMessage, UserQuestion } from "../types.js";
import { groupTranscriptMessages } from "../transcriptGroups.js";
import type { TranscriptGroup } from "../transcriptGroups.js";

/**
 * Waku's transcript list is virtualized, but its row projection is also
 * cached. React needs the same separation: a new render of the workbench must
 * not regroup a settled transcript merely because an unrelated surface
 * changed. These values intentionally describe rendering policy, not storage.
 */
export const LONG_TRANSCRIPT_THRESHOLD = 80;
export const TRANSCRIPT_ROW_ESTIMATE_PX = 96;
export const TRANSCRIPT_OVERSCAN_ROWS = 8;

export type PendingTranscriptQuestion = {
  questionId: string;
  questions: UserQuestion[];
};

export interface TranscriptRenderInput {
  messages: readonly TranscriptMessage[];
  pendingQuestion: PendingTranscriptQuestion | null;
  density: TranscriptDensity;
}

export interface TranscriptRenderCache {
  fingerprint: number | null;
  groups: readonly TranscriptGroup[];
  messageRefs: readonly TranscriptMessage[] | null;
  pendingQuestion: PendingTranscriptQuestion | null;
  density: TranscriptDensity | null;
}

export interface TranscriptRowLayout {
  index: number;
  start: number;
  size: number;
}

export interface TranscriptViewportWindow {
  totalSize: number;
  first: number;
  last: number;
  rows: readonly TranscriptRowLayout[];
}

const FNV_OFFSET = 0xcbf29ce484222325n;
const FNV_PRIME = 0x100000001b3n;
const MAX_SAFE_HASH = 0xffffffffffffffffn;

/**
 * Cheap structural fingerprint for the fields the group projection consults.
 * This is the TypeScript counterpart of Waku's transcript_rows_fingerprint:
 * text-bearing rows are represented by their identity plus cheap shape data,
 * while the cache separately checks row references before reusing a result.
 * No serialization of the transcript is performed on the render path.
 */
export function transcriptRenderFingerprint(input: TranscriptRenderInput): number {
  let hash = FNV_OFFSET;
  hash = mixString(hash, input.density);
  hash = mix(hash, input.messages.length);
  for (const message of input.messages) {
    hash = mixString(hash, message.$kind);
    hash = mixString(hash, message.id);
    if ("content" in message) {
      // Content is painted by the row, but only its cheap shape belongs in
      // this structural fingerprint. Reference replacement is the precise
      // invalidation signal for streamed text.
      hash = mix(hash, message.content.length);
      hash = mix(hash, message.content.trim().length === 0 ? 1 : 0);
    }
    if (message.$kind === "assistant") {
      hash = mix(hash, message.toolCalls.length);
      for (const call of message.toolCalls) {
        hash = mixString(hash, call.id);
        hash = mixString(hash, call.name);
        hash = mix(hash, call.argumentsJson.length);
      }
    } else if (message.$kind === "tool") {
      hash = mixString(hash, message.toolCallId);
      hash = mix(hash, message.summary.length);
      hash = mix(hash, message.isError ? 1 : 0);
    } else if (message.$kind === "compact") {
      hash = mix(hash, message.summary.length);
      hash = mix(hash, message.droppedCount);
    }
  }
  if (input.pendingQuestion) {
    hash = mixString(hash, input.pendingQuestion.questionId);
    hash = mix(hash, input.pendingQuestion.questions.length);
    for (const question of input.pendingQuestion.questions) {
      hash = mix(hash, question.header.length);
      hash = mix(hash, question.question.length);
      hash = mix(hash, question.options.length);
      hash = mix(hash, question.multiSelect ? 1 : 0);
    }
  }
  return Number(hash & MAX_SAFE_HASH);
}

export function createTranscriptRenderCache(): TranscriptRenderCache {
  return {
    fingerprint: null,
    groups: [],
    messageRefs: null,
    pendingQuestion: null,
    density: null,
  };
}

/**
 * Build the visible row projection once per semantic transcript snapshot.
 * Message and question inputs are immutable snapshots; reference comparison is
 * what lets a streaming update invalidate only the changed snapshot while an
 * unrelated parent render reuses the exact group objects for memoized rows.
 */
export function projectTranscriptGroups(
  cache: TranscriptRenderCache,
  input: TranscriptRenderInput,
): readonly TranscriptGroup[] {
  const sameMessages = cache.messageRefs !== null
    && cache.messageRefs.length === input.messages.length
    && input.messages.every((message, index) => cache.messageRefs?.[index] === message);
  if (
    sameMessages
    && cache.pendingQuestion === input.pendingQuestion
    && cache.density === input.density
  ) {
    return cache.groups;
  }

  const groups = groupTranscriptMessages(
    input.messages as TranscriptMessage[],
    input.pendingQuestion,
    input.density,
  );
  cache.fingerprint = transcriptRenderFingerprint(input);
  cache.groups = groups;
  cache.messageRefs = input.messages;
  cache.pendingQuestion = input.pendingQuestion;
  cache.density = input.density;
  return groups;
}

/**
 * Produce measured absolute-row positions. The caller owns the measurements;
 * this function only folds already-rendered row sizes into a layout, just as
 * Waku's ListState consumes measured rows without asking the daemon for them.
 */
export function layoutTranscriptRows(
  rowCount: number,
  measuredSizes: ReadonlyMap<number, number>,
  estimate = TRANSCRIPT_ROW_ESTIMATE_PX,
  gap = 0,
): { totalSize: number; rows: readonly TranscriptRowLayout[] } {
  const rows: TranscriptRowLayout[] = [];
  let totalSize = 0;
  for (let index = 0; index < rowCount; index += 1) {
    const size = measuredSizes.get(index) ?? estimate;
    rows.push({ index, start: totalSize, size });
    totalSize += size;
    if (index + 1 < rowCount) totalSize += gap;
  }
  return { totalSize, rows };
}

/**
 * Select only the visible rows plus bounded overdraw. `starts` must be the
 * layout returned by layoutTranscriptRows; binary search keeps viewport work
 * independent of the number of rows already measured.
 */
export function transcriptViewportWindow(
  layout: { totalSize: number; rows: readonly TranscriptRowLayout[] },
  scrollTop: number,
  viewportHeight: number,
  overscan = TRANSCRIPT_OVERSCAN_ROWS,
): TranscriptViewportWindow {
  if (layout.rows.length === 0) {
    return { totalSize: layout.totalSize, first: 0, last: 0, rows: [] };
  }
  const top = Math.max(0, scrollTop);
  const bottom = top + Math.max(0, viewportHeight);
  const firstVisible = Math.max(0, upperBound(layout.rows, top) - 1);
  const lastVisible = Math.min(layout.rows.length, upperBound(layout.rows, bottom) + 1);
  const first = Math.max(0, firstVisible - overscan);
  const last = Math.min(layout.rows.length, lastVisible + overscan);
  return {
    totalSize: layout.totalSize,
    first,
    last,
    rows: layout.rows.slice(first, last),
  };
}

function upperBound(rows: readonly TranscriptRowLayout[], target: number): number {
  let low = 0;
  let high = rows.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (rows[middle]!.start <= target) low = middle + 1;
    else high = middle;
  }
  return low;
}

function mix(hash: bigint, value: number): bigint {
  return ((hash ^ BigInt(value >>> 0)) * FNV_PRIME) & MAX_SAFE_HASH;
}

function mixString(hash: bigint, value: string): bigint {
  let next = mix(hash, value.length);
  for (const character of value) next = mix(next, character.codePointAt(0) ?? 0);
  return next;
}
