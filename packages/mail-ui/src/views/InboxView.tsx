import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type RefObject,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { AttachmentPreviewModal } from "../components/AttachmentPreviewModal";
import { SkeletonRows } from "@rightkit/react-shell/react";
import { useAppStore } from "../store";
import { useInboxAttachmentPreview } from "../hooks/useInboxAttachmentPreview";
import { useSearchCoverage } from "../hooks/useSearchCoverage";
import {
  FOLDER_LABEL,
  CATEGORY_LABEL_IDS,
  keyOf,
  computeListSource,
  hasInboxCategories,
} from "../lib/mail";
import { type Thread } from "../lib/ipc";
import { InboxToolbar } from "./InboxToolbar";
import { InboxCategoryTabs } from "./InboxCategoryTabs";
import { InboxEmptyState } from "./InboxEmptyState";
import { InboxFolderBanner } from "./InboxFolderBanner";
import { InboxSearchChips } from "./InboxSearchChips";
import { InboxVirtualList } from "./InboxVirtualList";
import { SearchCoverageBanner } from "./SearchCoverageBanner";
export function InboxView({
  scrollRef,
}: {
  scrollRef: RefObject<HTMLDivElement | null>;
}) {
  const threads = useAppStore((s) => s.threads);
  const results = useAppStore((s) => s.results);
  const query = useAppStore((s) => s.query);
  const unreadOnly = useAppStore((s) => s.unreadOnly);
  const setUnreadOnly = useAppStore((s) => s.setUnreadOnly);
  const sortMode = useAppStore((s) => s.sortMode);
  const setSortMode = useAppStore((s) => s.setSortMode);
  const multiInbox = useAppStore((s) => s.multiInbox);
  const selected = useAppStore((s) => s.selected);
  const setSelected = useAppStore((s) => s.setSelected);
  const focusIdx = useAppStore((s) => s.focusIdx);
  const folder = useAppStore((s) => s.folder);
  const destructiveOn = useAppStore((s) => s.destructiveOn);
  const accounts = useAppStore((s) => s.accounts);
  const activeAccount = useAppStore((s) => s.activeAccount);
  const busy = useAppStore((s) => s.busy);
  const setErr = useAppStore((s) => s.setErr);
  const view = useAppStore((s) => s.view);
  const openThread = useAppStore((s) => s.openThread);
  const readingPane = useAppStore((s) => s.readingPane);
  const category = useAppStore((s) => s.category);
  const setCategory = useAppStore((s) => s.setCategory);
  const setOpenThread = useAppStore((s) => s.setOpenThread);
  const setQuery = useAppStore((s) => s.setQuery);
  const setView = useAppStore((s) => s.setView);
  const clientIdSet = useAppStore((s) => s.clientIdSet);
  const clientSecretSet = useAppStore((s) => s.clientSecretSet);
  const searching = useAppStore((s) => s.searching);
  const noMoreOlder = useAppStore((s) => s.noMoreOlder);
  const loadingOlder = useAppStore((s) => s.loadingOlder);
  const threadsLoading = useAppStore((s) => s.threadsLoading);
  const loadOlderMessages = useAppStore((s) => s.loadOlderMessages);
  const labels = useAppStore((s) => s.labels);
  const activeLabel = useAppStore((s) => s.activeLabel);
  const run = useAppStore((s) => s.run);
  const saveAndScan = useAppStore((s) => s.saveAndScan);
  const toggleSelectAll = useAppStore((s) => s.toggleSelectAll);
  const runBulk = useAppStore((s) => s.runBulk);
  const emptyFolderFlow = useAppStore((s) => s.emptyFolderFlow);
  const deleteForeverFlow = useAppStore((s) => s.deleteForeverFlow);
  const labelNameById = useMemo(
    () => new Map(labels.map((l) => [l.id, l.name])),
    [labels],
  );
  const currentScopeLabel = activeLabel ?? FOLDER_LABEL[folder];
  const labelNameOf = useCallback(
    (id: string) => {
      if (
        id === currentScopeLabel ||
        id === "INBOX" ||
        id === "UNREAD" ||
        id === "IMPORTANT" ||
        id === "CHAT" ||
        id.startsWith("CATEGORY_")
      ) {
        return "";
      }
      return labelNameById.get(id) ?? id;
    },
    [currentScopeLabel, labelNameById],
  );
  const {
    attachmentPreview,
    setAttachmentPreview,
    openingAttachmentKey,
    openAttachmentPreview,
    downloadPreviewAttachment,
  } = useInboxAttachmentPreview({ saveAndScan, setErr });
  const { coverage, dismissCoverageBanner } = useSearchCoverage({
    activeAccount,
    searchActive: !!query.trim(),
  });
  const credsSet = clientIdSet && clientSecretSet;
  const isInboxView = !activeLabel && (folder === "all" || folder === "inbox");
  const hasCategories = useMemo(() => hasInboxCategories(labels), [labels]);
  const showTabs = isInboxView && view === "inbox" && hasCategories;
  const listVisible = !openThread || readingPane;
  const labelById = useMemo(
    () => new Map(labels.map((l) => [l.id, l])),
    [labels],
  );
  const queryTokens = useMemo(
    () => query.split(/\s+/).filter(Boolean),
    [query],
  );
  const hasOp = (op: string) => queryTokens.includes(op);
  const toggleOp = (op: string) => {
    setQuery(
      (queryTokens.includes(op)
        ? queryTokens.filter((t) => t !== op)
        : [...queryTokens, op]
      ).join(" "),
    );
    setOpenThread(null);
  };
  const listSource = useMemo(
    () => computeListSource(threads, results, query, unreadOnly, sortMode),
    [threads, results, query, unreadOnly, sortMode],
  );
  const inboxTotal = labelById.get("INBOX")?.total;
  const activeCategoryTotal =
    category === "PRIMARY" && typeof inboxTotal === "number"
      ? Math.max(
          0,
          inboxTotal -
            CATEGORY_LABEL_IDS.reduce(
              (sum, id) => sum + (labelById.get(id)?.total ?? 0),
              0,
            ),
        )
      : category
        ? labelById.get(category)?.total
        : undefined;
  const activeScopeTotal =
    activeCategoryTotal ??
    (activeLabel
      ? labelById.get(activeLabel)?.total
      : labelById.get(FOLDER_LABEL[folder])?.total) ??
    listSource.length;
  const pageEnd = Math.min(listSource.length, 50);
  const selectedRefs = useMemo(
    () =>
      listSource
        .filter((t) => selected.has(keyOf(t)))
        .map((t) => ({ account_id: t.account_id, thread_id: t.thread_id })),
    [listSource, selected],
  );
  const allSelected = useMemo(
    () =>
      listSource.length > 0 && listSource.every((t) => selected.has(keyOf(t))),
    [listSource, selected],
  );
  const someSelected = selected.size > 0;
  // Flatten grouped/flat modes into one virtualizer item stream.
  const listGrouped = multiInbox && !query.trim() && listSource.length > 0;
  const listItems = useMemo<
    Array<
      | { kind: "header"; label: string }
      | { kind: "row"; thread: Thread; rowIndex: number }
    >
  >(() => {
    if (!listGrouped) {
      return listSource.map((thread, rowIndex) => ({
        kind: "row" as const,
        thread,
        rowIndex,
      }));
    }
    const items: Array<
      | { kind: "header"; label: string }
      | { kind: "row"; thread: Thread; rowIndex: number }
    > = [];
    const section = (label: string, predicate: (t: Thread) => boolean) => {
      const rows = listSource
        .map((thread, rowIndex) => ({ thread, rowIndex }))
        .filter((r) => predicate(r.thread));
      if (rows.length === 0) return;
      items.push({ kind: "header", label });
      for (const r of rows)
        items.push({ kind: "row", thread: r.thread, rowIndex: r.rowIndex });
    };
    section("Unread", (t) => t.unread);
    section("Starred", (t) => !t.unread && t.starred);
    section("Everything else", (t) => !t.unread && !t.starred);
    return items;
  }, [listGrouped, listSource]);
  const flatIndexOfRow = useMemo(() => {
    const m = new Map<number, number>();
    listItems.forEach((it, i) => {
      if (it.kind === "row") m.set(it.rowIndex, i);
    });
    return m;
  }, [listItems]);
  const [listAnchor, setListAnchor] = useState<HTMLDivElement | null>(null);
  const [scrollMargin, setScrollMargin] = useState(0);
  // eslint-disable-next-line react-hooks/incompatible-library
  const rowVirtualizer = useVirtualizer({
    count: listItems.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (i) => {
      const it = listItems[i];
      if (!it || it.kind === "header") return 34;
      return it.thread.attachment_names.length > 0 ? 56 : 40;
    },
    overscan: 12,
    scrollMargin,
    getItemKey: (i) => {
      const it = listItems[i];
      return it.kind === "row" ? keyOf(it.thread) : `h:${it.label}:${i}`;
    },
  });
  const listScopeKey = [
    view,
    activeAccount ?? "",
    activeLabel ?? "",
    folder,
    category ?? "",
    query.trim(),
    unreadOnly ? "unread" : "all",
    sortMode,
  ].join("|");
  const prevListScopeKey = useRef<string | null>(null);
  useLayoutEffect(() => {
    if (view !== "inbox") {
      prevListScopeKey.current = listScopeKey;
      return;
    }
    const previous = prevListScopeKey.current;
    prevListScopeKey.current = listScopeKey;
    if (previous === null || previous === listScopeKey) return;
    scrollRef.current?.scrollTo({ top: 0, left: 0 });
    rowVirtualizer.scrollToOffset(0);
  }, [listScopeKey, rowVirtualizer, scrollRef, view]);
  // Measure the variable header offset inside the shared .content scroller.
  const listEmpty = listSource.length === 0;
  const searchPending =
    !!query.trim() && searching && listSource.length === 0;
  useLayoutEffect(() => {
    if (listAnchor) setScrollMargin(listAnchor.offsetTop);
  }, [
    listAnchor,
    view,
    openThread,
    readingPane,
    folder,
    query,
    multiInbox,
    unreadOnly,
    listEmpty,
  ]);
  useEffect(() => {
    if (focusIdx < 0) return;
    const fi = flatIndexOfRow.get(focusIdx);
    if (fi != null) rowVirtualizer.scrollToIndex(fi, { align: "auto" });
  }, [focusIdx, flatIndexOfRow, rowVirtualizer]);
  // Infinite scroll when the last virtual row comes into view.
  const virtualItems = rowVirtualizer.getVirtualItems();
  const lastVirtualIndex = virtualItems.length
    ? virtualItems[virtualItems.length - 1].index
    : -1;
  useEffect(() => {
    if (view !== "inbox" || (openThread && !readingPane)) return;
    const scrollTop = scrollRef.current?.scrollTop ?? 0;
    if (scrollTop < 24) return;
    if (
      lastVirtualIndex >= listItems.length - 1 &&
      lastVirtualIndex >= 0 &&
      !loadingOlder &&
      !noMoreOlder
    ) {
      loadOlderMessages();
    }
  }, [
    lastVirtualIndex,
    listItems.length,
    loadingOlder,
    noMoreOlder,
    view,
    openThread,
    readingPane,
    query,
    scrollRef,
    loadOlderMessages,
  ]);
  return (
    <>
      {listVisible && query.trim() && (
        <SearchCoverageBanner
          coverage={coverage}
          onDismiss={dismissCoverageBanner}
        />
      )}
      {listVisible && (
        <InboxSearchChips
          hasOp={hasOp}
          query={query}
          searching={searching}
          toggleOp={toggleOp}
        />
      )}
      {listVisible && (
        <div className="thread-list flex flex-col">
          <InboxFolderBanner
            destructiveOn={destructiveOn}
            emptyFolderFlow={emptyFolderFlow}
            folder={folder}
            listLength={listSource.length}
          />
          <InboxToolbar
            activeScopeTotal={activeScopeTotal}
            allSelected={allSelected}
            busy={busy}
            deleteForeverFlow={deleteForeverFlow}
            destructiveOn={destructiveOn}
            folder={folder}
            listLength={listSource.length}
            loadOlderMessages={loadOlderMessages}
            loadingOlder={loadingOlder}
            noMoreOlder={noMoreOlder}
            pageEnd={pageEnd}
            run={run}
            runBulk={runBulk}
            selectedRefs={selectedRefs}
            selectedSize={selected.size}
            setSelected={setSelected}
            setSortMode={setSortMode}
            setUnreadOnly={setUnreadOnly}
            someSelected={someSelected}
            sortMode={sortMode}
            toggleSelectAll={toggleSelectAll}
            unreadOnly={unreadOnly}
          />
          {showTabs && !query.trim() && !unreadOnly && (
            <InboxCategoryTabs
              category={category}
              labelById={labelById}
              setCategory={setCategory}
              setOpenThread={setOpenThread}
            />
          )}
          {searchPending ? (
            <SkeletonRows rows={9} />
          ) : listSource.length === 0 && threadsLoading && !query.trim() ? (
            <SkeletonRows rows={9} />
          ) : listSource.length === 0 ? (
            <InboxEmptyState
              accountsLength={accounts.length}
              busy={busy}
              credsSet={credsSet}
              folder={folder}
              query={query}
              run={run}
              setUnreadOnly={setUnreadOnly}
              setView={setView}
              threadsLength={threads.length}
              unreadOnly={unreadOnly}
            />
          ) : (
            <InboxVirtualList
              focusIdx={focusIdx}
              getTotalSize={rowVirtualizer.getTotalSize}
              labelNameOf={labelNameOf}
              listItems={listItems}
              measureElement={rowVirtualizer.measureElement}
              onOpenAttachment={openAttachmentPreview}
              openingAttachmentKey={openingAttachmentKey}
              query={query}
              scrollMargin={scrollMargin}
              selected={selected}
              setListAnchor={setListAnchor}
              virtualItems={virtualItems}
            />
          )}
          {!query.trim() && listSource.length > 0 && loadingOlder && (
            <div className="list-foot flex justify-center px-2 pb-7 pt-4">
              <span className="list-foot-msg text-[13px] text-mr-muted">
                Loading older...
              </span>
            </div>
          )}
        </div>
      )}
      {attachmentPreview && (
        <AttachmentPreviewModal
          preview={attachmentPreview}
          onClose={() => setAttachmentPreview(null)}
          onDownload={downloadPreviewAttachment}
        />
      )}
    </>
  );
}
