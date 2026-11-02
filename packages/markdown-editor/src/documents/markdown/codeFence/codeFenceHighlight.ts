import { Plugin, PluginKey, type EditorState } from "prosemirror-state";
import { Decoration, DecorationSet } from "prosemirror-view";
import type { Node as ProseMirrorNode } from "prosemirror-model";
import { scheduleAfterPaint } from "@rightkit/document-core/app/deferredMount";
import { ensureShikiLanguage, getShikiHighlighter } from "@rightkit/doc-render";

import { SHIKI_DARK_THEME, SHIKI_LIGHT_THEME } from "../shikiThemes.js";

/**
 * Contract 14 §5.2.2 — native `code_fence` syntax highlighting.
 *
 * Highlighting is presentation-only decoration over the node's text:
 *  - lazy: Shiki loads only when a fence needs a token pass,
 *  - bounded: an LRU token cache keyed by language + content hash,
 *  - asynchronous relative to the text commit: the text transaction lands first
 *    and highlight work runs after paint, so typing is never blocked,
 *  - priority: the active (selection-containing) fence is tokenized first, then
 *    the remaining fences in document order,
 *  - stale-job-safe: a job recomputes decorations from the current doc at
 *    completion time and carries a monotonic job id.
 *
 * It never creates a second editor state and never mutates the document — it
 * only refreshes the plugin's decoration set via a metadata-only transaction.
 */

const HIGHLIGHT_META = "viewright:code-fence-highlight";

interface TokenSpec {
  /** Content text of the token. */
  content: string;
  /** Start offset in the code string. */
  offset: number;
  /** Light/dark theme colors (CSS hex). */
  light: string;
  dark: string;
}

interface CodeFenceHighlightState {
  docSignature: string;
  decorations: DecorationSet;
}

function docSignature(state: EditorState): string {
  // Cheap structural fingerprint; per-fence content is covered by the token cache key.
  return `${state.doc.childCount}:${state.doc.content.size}`;
}

export const codeFenceHighlightPluginKey = new PluginKey<CodeFenceHighlightState>(
  "viewright-code-fence-highlight",
);

type ThemedLines = ReadonlyArray<
  ReadonlyArray<{ content: string; offset: number; variants: Record<string, { color?: string }> }>
>;

// Token passes stay lazy & viewport-gated; doc changes map existing decorations before
// touched fences are cleared so enabling this cannot expose stale token positions.
const FENCE_SYNTAX_TOKENS_ENABLED = true;
export const CODE_FENCE_HIGHLIGHT_MAX_CHARS = 32_000;
export const CODE_FENCE_HIGHLIGHT_MAX_LINES = 1_000;

function isFenceWithinHighlightBudget(site: FenceSite): boolean {
  if (site.content.length > CODE_FENCE_HIGHLIGHT_MAX_CHARS) return false;
  let lines = site.content.length > 0 ? 1 : 0;
  for (const character of site.content) {
    if (character === "\n" && ++lines > CODE_FENCE_HIGHLIGHT_MAX_LINES) return false;
  }
  return true;
}

/** Tokenizes one fence with the shared lazy Shiki highlighter; unknown languages stay plain text. */
async function tokenizeFence(site: FenceSite): Promise<readonly TokenSpec[]> {
  if (!FENCE_SYNTAX_TOKENS_ENABLED || !isFenceWithinHighlightBudget(site)) return [];
  const hl = await getShikiHighlighter();
  const lang = await ensureShikiLanguage(hl, site.language || "text");
  if (lang === null) return [];
  const lines = hl.codeToTokensWithThemes(site.content, {
    lang,
    // Keyed by theme name: Shiki names each token's `variants` entry after these keys.
    themes: { [SHIKI_LIGHT_THEME]: SHIKI_LIGHT_THEME, [SHIKI_DARK_THEME]: SHIKI_DARK_THEME },
  });
  return tokensFromLines(lines);
}

// ── Bounded token cache (language + content hash → token spec list) ─────────
const TOKEN_CACHE = new Map<string, readonly TokenSpec[]>();
export const CODE_FENCE_TOKEN_CACHE_MAX = 1_024;

export interface CodeFenceHighlightMetrics {
  readonly jobs: number;
  readonly completedJobs: number;
  readonly cancelledJobs: number;
  readonly cacheHits: number;
  readonly cacheMisses: number;
  readonly offscreenBatches: number;
  readonly cacheSize: number;
  readonly firstVisibleHighlightMs: number | null;
  readonly highlighterLoadMs: number | null;
  readonly highlightLatencyMs: number | null;
  readonly offscreenTimeMs: number | null;
}

let metrics: {
  jobs: number;
  completedJobs: number;
  cancelledJobs: number;
  cacheHits: number;
  cacheMisses: number;
  offscreenBatches: number;
  firstVisibleHighlightMs: number | null;
  highlighterLoadMs: number | null;
  highlightLatencyMs: number | null;
  offscreenTimeMs: number | null;
} = {
  jobs: 0,
  completedJobs: 0,
  cancelledJobs: 0,
  cacheHits: 0,
  cacheMisses: 0,
  offscreenBatches: 0,
  firstVisibleHighlightMs: null,
  highlighterLoadMs: null,
  highlightLatencyMs: null,
  offscreenTimeMs: null,
};

export function resetCodeFenceHighlightMetrics(): void {
  TOKEN_CACHE.clear();
  metrics = {
    jobs: 0,
    completedJobs: 0,
    cancelledJobs: 0,
    cacheHits: 0,
    cacheMisses: 0,
    offscreenBatches: 0,
    firstVisibleHighlightMs: null,
    highlighterLoadMs: null,
    highlightLatencyMs: null,
    offscreenTimeMs: null,
  };
}

export function codeFenceHighlightMetrics(): CodeFenceHighlightMetrics {
  return { ...metrics, cacheSize: TOKEN_CACHE.size };
}

function tokenKey(lang: string, content: string): string {
  return `${lang}::${content.length}::${contentHash(content)}`;
}

function contentHash(content: string): string {
  // FNV-1a 32-bit — compact and deterministic for a cache key.
  let hash = 0x811c9dc5;
  for (let i = 0; i < content.length; i++) {
    hash ^= content.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0).toString(16);
}

function rememberTokens(key: string, specs: readonly TokenSpec[]): void {
  TOKEN_CACHE.delete(key);
  TOKEN_CACHE.set(key, specs);
  if (TOKEN_CACHE.size > CODE_FENCE_TOKEN_CACHE_MAX) {
    const oldest = TOKEN_CACHE.keys().next().value;
    if (oldest !== undefined) TOKEN_CACHE.delete(oldest);
  }
}

function cachedTokens(key: string): readonly TokenSpec[] | undefined {
  const value = TOKEN_CACHE.get(key);
  if (value === undefined) {
    metrics.cacheMisses++;
    return undefined;
  }
  metrics.cacheHits++;
  // Touch entry to make this a true bounded LRU, not insertion-order eviction.
  TOKEN_CACHE.delete(key);
  TOKEN_CACHE.set(key, value);
  return value;
}

function tokensFromLines(lines: ThemedLines): readonly TokenSpec[] {
  const out: TokenSpec[] = [];
  for (const line of lines) {
    for (const token of line) {
      const light = token.variants[SHIKI_LIGHT_THEME]?.color;
      const dark = token.variants[SHIKI_DARK_THEME]?.color;
      if (!light && !dark) continue;
      out.push({
        content: token.content,
        offset: token.offset,
        light: light ?? "inherit",
        dark: dark ?? "inherit",
      });
    }
  }
  return out;
}

/** Render one fence's token specs as decorations at absolute positions. */
function fenceDecorations(pos: number, specs: readonly TokenSpec[]): Decoration[] {
  const decos: Decoration[] = [];
  for (const spec of specs) {
    const from = pos + 1 + spec.offset;
    const to = from + spec.content.length;
    if (to <= from) continue;
    decos.push(
      Decoration.inline(from, to, {
        class: "vr-code-token",
        style: `--shiki-light:${spec.light};--shiki-dark:${spec.dark};`,
      }),
    );
  }
  return decos;
}

interface FenceSite {
  node: ProseMirrorNode;
  pos: number;
  key: string;
  language: string;
  content: string;
}

/** Collect fence sites; the active (selection-containing) one comes first. */
function collectFenceSites(state: EditorState): FenceSite[] {
  const sites: FenceSite[] = [];
  const active = state.selection.$from.pos;
  state.doc.descendants((node, pos) => {
    if (node.type.name !== "code_fence") return true;
    const language = String(node.attrs.language ?? "");
    const content = node.textContent;
    sites.push({ node, pos, key: tokenKey(language || "text", content), language, content });
    return true;
  });
  const activeSite = sites.find((s) => s.pos <= active && active < s.pos + s.node.nodeSize);
  if (activeSite && sites.length > 1) {
    return [activeSite, ...sites.filter((s) => s !== activeSite)];
  }
  return sites;
}

function isFenceInViewport(
  view: { nodeDOM: (pos: number) => Node | null | undefined },
  pos: number,
): boolean {
  try {
    const dom = view.nodeDOM(pos);
    // A missing or detached node has no reliable viewport geometry. Treat it as
    // deferred work; only the active fence is promoted by the caller. This
    // prevents cold mount from treating every off-screen fence as visible while
    // the PM DOM is still being laid out.
    if (!(dom instanceof HTMLElement)) return false;
    const rect = dom.getBoundingClientRect();
    if (rect.width === 0 && rect.height === 0) return false;
    const vh = typeof window !== "undefined" ? window.innerHeight : 800;
    // Visible plus 400px prefetch — near-viewport fences highlight next.
    return rect.bottom >= -400 && rect.top <= vh + 400;
  } catch {
    return true;
  }
}

/** Build the full decoration set for the current doc, warming the cache for every fence. */
function buildDecorations(state: EditorState): DecorationSet {
  const decos: Decoration[] = [];
  for (const site of collectFenceSites(state)) {
    const cached = cachedTokens(site.key);
    if (cached) decos.push(...fenceDecorations(site.pos, cached));
  }
  return DecorationSet.create(state.doc, decos);
}

export function codeFenceHighlightPlugin(): Plugin<CodeFenceHighlightState> {
  let jobCounter = 0;
  return new Plugin<CodeFenceHighlightState>({
    key: codeFenceHighlightPluginKey,
    state: {
      init: (_config, state) => ({
        docSignature: docSignature(state),
        decorations: DecorationSet.empty,
      }),
      apply: (tr, value, _oldState, newState) => {
        const meta = tr.getMeta(HIGHLIGHT_META) as
          | { docSignature: string; decorations: DecorationSet }
          | undefined;
        if (meta && meta.docSignature === docSignature(newState)) {
          return { docSignature: meta.docSignature, decorations: meta.decorations };
        }
        if (tr.docChanged) {
          // Keep unaffected token spans aligned through inserts/deletes. Decorations in a
          // changed fence are removed until its lazy pass retokenizes current content; retaining
          // them for one frame is what caused the original colour/character misalignment.
          const mapped = value.decorations.map(tr.mapping, tr.doc);
          const oldSitesByMappedPos = new Map<number, FenceSite>();
          for (const site of collectFenceSites(_oldState)) {
            const mappedPos = tr.mapping.mapResult(site.pos, 1);
            if (!mappedPos.deleted) oldSitesByMappedPos.set(mappedPos.pos, site);
          }
          const changedRanges: Array<{ from: number; to: number }> = [];
          for (const site of collectFenceSites(newState)) {
            const oldSite = oldSitesByMappedPos.get(site.pos);
            if (!oldSite || oldSite.key !== site.key) {
              changedRanges.push({ from: site.pos + 1, to: site.pos + site.node.nodeSize - 1 });
            }
          }
          if (changedRanges.length > 0) {
            const stale = mapped
              .find()
              .filter((deco) =>
                changedRanges.some((range) => deco.from < range.to && range.from < deco.to),
              );
            return {
              docSignature: docSignature(newState),
              decorations: stale.length > 0 ? mapped.remove(stale) : mapped,
            };
          }
          return { docSignature: docSignature(newState), decorations: mapped };
        }
        return value;
      },
    },
    props: {
      decorations: (state) => {
        const value = codeFenceHighlightPluginKey.getState(state);
        if (value && value.docSignature === docSignature(state)) return value.decorations;
        return buildDecorations(state);
      },
    },
    view: (view) => {
      let cancelScheduled = () => {};
      const schedule = () => {
        cancelScheduled();
        const capturedState = view.state;
        const jobId = ++jobCounter;
        const jobStartedAt = performance.now();
        metrics.jobs++;
        cancelScheduled = scheduleAfterPaint(() => {
          void (async () => {
            // Contract 14 §5.2.2 — viewport-gated highlighting. The active
            // fence and viewport/near-viewport fences are tokenized first;
            // off-screen fences are deferred to a later paint so cold mount
            // and typing are never blocked by off-screen work. One shared
            // viewport check (not one observer per block) is sufficient for
            // the current scale; an IntersectionObserver registry can replace
            // the rect check if per-frame cost grows.
            const sites = collectFenceSites(capturedState);
            const visibleSites: FenceSite[] = [];
            const offscreenSites: FenceSite[] = [];
            for (const site of sites) {
              if (cachedTokens(site.key)) continue;
              if (!isFenceWithinHighlightBudget(site)) continue;
              const active = capturedState.selection.$from.pos;
              const isActive = site.pos <= active && active < site.pos + site.node.nodeSize;
              if (isActive || isFenceInViewport(view, site.pos)) visibleSites.push(site);
              else offscreenSites.push(site);
            }
            if (visibleSites.length === 0 && offscreenSites.length === 0) {
              metrics.completedJobs++;
              return;
            }
            const highlighterStartedAt = performance.now();
            if (FENCE_SYNTAX_TOKENS_ENABLED) {
              try {
                await getShikiHighlighter();
              } catch {
                // Each fence retries through tokenizeFence and falls back to plain text.
              }
            }
            metrics.highlighterLoadMs ??= performance.now() - highlighterStartedAt;
            // Visible / active first — this is the first useful paint.
            for (const site of visibleSites) {
              if (cachedTokens(site.key)) continue;
              if (jobId !== jobCounter) {
                metrics.cancelledJobs++;
                return;
              }
              try {
                rememberTokens(site.key, await tokenizeFence(site));
              } catch {
                rememberTokens(site.key, []);
              }
            }
            // Publish visible decorations before touching off-screen work.
            if (jobId !== jobCounter) {
              metrics.cancelledJobs++;
              return;
            }
            {
              const current = view.state;
              const decorations = buildDecorations(current);
              const tr = current.tr.setMeta(HIGHLIGHT_META, {
                docSignature: docSignature(current),
                decorations,
              });
              if (tr.docChanged === false) view.dispatch(tr);
              metrics.firstVisibleHighlightMs ??= performance.now() - jobStartedAt;
            }
            // Off-screen fences only when spare scheduled work exists — they
            // must not delay the visible batch above.
            // Off-screen work is explicitly budgeted. A large document cannot
            // turn one cold-mount job into an unbounded Shiki burst.
            const OFFSCREEN_BATCH_SIZE = 2;
            const offscreenStartedAt = offscreenSites.length > 0 ? performance.now() : null;
            for (let offset = 0; offset < offscreenSites.length; offset += OFFSCREEN_BATCH_SIZE) {
              await new Promise<void>((resolve) => {
                scheduleAfterPaint(resolve);
              });
              if (jobId !== jobCounter) {
                metrics.cancelledJobs++;
                return;
              }
              metrics.offscreenBatches++;
              for (const site of offscreenSites.slice(offset, offset + OFFSCREEN_BATCH_SIZE)) {
                if (cachedTokens(site.key)) continue;
                if (jobId !== jobCounter) {
                  metrics.cancelledJobs++;
                  return;
                }
                try {
                  rememberTokens(site.key, await tokenizeFence(site));
                } catch {
                  rememberTokens(site.key, []);
                }
              }
              if (jobId !== jobCounter) {
                metrics.cancelledJobs++;
                return;
              }
              const current = view.state;
              const decorations = buildDecorations(current);
              const tr = current.tr.setMeta(HIGHLIGHT_META, {
                docSignature: docSignature(current),
                decorations,
              });
              if (tr.docChanged === false) view.dispatch(tr);
            }
            if (offscreenStartedAt !== null) {
              metrics.offscreenTimeMs = performance.now() - offscreenStartedAt;
            }
            metrics.highlightLatencyMs = performance.now() - jobStartedAt;
            metrics.completedJobs++;
          })();
        });
      };
      // Cold mount is deferred until after useful PM paint and follows the same
      // active/viewport/near-viewport priority as subsequent edits.
      schedule();
      return {
        update: (view, prevState) => {
          if (view.state.doc === prevState.doc) return;
          schedule();
        },
        destroy: () => {
          // In-flight highlighter promises may resolve after the EditorView is
          // gone; invalidate their job before cancelling the next paint.
          jobCounter++;
          cancelScheduled();
        },
      };
    },
  });
}
