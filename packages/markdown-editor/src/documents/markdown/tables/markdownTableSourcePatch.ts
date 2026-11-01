import type { Node as ProseMirrorNode } from "prosemirror-model";

export interface MarkdownTablePatch {
  readonly from: number;
  readonly to: number;
  readonly insert: string;
}

export interface MarkdownTableRangeAttrs {
  readonly sourceFrom: number;
  readonly sourceTo: number;
  readonly prefixFrom: number;
  readonly prefixTo: number;
}

/**
 * Compute a minimal local replacement for a changed table block. Prefix and
 * unrelated document bytes stay outside the patch.
 */
export function computeMarkdownTableSourcePatch(
  table: ProseMirrorNode,
  serialized: string,
): MarkdownTablePatch | null {
  const range = readMarkdownTableRange(table);
  if (!range) return null;
  const previous = String(table.attrs.rawSource ?? "");
  if (previous.length !== range.sourceTo - range.sourceFrom) {
    return {
      from: range.prefixFrom,
      to: range.sourceTo,
      insert: String(table.attrs.prefix ?? "") + serialized,
    };
  }
  let start = 0;
  while (
    start < previous.length &&
    start < serialized.length &&
    previous[start] === serialized[start]
  )
    start++;
  let end = 0;
  while (
    end < previous.length - start &&
    end < serialized.length - start &&
    previous[previous.length - end - 1] === serialized[serialized.length - end - 1]
  )
    end++;
  return {
    from: range.sourceFrom + start,
    to: range.sourceTo - end,
    insert: serialized.slice(start, serialized.length - end),
  };
}

export function readMarkdownTableRange(node: ProseMirrorNode): MarkdownTableRangeAttrs | null {
  const attrs = node.attrs as Partial<MarkdownTableRangeAttrs>;
  const { sourceFrom, sourceTo, prefixFrom, prefixTo } = attrs;
  if (
    typeof sourceFrom !== "number" ||
    typeof sourceTo !== "number" ||
    typeof prefixFrom !== "number" ||
    typeof prefixTo !== "number" ||
    !Number.isInteger(sourceFrom) ||
    !Number.isInteger(sourceTo) ||
    !Number.isInteger(prefixFrom) ||
    !Number.isInteger(prefixTo) ||
    sourceFrom < 0 ||
    sourceTo < sourceFrom ||
    prefixFrom < 0 ||
    prefixTo < prefixFrom
  ) {
    return null;
  }
  return {
    sourceFrom,
    sourceTo,
    prefixFrom,
    prefixTo,
  };
}
