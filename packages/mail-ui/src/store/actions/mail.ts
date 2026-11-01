// Mail store actions: account/health refresh, thread list loading + pagination,
// opening a thread (with inline images, attachments, trust check, labels), and the
// thread mutations (star, select, bulk actions, empty/delete-forever, nav, act-on-open).
import {
  FOLDER_LABEL,
  RISKY_EXT,
  computeListSource,
  keyOf,
} from "../../lib/mail";
import {
  unreadCounts,
  listThreads,
  loadOlder,
  markThreadRead,
  markThreadUnread,
  scanFile,
  bulkAction,
  archiveThread,
  trashThread,
  folderCount,
  appendDiag,
  deleteDrafts,
  type Thread,
} from "../../lib/ipc";
import { appendPage, fetchNextSearchPage } from "../../lib/searchPaging";
import type { AppState } from "../types";
import { BULK_INVERSE, BULK_LABEL } from "../types";
import type { ThreadRef } from "../../lib/ipc/mail";
import type { StoreGet } from "../mk";
import { showDialog } from "../../hooks/dialog";
import { openThreadAction } from "./mail/openThread";
import {
  refreshAction,
  refreshAfterSyncAction,
  refreshStaticSettingsAction,
} from "./mail/refresh";
import {
  appendSlowDiag,
  mergeThreadRows,
  nowMs,
  perfVisibility,
  SLOW_SCOPE_MS,
} from "./mail/helpers";
import { hostAppName } from "../../appName";

// Progress guard for "load older" pagination. `load_older` returns the count of
// messages UPSERTED (re-fetched duplicates included), so a `before:<oldest>`
// query that re-returns the same boundary rows reports count > 0 forever — which
// the infinite-scroll effect turns into an endless reload loop (the "Loading
// older…" hang at Drafts). We remember the boundary we last fetched at, per
// scope, and stop if a fetch doesn't move it.
let lastOlderFetch: { scope: string; oldest: number } | null = null;
let loadThreadsSeq = 0;
const PERMANENT_DELETE_DISABLED =
  `Permanent delete is not available yet. ${hostAppName()} keeps Trash and Spam recoverable until the broader Gmail delete scope is configured.`;
type MailActions = Pick<
  AppState,
  | "refresh"
  | "refreshAfterSync"
  | "refreshStaticSettings"
  | "loadThreads"
  | "loadOlderMessages"
  | "openThreadView"
  | "saveAndScan"
  | "toggleStar"
  | "toggleStarRow"
  | "toggleSelect"
  | "toggleSelectAll"
  | "runBulk"
  | "emptyFolderFlow"
  | "deleteForeverFlow"
  | "navTo"
  | "actOnOpen"
>;

export function mailActions(get: StoreGet): MailActions {
  return {
    refresh: () => refreshAction(get),
    refreshAfterSync: () => refreshAfterSyncAction(get),
    refreshStaticSettings: () => refreshStaticSettingsAction(get),
    loadThreads: async () => {
      const s = get();
      const seq = ++loadThreadsSeq;
      const started = nowMs();
      const label = s.activeLabel ?? FOLDER_LABEL[s.folder];
      const account = s.activeLabel
        ? s.activeAccount
        : s.folder === "all"
          ? null
          : s.activeAccount;
      const isInboxView =
        !s.activeLabel && (s.folder === "all" || s.folder === "inbox");
      const cat = isInboxView && !s.unreadOnly ? s.category : null;
      s.setThreadsLoading(true);
      try {
        const next = await listThreads(account, label, cat);
        const ms = Math.round(nowMs() - started);
        if (ms >= SLOW_SCOPE_MS) {
          appendSlowDiag(
            `THREAD load_threads ms=${ms} rows=${next.length} account=${account ?? "*"} label=${label} category=${cat ?? "-"} visibility=${perfVisibility()}`,
          );
        }
        if (seq !== loadThreadsSeq) return;
        s.setThreads((prev) => mergeThreadRows(prev, next));
      } catch (e) {
        if (seq === loadThreadsSeq) s.setErr(String(e));
      } finally {
        if (seq === loadThreadsSeq) s.setThreadsLoading(false);
      }
    },
    loadOlderMessages: async () => {
      const s = get();
      if (s.loadingOlder || s.noMoreOlder) return;
      if (s.query.trim()) {
        // Search results page by keyset cursor, not by the folder boundary.
        s.setLoadingOlder(true);
        try {
          const page = await fetchNextSearchPage();
          if (page) s.setResults((prev) => appendPage(prev, page.rows));
          if (!page || page.done) s.setNoMoreOlder(true);
        } catch (e) {
          s.setErr(String(e));
          s.setNoMoreOlder(true);
        } finally {
          s.setLoadingOlder(false);
        }
        return;
      }
      const label = s.activeLabel ?? FOLDER_LABEL[s.folder];
      if (s.threads.length === 0) return;
      const oldestMs = Math.min(...s.threads.map((t) => t.last_message_at));
      const scope = `${s.folder}|${s.activeLabel ?? ""}|${s.activeAccount ?? ""}|${s.category ?? ""}|${s.unreadOnly ? "unread" : "all"}`;
      // Reset the boundary memo when the scope changes (new folder/account/label).
      if (lastOlderFetch && lastOlderFetch.scope !== scope)
        lastOlderFetch = null;
      // We already fetched at this exact boundary and it didn't move — the server
      // is re-returning the same messages. Stop, or we loop forever.
      if (lastOlderFetch && lastOlderFetch.oldest === oldestMs) {
        s.setNoMoreOlder(true);
        appendDiag(
          `load-older: no progress at ${oldestMs} (${scope}) — halting`,
        ).catch(() => {});
        return;
      }
      const ids = s.activeLabel
        ? s.activeAccount
          ? [s.activeAccount]
          : []
        : s.folder === "all"
          ? s.accounts.map((a) => a.id)
          : s.activeAccount
            ? [s.activeAccount]
            : [];
      if (ids.length === 0) return;
      s.setLoadingOlder(true);
      const started = nowMs();
      try {
        const counts = await Promise.all(
          ids.map((id) => loadOlder(id, label, oldestMs, 50)),
        );
        const ms = Math.round(nowMs() - started);
        if (ms >= SLOW_SCOPE_MS) {
          appendSlowDiag(
            `THREAD load_older ms=${ms} counts=${counts.join(",")} scope=${scope} visibility=${perfVisibility()}`,
          );
        }
        lastOlderFetch = { scope, oldest: oldestMs };
        if (counts.reduce((a, b) => a + b, 0) === 0) s.setNoMoreOlder(true);
        else s.setReloadKey((k) => k + 1);
      } catch (e) {
        s.setErr(String(e));
        s.setNoMoreOlder(true);
      } finally {
        s.setLoadingOlder(false);
      }
    },
    openThreadView: (t) => openThreadAction(get, t),
    saveAndScan: async (label, download) => {
      const s = get();
      if (
        RISKY_EXT.test(label) &&
        (await showDialog({
          kind: "confirm",
          message: `"${label}" is an executable or script type that can harm your computer. Download anyway?`,
        })) !== "ok"
      )
        return;
      download()
        .then((path) => {
          s.showUndo(`Saved ${label} — scanning…`, [], "");
          scanFile(path)
            .then((verdict) => {
              if (verdict === "threat")
                s.setErr(
                  `Warning: Antivirus scan flagged a threat in ${label}.`,
                );
              else
                s.showUndo(
                  verdict === "clean"
                    ? `Saved ${label} (scanned clean)`
                    : `Saved ${label} (AV scan unavailable)`,
                  [],
                  "",
                );
            })
            .catch(() => {});
        })
        .catch((e) => s.setErr(String(e)));
    },
    toggleStar: () => {
      const s = get();
      if (!s.openThread) return;
      const next = !s.openStarred;
      s.setOpenStarred(next);
      bulkAction(
        [
          {
            account_id: s.openThread.account_id,
            thread_id: s.openThread.thread_id,
          },
        ],
        next ? "star" : "unstar",
      ).catch((e) => s.setErr(String(e)));
    },
    toggleStarRow: (t) => {
      const s = get();
      const next = !t.starred;
      const same = (x: Thread) =>
        x.account_id === t.account_id && x.thread_id === t.thread_id;
      s.setThreads((prev) =>
        prev.map((x) => (same(x) ? { ...x, starred: next } : x)),
      );
      s.setResults((prev) =>
        prev.map((x) => (same(x) ? { ...x, starred: next } : x)),
      );
      bulkAction(
        [{ account_id: t.account_id, thread_id: t.thread_id }],
        next ? "star" : "unstar",
      ).catch((e) => s.setErr(String(e)));
    },
    toggleSelect: (t) => {
      const k = keyOf(t);
      get().setSelected((prev) => {
        const next = new globalThis.Set(prev);
        if (next.has(k)) next.delete(k);
        else next.add(k);
        return next;
      });
    },
    toggleSelectAll: () => {
      const s = get();
      const list = computeListSource(
        s.threads,
        s.results,
        s.query,
        s.unreadOnly,
        s.sortMode,
      );
      const all =
        list.length > 0 && list.every((t) => s.selected.has(keyOf(t)));
      s.setSelected(
        all ? new globalThis.Set() : new globalThis.Set(list.map(keyOf)),
      );
    },
    runBulk: async (action, refs) => {
      const s = get();
      if (refs.length === 0) return;
      const keys = new globalThis.Set(refs.map(keyOf));
      if (action === "read" || action === "unread") {
        const u = action === "unread";
        s.setThreads((prev) =>
          prev.map((x) => (keys.has(keyOf(x)) ? { ...x, unread: u } : x)),
        );
        s.setResults((prev) =>
          prev.map((x) => (keys.has(keyOf(x)) ? { ...x, unread: u } : x)),
        );
      } else {
        // archive / trash / untrash all remove the thread from the current folder.
        s.setThreads((prev) => prev.filter((x) => !keys.has(keyOf(x))));
        s.setResults((prev) => prev.filter((x) => !keys.has(keyOf(x))));
      }
      s.setSelected(new globalThis.Set());
      try {
        await bulkAction(refs, action);
      } catch (e) {
        s.setErr(String(e));
      }
      unreadCounts()
        .then(s.setCounts)
        .catch(() => {});
      folderCount("DRAFT", s.activeAccount)
        .then(s.setDraftCount)
        .catch(() => {});
      const inverse = BULK_INVERSE[action];
      if (inverse)
        s.showUndo(`${BULK_LABEL[action] ?? ""} ${refs.length}`, refs, inverse);
    },
    emptyFolderFlow: async (label, count) => {
      const s = get();
      const where = label === "TRASH" ? "Trash" : "Spam";
      s.setErr(
        `${where} has ${count} message${count === 1 ? "" : "s"} selected for permanent deletion, but ${PERMANENT_DELETE_DISABLED}`,
      );
    },
    deleteForeverFlow: async (refs) => {
      const s = get();
      if (refs.length === 0) return;
      s.setErr(PERMANENT_DELETE_DISABLED);
    },
    navTo: (delta) => {
      const s = get();
      if (!s.openThread) return;
      const list = computeListSource(
        s.threads,
        s.results,
        s.query,
        s.unreadOnly,
        s.sortMode,
      );
      const idx = list.findIndex(
        (t) =>
          t.account_id === s.openThread!.account_id &&
          t.thread_id === s.openThread!.thread_id,
      );
      if (idx < 0) return;
      const target = list[idx + delta];
      if (target) s.openThreadView(target);
    },
    actOnOpen: (kind) => {
      const s = get();
      if (!s.openThread) return;
      // Gmail 1:1: drafts are deleted via `drafts.delete`, not `messages.modify
      // TRASH`. If the open thread carries the DRAFT label, trash becomes
      // discard. This catches every entry point (toolbar trash button, more
      // menu "Delete", future hotkeys) without needing per-callsite gating.
      if (kind === "trash" && s.openLabels.includes("DRAFT")) {
        const ref: ThreadRef = {
          account_id: s.openThread.account_id,
          thread_id: s.openThread.thread_id,
        };
        const advance = nextThreadAfter(s, ref);
        s.setThreads((prev) => prev.filter((x) => !sameAsRef(ref)(x)));
        s.setResults((prev) => prev.filter((x) => !sameAsRef(ref)(x)));
        deleteDrafts([ref])
          .then(() => {
            refreshCounts(s);
            folderCount("DRAFT", s.activeAccount)
              .then(s.setDraftCount)
              .catch(() => {});
          })
          .catch(() => {});
        s.showUndo("Draft discarded", [ref], "untrash");
        advance();
        return;
      }
      const ref: ThreadRef = {
        account_id: s.openThread.account_id,
        thread_id: s.openThread.thread_id,
      };
      actOnOpenMap[kind](get, ref);
    },
  };
}

// actOnOpen was an 86-line if/else chain over five "act on the open thread"
// actions (archive/trash/spam/unread/read). Split into one helper per kind so
// each branch's intent is local — the dispatch map at the bottom is the only
// place that decides which kind routes where. Behavior identical to the
// pre-split chain; the public `actOnOpen(kind)` signature is unchanged so the
// five existing call sites (ReaderToolbar, ReaderMoreMenu, useKeyboardShortcuts)
// need no edits.

type ActOnOpenKind = "archive" | "trash" | "spam" | "unread" | "read";
type ActOnOpenFn = (get: StoreGet, ref: ThreadRef) => void;

const sameAsRef = (ref: ThreadRef) => (x: Thread) =>
  x.account_id === ref.account_id && x.thread_id === ref.thread_id;

const refreshCounts = (s: ReturnType<StoreGet>) => {
  unreadCounts()
    .then(s.setCounts)
    .catch(() => {});
};

const actArchive: ActOnOpenFn = (get, ref) => {
  const s = get();
  const advance = nextThreadAfter(s, ref);
  s.setThreads((prev) => prev.filter((x) => !sameAsRef(ref)(x)));
  s.setResults((prev) => prev.filter((x) => !sameAsRef(ref)(x)));
  archiveThread(ref.account_id, ref.thread_id)
    .then(() => {
      refreshCounts(s);
      folderCount("DRAFT", s.activeAccount)
        .then(s.setDraftCount)
        .catch(() => {});
    })
    .catch(() => {});
  s.showUndo("Archived", [ref], "unarchive");
  advance();
};

const actTrash: ActOnOpenFn = (get, ref) => {
  const s = get();
  const advance = nextThreadAfter(s, ref);
  s.setThreads((prev) => prev.filter((x) => !sameAsRef(ref)(x)));
  s.setResults((prev) => prev.filter((x) => !sameAsRef(ref)(x)));
  trashThread(ref.account_id, ref.thread_id)
    .then(() => {
      refreshCounts(s);
      folderCount("DRAFT", s.activeAccount)
        .then(s.setDraftCount)
        .catch(() => {});
    })
    .catch(() => {});
  s.showUndo("Moved to Trash", [ref], "untrash");
  advance();
};

const actSpam: ActOnOpenFn = (get, ref) => {
  const s = get();
  const advance = nextThreadAfter(s, ref);
  s.setThreads((prev) => prev.filter((x) => !sameAsRef(ref)(x)));
  s.setResults((prev) => prev.filter((x) => !sameAsRef(ref)(x)));
  bulkAction([ref], "spam")
    .then(() => {
      refreshCounts(s);
      folderCount("DRAFT", s.activeAccount)
        .then(s.setDraftCount)
        .catch(() => {});
    })
    .catch(() => {});
  s.showUndo("Reported spam", [ref], "unspam");
  advance();
};

const actRead: ActOnOpenFn = (get, ref) => {
  const s = get();
  const same = sameAsRef(ref);
  s.setThreads((prev) => prev.map((x) => (same(x) ? { ...x, unread: false } : x)));
  s.setResults((prev) => prev.map((x) => (same(x) ? { ...x, unread: false } : x)));
  markThreadRead(ref.account_id, ref.thread_id)
    .then(() => refreshCounts(s))
    .catch(() => {});
};

const actUnread: ActOnOpenFn = (get, ref) => {
  const s = get();
  const same = sameAsRef(ref);
  s.setThreads((prev) => prev.map((x) => (same(x) ? { ...x, unread: true } : x)));
  s.setResults((prev) => prev.map((x) => (same(x) ? { ...x, unread: true } : x)));
  markThreadUnread(ref.account_id, ref.thread_id)
    .then(() => refreshCounts(s))
    .catch(() => {});
  s.setOpenThread(null);
};

const actOnOpenMap: Record<ActOnOpenKind, ActOnOpenFn> = {
  archive: actArchive,
  trash: actTrash,
  spam: actSpam,
  read: actRead,
  unread: actUnread,
};

/** Look up the thread that should become active once `ref` is removed —
 *  MUST be called before the removing `setThreads`/`setResults` filter runs,
 *  since the store's `get()` reflects those synchronously and would otherwise
 *  see the already-filtered list (losing the "jump to the next thread" UX).
 *  Returns a thunk that applies the decision; callers invoke it after the
 *  filter + showUndo, matching the original inline ordering. Shared by
 *  archive/trash/spam — the three "act + advance" actions. */
function nextThreadAfter(s: ReturnType<StoreGet>, ref: ThreadRef): () => void {
  const list = computeListSource(
    s.threads,
    s.results,
    s.query,
    s.unreadOnly,
    s.sortMode,
  );
  const idx = list.findIndex(sameAsRef(ref));
  const nextThread = list[idx + 1] ?? null;
  return () => {
    if (nextThread) s.openThreadView(nextThread);
    else s.setOpenThread(null);
  };
}
