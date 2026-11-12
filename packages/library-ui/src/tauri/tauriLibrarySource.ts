// The Tauri `LibrarySource` over ScrapeRight's artifact library: the commands its Library and Detail screens call
// (`get_history_page`, `get_transcript`, `get_transcript_variant`, `delete_transcript`) and its `job://update` event.
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { createShell } from "@rightkit/tauri-shell";
import type { LibraryDetail, LibraryPageResult, LibraryQuery, LibraryRow, LibrarySource, LibraryVariant, Unsubscribe } from "../contract.js";
import type { HistoryRow, TranscriptDetail, TranscriptVariant } from "../queue/contract.js";
import { savedMediaKind } from "../queue/lib/mediaSelection.js";

/** The native command names; each can be overridden. */
export interface TauriLibraryCommands {
  historyPage: string;
  transcript: string;
  transcriptVariant: string;
  deleteTranscript: string;
}

export interface TauriLibraryOptions {
  /** Overrides of the command names (defaults are ScrapeRight's). */
  commands?: Partial<TauriLibraryCommands>;
  /** Event whose payload `{ stage }` reaching "done" means a new row exists. Default `"job://update"`. */
  changeEvent?: string;
  /** How native events reach this window. Default: Tauri's `listen`. */
  listen?: <T>(event: string, handler: (payload: T) => void) => Promise<Unsubscribe>;
  /** Copies text. Default: the RightKit shell clipboard. */
  copyText?: (text: string) => Promise<void>;
}

const COMMANDS: TauriLibraryCommands = {
  historyPage: "get_history_page",
  transcript: "get_transcript",
  transcriptVariant: "get_transcript_variant",
  deleteTranscript: "delete_transcript",
};

/** ScrapeRight orders pages in the engine; the list's `variant` is the order (`HistorySort`). */
export const LIBRARY_SORT_VARIANTS: LibraryVariant[] = [
  { key: "newest", label: "Newest" },
  { key: "oldest", label: "Oldest" },
  { key: "longest", label: "Longest" },
  { key: "shortest", label: "Shortest" },
];
const SORTS = new Set(LIBRARY_SORT_VARIANTS.map((v) => v.key));

interface HistoryPage {
  rows: HistoryRow[];
  next_cursor: string | null;
  total?: number | null;
}

function notMapped(what: string): Error {
  return new Error(`@rightkit/library-ui/tauri: ${what} is not mapped: ScrapeRight has no native call for it.`);
}

// The engine's `created_at` is an ISO timestamp.
function toRow(r: HistoryRow): LibraryRow {
  const at = Date.parse(r.created_at);
  return {
    id: r.id,
    text: r.title || r.snippet || r.url,
    timestampMs: Number.isNaN(at) ? 0 : at,
    origin: { label: r.kind, kind: "file" },
    durationMs: r.duration_s != null ? Math.round(r.duration_s * 1000) : null,
  };
}

export function tauriLibrarySource(options: TauriLibraryOptions = {}): LibrarySource {
  const cmd = { ...COMMANDS, ...options.commands };
  const listen = options.listen ?? (<T>(event: string, handler: (payload: T) => void) => tauriListen<T>(event, (e) => handler(e.payload)));
  let shell: ReturnType<typeof createShell> | null = null;
  // The engine pages by cursor, the list by offset: cursors seen so far, per search and order, keyed by offset.
  const cursors = new Map<string, string | null>();
  const keyOf = (q: string, sort: string, offset: number) => `${sort}\n${q}\n${offset}`;
  const page = (query: string, cursor: string | null, limit: number, sort: string) =>
    invoke<HistoryPage>(cmd.historyPage, { query, cursor, limit, sort });

  return {
    async query(q: LibraryQuery): Promise<LibraryPageResult> {
      const sort = SORTS.has(q.variant) ? q.variant : "newest";
      const text = q.search.trim();
      let cursor: string | null = null;
      if (q.offset > 0) {
        const known = cursors.get(keyOf(text, sort, q.offset));
        if (known === undefined) {
          // Not reached through this source yet: walk from the first page.
          for (let at = 0; at < q.offset; ) {
            const step = await page(text, cursor, Math.min(q.limit, q.offset - at), sort);
            at += step.rows.length;
            cursors.set(keyOf(text, sort, at), step.next_cursor);
            cursor = step.next_cursor;
            if (!cursor) return { rows: [], total: at, hasMore: false };
          }
        } else {
          cursor = known;
        }
        if (!cursor) return { rows: [], total: q.offset, hasMore: false };
      }
      const result = await page(text, cursor, q.limit, sort);
      const loaded = q.offset + result.rows.length;
      cursors.set(keyOf(text, sort, loaded), result.next_cursor);
      return {
        rows: result.rows.map(toRow),
        total: typeof result.total === "number" ? result.total : loaded,
        hasMore: Boolean(result.next_cursor),
      };
    },
    // A finished job adds a row; the engine sends no other history change event.
    subscribe: (onChange) =>
      listen<{ stage?: string }>(options.changeEvent ?? "job://update", (e) => {
        if (e.stage === "done") onChange();
      }),
    copyText: options.copyText ?? ((text) => (shell ??= createShell()).clipboard.write({ text })),
    remove: (id) => invoke<void>(cmd.deleteTranscript, { id }),
    // ScrapeRight keeps artifacts as recorded: it has no edit, export-all or free-text save command.
    update: () => Promise.reject(notMapped("update")),
    exportAll: () => Promise.reject(notMapped("exportAll")),
    saveText: () => Promise.reject(notMapped("saveText")),
    async detail(id: string): Promise<LibraryDetail> {
      // The slim reply carries only a preview of the text; the full body is a variant. A failed variant keeps the preview.
      const slim = await invoke<TranscriptDetail>(cmd.transcript, { id });
      const full = await invoke<TranscriptVariant>(cmd.transcriptVariant, { id, kind: "text" }).catch(() => null);
      const mediaPath = slim.media_path ?? null;
      return {
        id: slim.id,
        title: slim.title || slim.url,
        text: full?.text ?? slim.text,
        timedText: slim.timed_text ?? null,
        markdown: slim.md_text ?? null,
        timedMarkdown: slim.timed_md_text ?? null,
        srt: slim.srt ?? null,
        mediaUrl: mediaPath ? convertFileSrc(mediaPath) : null,
        mediaKind: mediaPath ? savedMediaKind(slim.media_meta, mediaPath) : null,
        images: slim.slides.map((file) => convertFileSrc(file)),
        sourceUrl: /^https?:\/\//i.test(slim.url) ? slim.url : null,
      };
    },
  };
}
