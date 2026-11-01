import { Plugin, PluginKey, type EditorState } from "prosemirror-state";
import { Decoration, DecorationSet, type EditorView } from "prosemirror-view";
import type { Node as PMNode } from "prosemirror-model";
import { findLiteralMatches } from "@rightkit/document-core/documents/searchUtils";
import type { SearchOptions, SearchResult } from "@rightkit/document-core/documents/types";

// Find decorations live in a the host app-owned ProseMirror plugin appended to the editor's live view.
// We use the SAME prosemirror-state/view instances the vendored editor uses (pnpm dedupes them),
// so the plugin + decorations interoperate. The vendored editor is never modified.
const findKey = new PluginKey<DecorationSet>("vrDocxFind");

interface Range {
  from: number;
  to: number;
}

interface FlattenedBlock {
  /** The block's inline text flattened into one string. */
  str: string;
  /** map[i] = absolute ProseMirror position of str[i]. */
  map: number[];
}

// Flattening a doc into per-textblock strings + position maps is the expensive part of a scan (a
// full tree walk). The result only depends on the doc's *content*, and ProseMirror always produces a
// brand-new doc node object on every transaction that changes content — so keying a WeakMap on the
// doc node reference gives us free, correct-by-construction invalidation: unchanged doc → cache hit,
// edited doc → new object → cache miss → rebuilt once. WeakMap also means stale entries for
// discarded doc revisions are simply garbage-collected, no manual eviction needed.
const flattenCache = new WeakMap<PMNode, FlattenedBlock[]>();

function flattenTextBlocks(doc: PMNode): FlattenedBlock[] {
  const blocks: FlattenedBlock[] = [];
  doc.descendants((node, pos) => {
    if (!node.isTextblock) return true; // descend into containers (doc, tables, cells…)
    // Flatten the block's inline text into one string + a per-char absolute-position map.
    let str = "";
    const map: number[] = [];
    node.forEach((child, offset) => {
      if (child.isText && child.text) {
        const base = pos + 1 + offset; // absolute pos of this text node's first char
        for (let i = 0; i < child.text.length; i++) {
          str += child.text[i];
          map.push(base + i);
        }
      }
    });
    blocks.push({ str, map });
    return false; // a textblock's children are inline — already handled above
  });
  return blocks;
}

function getFlattenedBlocks(doc: PMNode): FlattenedBlock[] {
  let blocks = flattenCache.get(doc);
  if (!blocks) {
    blocks = flattenTextBlocks(doc);
    flattenCache.set(doc, blocks);
  }
  return blocks;
}

/**
 * Scan the document for `query` honoring match-case / whole-word, returning absolute ProseMirror
 * {from,to} ranges. Matching is per-TEXTBLOCK (so it never spans paragraph boundaries) but DOES span
 * split inline text nodes / marked runs within a block, and includes table-cell paragraphs.
 */
export function scanDocxMatches(doc: PMNode, query: string, options: SearchOptions): Range[] {
  if (!query) return [];
  const ranges: Range[] = [];
  for (const { str, map } of getFlattenedBlocks(doc)) {
    for (const m of findLiteralMatches(str, query, options)) {
      const from = map[m.start];
      const to = map[m.end - 1];
      if (from != null && to != null) ranges.push({ from, to: to + 1 });
    }
  }
  return ranges;
}

export interface DocxFindController {
  find(query: string, options: SearchOptions): SearchResult;
  next(): SearchResult;
  previous(): SearchResult;
  replace(replacement: string): SearchResult;
  replaceAll(replacement: string): SearchResult;
  clear(): void;
  getState(): SearchResult;
}

/**
 * Build a Find controller over a live ProseMirror EditorView (handed to us by the editor via
 * onEditorViewReady). Appends a decoration plugin once; drives matches/active via transaction meta;
 * scrolls the active match into view; replace preserves the active run's marks.
 */
export function createDocxFindController(
  view: EditorView,
  scrollToPos: (pos: number) => void,
): DocxFindController {
  let ranges: Range[] = [];
  let active = -1;
  let query = "";
  let options: SearchOptions = { matchCase: false, wholeWord: false };

  const ensurePlugin = () => {
    if (findKey.get(view.state)) return;
    const plugin = new Plugin<DecorationSet>({
      key: findKey,
      state: {
        init: () => DecorationSet.empty,
        apply(tr, set) {
          const meta = tr.getMeta(findKey) as DecorationSet | undefined;
          if (meta) return meta;
          return set.map(tr.mapping, tr.doc); // keep decorations aligned through edits
        },
      },
      props: {
        decorations: (state: EditorState) => findKey.getState(state),
      },
    });
    view.updateState(view.state.reconfigure({ plugins: view.state.plugins.concat(plugin) }));
  };

  const paint = () => {
    ensurePlugin();
    const decos = ranges.map((r, i) =>
      Decoration.inline(r.from, r.to, {
        class: i === active ? "vr-docx-search-active" : "vr-docx-search-match",
      }),
    );
    view.dispatch(view.state.tr.setMeta(findKey, DecorationSet.create(view.state.doc, decos)));
  };

  const result = (): SearchResult => ({ count: ranges.length, activeIndex: active });

  // Scroll the active match into view — decoration-only, never a selection change (a selection
  // dispatch here would steal focus from the FindBar input mid-type).
  const reveal = () => {
    const r = ranges[active];
    if (r) scrollToPos(r.from);
  };

  const rescan = (resetActive: boolean) => {
    ranges = scanDocxMatches(view.state.doc, query, options);
    if (resetActive || active < 0) active = ranges.length ? 0 : -1;
    else active = ranges.length ? Math.min(active, ranges.length - 1) : -1;
    paint();
    reveal();
  };

  return {
    find(q, opts) {
      query = q;
      options = opts;
      rescan(true);
      return result();
    },
    next() {
      if (ranges.length) {
        active = (active + 1) % ranges.length;
        paint();
        reveal();
      }
      return result();
    },
    previous() {
      if (ranges.length) {
        active = (active - 1 + ranges.length) % ranges.length;
        paint();
        reveal();
      }
      return result();
    },
    replace(replacement) {
      const r = ranges[active];
      if (r) {
        const marks = view.state.doc.resolve(r.from).marks();
        const content = replacement ? view.state.schema.text(replacement, marks) : null;
        view.dispatch(
          content
            ? view.state.tr.replaceWith(r.from, r.to, content)
            : view.state.tr.delete(r.from, r.to),
        );
        rescan(false);
      }
      return result();
    },
    replaceAll(replacement) {
      const all = scanDocxMatches(view.state.doc, query, options);
      if (all.length) {
        let tr = view.state.tr;
        // Apply last→first so earlier ranges' positions stay valid as the doc shrinks/grows.
        for (let i = all.length - 1; i >= 0; i--) {
          const r = all[i];
          const marks = tr.doc.resolve(r.from).marks();
          if (replacement)
            tr = tr.replaceWith(r.from, r.to, view.state.schema.text(replacement, marks));
          else tr = tr.delete(r.from, r.to);
        }
        view.dispatch(tr);
        rescan(true);
      }
      return result();
    },
    clear() {
      ranges = [];
      active = -1;
      query = "";
      paint();
    },
    getState: result,
  };
}
