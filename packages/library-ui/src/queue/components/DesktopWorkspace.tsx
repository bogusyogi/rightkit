import { useEffect, useMemo, useRef, useState, type CSSProperties, type KeyboardEvent } from "react";
import { formatInvokeError } from "../lib/errorMessage.js";
import type { DisplayJob, HistoryRow, JobEvent, JobInfo, MediaSpec } from "../contract.js";
import { getShell, invoke, listen } from "../lib/ipc/index.js";
import { applyJobEvent, isTerminalStage } from "../lib/jobState.js";
import {
  buildDesktopSourceGroups,
  type DesktopSource,
  type DesktopSourceFilter,
} from "../lib/desktopSources.js";
import {
  HISTORY_PAGE_SIZE,
  mergeHistoryRows,
  nearBottom,
  type HistoryPage,
} from "../lib/historyPage.js";
import { isActiveStage, labelForSource, categoryForKind, statusText } from "../lib/jobDisplay.js";
import { parseTimestamp } from "../lib/format.js";
import { IMAGE_KIND_LABEL } from "../lib/mediaSelection.js";
import { selectWorkbenchJobId } from "../lib/workbench.js";
import { isMac } from "../lib/platform.js";
import { applyTheme, loadThemeChoice, saveThemeChoice, themeStore } from "../lib/theme.js";
import {
  ChevronDown,
  Columns2,
  LoaderCircle,
  LogIn,
  Minus,
  PanelLeftOpen,
  Search,
  Settings as SettingsIcon,
  Square,
  Contrast,
  Video,
  X,
} from "lucide-react";
import { AppShell, CaptionControls } from "@rightkit/react-shell/react";
import type { NavGroup } from "@rightkit/react-shell";
import { moveIndex, type ListMove } from "@rightkit/platform-ui/headless";
import { PasteBar } from "./PasteBar.js";
import { CAPTION_LABELS } from "./TitleBar.js";
import { JobStatePreview } from "./JobStatePreview.js";
import { Detail } from "../views/Detail.js";
import { SourcePlatformIcon, sourcePlatform } from "./SourcePlatformIcon.js";

// The workspace has no navigation list of its own (the title bar tabs are app chrome), so the shell
// frame gets an empty destination set.
const NO_NAV: readonly NavGroup[] = [];
const NO_NAVIGATE = () => {};
const SIDEBAR_DEFAULT = 288;
const SIDEBAR_MIN = 240;
const SIDEBAR_MAX = 380;

const LIST_KEY_MOVES: Record<string, ListMove | undefined> = {
  ArrowDown: "next",
  ArrowUp: "prev",
  Home: "first",
  End: "last",
};

type Props = {
  jobs: DisplayJob[];
  submitBlocked?: boolean;
  onAdd: (inputs: string[], media?: MediaSpec) => Promise<boolean> | boolean;
  onPickFiles: () => void;
  onOpen: (id: string) => void;
  onRemove: (id: string) => void;
  onCancel: (id: string) => void;
  onRerun: (job: JobInfo) => void;
  onClearCompleted: () => void;
  onClearAll: () => void;
  onRetryAllFailed: () => void;
  onRetryAfterSignin: (job: JobInfo, options?: { skipExport?: boolean }) => void;
  onError: (message: string) => void;
  requestedId?: string | null;
  onSelectionChange?: (id: string | null) => void;
  onRerunSource?: (input: string) => void;
  onOpenLibrary?: () => void;
  onOpenSettings?: (section?: string) => void;
};
type SourceFilter = DesktopSourceFilter | "video" | "social" | "document" | "web" | "audio";
const FILTERS: { id: SourceFilter; label: string }[] = [
  { id: "all", label: "All Items" },
  { id: "active", label: "In Progress" },
  { id: "attention", label: "Needs Attention" },
  { id: "video", label: "Videos" },
  { id: "social", label: "Social Posts" },
  { id: "document", label: "Documents" },
  { id: "web", label: "Web Pages" },
  { id: "audio", label: "Audio" },
  { id: "saved", label: "Saved Items" },
];
const inputOf = (source: DesktopSource) =>
  source.type === "queue" ? source.job.input : source.row.url;
const kindOf = (source: DesktopSource) =>
  source.type === "queue" ? source.job.kind : source.row.kind;
const activeOf = (source: DesktopSource) =>
  source.type === "queue" && isActiveStage(source.job.stage, { includeQueued: true });

export function DesktopWorkspace(props: Props) {
  const { jobs, requestedId, onSelectionChange, onError } = props;
  const [collapsed, setCollapsed] = useState(false);
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<SourceFilter>("all");
  const [selectedId, setSelectedId] = useState<string | null>(requestedId ?? null);
  const [history, setHistory] = useState<HistoryRow[]>([]);
  const [historyLoading, setHistoryLoading] = useState(true);
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [historyCursor, setHistoryCursor] = useState<string | null>(null);
  const [historyLoadingMore, setHistoryLoadingMore] = useState(false);
  const [toolbarTarget, setToolbarTarget] = useState<HTMLDivElement | null>(null);
  const filterMenu = useRef<HTMLDetailsElement>(null);
  // WS-6: attach attempts (transcribe a saved download) are queue jobs that
  // target an existing library row — they never go through add_jobs, so the
  // app-level job reconciliation never inserts them. Track them here: the
  // snapshot supplies attempts already in flight, live events drive them,
  // and the Detail's submit callback registers fresh ones immediately.
  const [attachJobs, setAttachJobs] = useState<Map<string, DisplayJob>>(() => new Map());
  // The snapshot effect runs once; read the latest error sink through a ref.
  const onErrorRef = useRef(onError);
  onErrorRef.current = onError;
  useEffect(() => {
    let live = true;
    invoke<JobInfo[]>("get_jobs")
      .then((list) => {
        if (!live) return;
        setAttachJobs((cur) => {
          const next = new Map(cur);
          for (const job of list) {
            if (job.attach_to) next.set(job.id, job as DisplayJob);
          }
          return next;
        });
      })
      .catch((e) => onErrorRef.current?.(formatInvokeError(e)));
    return () => {
      live = false;
    };
  }, []);
  useEffect(() => {
    const unlisten = listen<JobEvent>("job://update", (event) => {
      setAttachJobs((cur) => {
        const job = cur.get(event.payload.id);
        if (!job) return cur;
        const next = new Map(cur);
        next.set(event.payload.id, applyJobEvent(job, event.payload));
        return next;
      });
    });
    return () => {
      void unlisten.then((off) => off());
    };
  }, []);
  const trackAttempt = (job: JobInfo) => {
    setAttachJobs((cur) => new Map(cur).set(job.id, job as DisplayJob));
  };
  // Attempts the app-level jobs array already owns win; tracked attempts
  // fill the gap between submit and the next snapshot.
  const mergedJobs = useMemo(() => {
    const known = new Set(jobs.map((job) => job.id));
    const extra = [...attachJobs.values()].filter((job) => !known.has(job.id));
    return extra.length ? [...extra, ...jobs] : jobs;
  }, [jobs, attachJobs]);
  const attemptByTarget = useMemo(() => {
    const map = new Map<string, DisplayJob>();
    for (const job of mergedJobs) {
      if (job.attach_to && !isTerminalStage(job.stage)) map.set(job.attach_to, job);
    }
    return map;
  }, [mergedJobs]);
  const historyIds = useMemo(() => new Set(history.map((row) => row.id)), [history]);
  const completedKey = jobs
    .filter((job) => job.stage === "done")
    .map((job) => job.id)
    .sort()
    .join("|");
  // An attach attempt reaching done means its target row's transcript
  // surfaces changed — refetch history so the item shows them.
  const attachDoneKey = [...attachJobs.values()]
    .filter((job) => job.stage === "done")
    .map((job) => job.id)
    .sort()
    .join("|");
  useEffect(() => {
    if (requestedId) setSelectedId(requestedId);
  }, [requestedId]);
  // biome-ignore lint/correctness/useExhaustiveDependencies: membership changes refresh saved source metadata
  useEffect(() => {
    let current = true;
    setHistoryLoading(true);
    invoke<HistoryPage>("get_history_page", { query: "", cursor: null, limit: HISTORY_PAGE_SIZE })
      .then((page) => {
        if (current) {
          setHistory(page.rows);
          setHistoryCursor(page.next_cursor);
          setHistoryError(null);
        }
      })
      .catch((cause) => {
        if (current) setHistoryError(String(cause));
      })
      .finally(() => {
        if (current) setHistoryLoading(false);
      });
    return () => {
      current = false;
    };
  }, [completedKey, attachDoneKey]);
  const loadMoreHistory = () => {
    if (!historyCursor || historyLoadingMore) return;
    setHistoryLoadingMore(true);
    invoke<HistoryPage>("get_history_page", {
      query: "",
      cursor: historyCursor,
      limit: HISTORY_PAGE_SIZE,
    })
      .then((page) => {
        setHistory((rows) => mergeHistoryRows(rows, page.rows));
        setHistoryCursor(page.next_cursor);
      })
      .catch((cause) => setHistoryError(String(cause)))
      .finally(() => setHistoryLoadingMore(false));
  };
  // The sidebar search and filters run client-side over loaded rows, so while
  // one is active keep pulling pages until the whole library is searchable.
  // biome-ignore lint/correctness/useExhaustiveDependencies: loadMoreHistory closes over the same cursor state
  useEffect(() => {
    if ((query.trim() || filter !== "all") && historyCursor && !historyLoadingMore) {
      loadMoreHistory();
    }
  }, [query, filter, historyCursor, historyLoadingMore]);
  const allSources = useMemo(() => {
    const groups = buildDesktopSourceGroups(mergedJobs, history);
    return [...groups.active, ...groups.recent, ...groups.saved];
  }, [mergedJobs, history]);
  const sources = useMemo(() => {
    const statusFilter = ["all", "active", "attention", "saved"].includes(filter)
      ? (filter as DesktopSourceFilter)
      : "all";
    const groups = buildDesktopSourceGroups(mergedJobs, history, query, statusFilter);
    return [...groups.active, ...groups.recent, ...groups.saved].filter((source) => {
      if (["all", "active", "attention", "saved"].includes(filter)) return true;
      if (filter === "audio")
        return (
          /\.(mp3|m4a|wav|ogg|flac)(?:$|\?)/i.test(inputOf(source)) ||
          kindOf(source) === "media_url"
        );
      const kind = kindOf(source);
      if (filter === "social")
        return ["reel", "carousel", "instagram_post", "threads", "facebook"].includes(kind);
      const category = categoryForKind(kind);
      return filter === "video"
        ? ["youtube", "tiktok", "video_url"].includes(kind) ||
            /\.(mp4|mov|mkv|webm|avi)$/i.test(inputOf(source))
        : filter === "document"
          ? category === "document"
          : category === filter;
    });
  }, [filter, history, mergedJobs, query]);
  const selectedSource = allSources.find((source) => source.id === selectedId) ?? null;
  const selectedJob = selectedSource?.type === "queue" ? selectedSource.job : null;
  const selectedMetadata = history.find((row) => row.id === selectedId);
  const showDetail =
    selectedSource && (selectedSource.type === "saved" || selectedJob?.stage === "done");
  useEffect(() => {
    if (
      (selectedId && allSources.some((source) => source.id === selectedId)) ||
      (requestedId === selectedId && historyLoading)
    )
      return;
    const preferred = selectWorkbenchJobId(mergedJobs, null);
    setSelectedId(
      allSources.some((source) => source.id === preferred)
        ? preferred
        : (allSources[0]?.id ?? null),
    );
  }, [allSources, historyLoading, mergedJobs, requestedId, selectedId]);
  const select = (id: string | null) => {
    setSelectedId(id);
    onSelectionChange?.(id);
  };
  const sections = useMemo(() => sourceSections(sources, history), [sources, history]);
  const chooseTheme = (id: string) => {
    applyTheme(id);
    saveThemeChoice(id);
  };
  /** Arrow, Home and End on a source list: move the selection, clamped at both ends. */
  const moveSelection = (event: KeyboardEvent<HTMLElement>) => {
    const move = LIST_KEY_MOVES[event.key];
    if (!move) return;
    event.preventDefault();
    const index = sources.findIndex((source) => source.id === selectedId);
    const next = sources[moveIndex(index, sources.length, move, false)];
    if (next) select(next.id);
  };
  const selectedTitle = selectedSource?.title ?? "No item selected";
  // Frame, titlebar slot, collapsible sidebar and its drag/keyboard resize are the owner's `AppShell`
  // (react-shell). The workspace keeps its own titlebar content, source list and reader.
  const titlebarNode = (
  <header
    className="titlebar"
    data-tauri-drag-region
    style={{ "--entry-left": collapsed ? "48px" : "var(--rk-sidebar-width)" } as CSSProperties}
  >
    <div className="side-head">
      <span className="p-sp" />
      <button
        type="button"
        className="hb"
        aria-label={collapsed ? "Expand sources sidebar" : "Collapse sources sidebar"}
        aria-expanded={!collapsed}
        aria-controls="desktop-source-panel"
        onClick={() => setCollapsed((value) => !value)}
      >
        {collapsed ? <PanelLeftOpen size={16} /> : <Columns2 size={16} />}
      </button>
    </div>
    <PasteBar
      onAdd={props.onAdd}
      onPickFiles={props.onPickFiles}
      submitBlocked={props.submitBlocked}
    />
    <div className="toolbar-space" data-tauri-drag-region />
    {!isMac && (
      <CaptionControls
        adapter={getShell().bridge().window}
        labels={CAPTION_LABELS}
        className="caps"
        glyphs={{
          minimize: <Minus size={12} />,
          maximize: <Square size={10} />,
          close: <X size={13} />,
        }}
        buttonClasses={{ close: "close" }}
        titles={false}
      />
    )}
  </header>
  );
  const sidebarNode = (
  <aside
    className={`sidebar${collapsed ? " collapsed" : ""}`}
    style={{ "--sbw": "var(--rk-sidebar-width)" } as CSSProperties}
    data-collapsed={collapsed ? "true" : "false"}
    aria-label="Sources"
  >
    {collapsed ? (
      <div className="rail">
        <div
          className="rail-list"
          role="listbox"
          aria-label="Sources"
          tabIndex={0}
          onKeyDown={moveSelection}
        >
          {sources.map((source) => (
            <button
              key={source.id}
              type="button"
              className={`rtab${source.id === selectedId ? " sel" : ""}`}
              role="option"
              aria-selected={source.id === selectedId}
              aria-label={`Select ${source.title}`}
              title={source.title}
              onClick={() => select(source.id)}
            >
              {source.id === selectedId && <span className="mark" />}
              <SourceBadge source={source} />
            </button>
          ))}
        </div>
        <button
          type="button"
          className="rb"
          aria-label="Settings"
          onClick={() => props.onOpenSettings?.()}
        >
          <SettingsIcon size={16} />
        </button>
      </div>
    ) : (
      <div className="panel" id="desktop-source-panel">
        <div className="p-head">
          <details className="desktop-pop" ref={filterMenu}>
            <summary
              className="fbtn"
              aria-label={`Show: ${FILTERS.find((item) => item.id === filter)?.label}`}
            >
              <span>{FILTERS.find((item) => item.id === filter)?.label}</span>
              <span className="n">{sources.length || ""}</span>
              <ChevronDown className="ic" size={13} />
            </summary>
            <fieldset className="desktop-popup filter-popup" aria-label="Filter sources">
              {FILTERS.map((item) => (
                <button
                  type="button"
                  key={item.id}
                  aria-pressed={filter === item.id}
                  onClick={() => {
                    setFilter(item.id);
                    if (filterMenu.current) filterMenu.current.open = false;
                  }}
                >
                  {item.label}
                  <span>{filter === item.id ? "✓" : ""}</span>
                </button>
              ))}
              <hr />
              <button type="button" onClick={props.onOpenLibrary}>
                Manage saved files
              </button>
              <button type="button" onClick={props.onRetryAllFailed}>
                Retry failed
              </button>
              <button type="button" onClick={props.onClearCompleted}>
                Clear ready from queue
              </button>
              <button type="button" onClick={props.onClearAll}>
                Clear queue
              </button>
            </fieldset>
          </details>
        </div>
        <label className="search">
          <Search size={13} />
          <input
            type="search"
            aria-label="Search sources"
            placeholder="Search"
            value={query}
            onChange={(event) => setQuery(event.currentTarget.value)}
          />
          {query && (
            <button
              type="button"
              className="clear"
              aria-label="Clear search"
              onClick={() => setQuery("")}
            >
              <X size={11} />
            </button>
          )}
        </label>
        <div
          className="p-list"
          role="listbox"
          aria-label="Source list"
          tabIndex={0}
          onScroll={(event) => {
            if (nearBottom(event.currentTarget)) loadMoreHistory();
          }}
          onKeyDown={moveSelection}
        >
          {historyLoading && (
            <p className="list-status" role="status">
              Loading saved sources…
            </p>
          )}
          {historyError && (
            <p className="list-status" role="alert">
              Saved sources failed to load: {historyError}
            </p>
          )}
          {sections.map(([name, items]) => (
            <section key={name} aria-label={name}>
              <h2 className="l-head">{name}</h2>
              {items.map((source) => (
                <div
                  key={source.id}
                  className={`row${source.id === selectedId ? " sel" : ""}`}
                  data-job-card={source.type === "queue" ? "" : undefined}
                >
                  {source.id === selectedId && <span className="mark" />}
                  <span className="lead">
                    {source.type === "queue" && activeOf(source) ? (
                      <button
                        type="button"
                        className="badge-cancel"
                        aria-label={`Cancel queued item ${source.title}`}
                        disabled={source.job.cancelling}
                        onClick={() => props.onCancel(source.id)}
                      >
                        <SourceBadge source={source} />
                        <span className="stop" />
                      </button>
                    ) : (
                      <SourceBadge source={source} />
                    )}
                  </span>
                  <button
                    type="button"
                    className="row-main source-select"
                    role="option"
                    aria-selected={source.id === selectedId}
                    aria-label={`Select ${source.title}`}
                    onClick={() => select(source.id)}
                  >
                    <span className="row-title" title={source.title}>
                      {source.title}
                    </span>
                    <span className="row-line">
                      <span
                        className="row-sub"
                        title={sourceSubtitle(source, history)}
                      >
                        {source.type === "queue" && source.job.stage !== "done"
                          ? statusText(source.job)
                          : sourceSubtitle(source, history)}
                      </span>
                      <span className="row-meta">{rowDate(source, history)}</span>
                    </span>
                  </button>
                  {source.type === "queue" && source.job.needs_auth && (
                    <button
                      type="button"
                      className="pill"
                      onClick={() => {
                        select(source.id);
                        void invoke("sign_in", {
                          platform: source.job.needs_auth,
                          url: source.job.input,
                        }).catch((error) => onError(String(error)));
                      }}
                    >
                      Sign In
                    </button>
                  )}
                </div>
              ))}
            </section>
          ))}
          {!sources.length && !historyLoading && (
            <div className="l-empty">
              <b>{query ? "No results" : "Nothing here yet"}</b>
              <span>
                {query
                  ? "No sources match this search."
                  : "Paste a link or drop files to start."}
              </span>
            </div>
          )}
        </div>
        <footer className="p-foot">
          <span className="wm">
            Scrape<span>Right</span>
          </span>
          <button
            type="button"
            className="hb"
            aria-label="Settings"
            onClick={() => props.onOpenSettings?.()}
          >
            <SettingsIcon size={15} />
          </button>
          <details className="desktop-pop appearance">
            <summary className="hb" aria-label="Appearance">
              <Contrast size={15} />
            </summary>
            <div className="desktop-popup">
              <span className="menu-label">Appearance</span>
              {[
                { id: "auto", label: "System" },
                { id: "scraperight-light", label: "Light" },
                { id: "scraperight-dark", label: "Dark" },
              ].map((item) => (
                <button
                  type="button"
                  key={item.id}
                  onClick={(event) => {
                    chooseTheme(item.id);
                    const menu = event.currentTarget.closest("details");
                    if (menu) menu.open = false;
                  }}
                >
                  {item.label}
                  {loadThemeChoice() === item.id && <span>✓</span>}
                </button>
              ))}
            </div>
          </details>
        </footer>
      </div>
    )}
  </aside>
  );
  return (
    <AppShell
      groups={NO_NAV}
      activeId=""
      onNavigate={NO_NAVIGATE}
      titlebar={titlebarNode}
      sidebar={sidebarNode}
      sidebarCollapsed={collapsed}
      onSidebarCollapsedChange={setCollapsed}
      sidebarToggle={false}
      searchMode="none"
      sidebarWidth={SIDEBAR_DEFAULT}
      sidebarResizable={{ min: SIDEBAR_MIN, max: SIDEBAR_MAX }}
      shell={{ themeStore, mountOverlays: false }}
      className="claude-workspace"
      slots={{ body: "wbody" }}
    >
        <section
          className={`plane${!allSources.length ? " is-empty" : ""}`}
          aria-label="Source reader"
        >
          <div className="plane-bar" data-tauri-drag-region>
            <div className="tb-group reader-toolbar" ref={setToolbarTarget} />
          </div>
          <div className="reader">
            {showDetail && selectedSource && (
              <Detail
                key={selectedSource.id}
                desktop
                toolbarTarget={toolbarTarget}
                metadata={selectedMetadata}
                onRemoveQueue={
                  selectedSource.type === "queue"
                    ? () => props.onRemove(selectedSource.id)
                    : undefined
                }
                jobId={selectedSource.id}
                onRerun={props.onRerunSource}
                onError={onError}
                onOpenPro={() => props.onOpenSettings?.("pro")}
                // WS-6: a saved media row can ask for its transcript — the
                // attempt is a queue job tracked locally so progress renders
                // in place.
                attachAttempt={
                  selectedSource.type === "saved"
                    ? attemptByTarget.get(selectedSource.id)
                    : undefined
                }
                onTranscribe={
                  selectedSource.type === "saved" && historyIds.has(selectedSource.id)
                    ? () => {
                        void invoke<JobInfo>("transcribe_saved_media", {
                          id: selectedSource.id,
                        })
                          .then((job) => trackAttempt(job))
                          .catch((cause) => onError?.(String(cause)));
                      }
                    : undefined
                }
              />
            )}
            {selectedJob && !showDetail && (
              <div className="state">
                <div className={selectedSource && activeOf(selectedSource) ? "big" : "halo fail"}>
                  {selectedSource && activeOf(selectedSource) ? (
                    <ProgressRing pct={selectedJob.pct} size={64} />
                  ) : selectedJob.needs_auth ? (
                    <LogIn size={25} />
                  ) : (
                    <X size={25} />
                  )}
                </div>
                <h2>{selectedTitle}</h2>
                <p className="live" role="status" aria-live="polite">
                  {statusText(selectedJob)}
                </p>
                {selectedJob.needs_auth && (
                  <p>
                    Sign in to {selectedJob.needs_auth}, then close browser window. This item will
                    continue automatically.
                  </p>
                )}
                <div className="actions">
                  {Boolean(selectedSource && activeOf(selectedSource)) && (
                    <button
                      type="button"
                      className="btn"
                      aria-label={`Cancel ${selectedTitle}`}
                      disabled={selectedJob.cancelling}
                      onClick={() => props.onCancel(selectedJob.id)}
                    >
                      {selectedJob.cancelling ? "Cancelling…" : "Cancel"}
                    </button>
                  )}
                  {selectedJob.needs_auth && (
                    <button
                      type="button"
                      className="btn primary"
                      onClick={() => {
                        void invoke("sign_in", {
                          platform: selectedJob.needs_auth,
                          url: selectedJob.input,
                        }).catch((e) => onError(String(e)));
                      }}
                    >
                      Sign In
                    </button>
                  )}
                  {(selectedJob.stage === "error" || selectedJob.stage === "cancelled") && (
                    <button
                      type="button"
                      className="btn"
                      aria-label={`Retry ${selectedTitle}`}
                      onClick={() =>
                        selectedJob.needs_auth
                          ? props.onRetryAfterSignin(selectedJob)
                          : props.onRerun(selectedJob)
                      }
                    >
                      {selectedJob.needs_auth ? "I’ve signed in" : "Try Again"}
                    </button>
                  )}
                  <button
                    type="button"
                    className="btn"
                    aria-label={`Remove ${selectedTitle} from queue`}
                    onClick={() => props.onRemove(selectedJob.id)}
                  >
                    Remove from queue
                  </button>
                </div>
                <details className="state-details">
                  <summary>Details</summary>
                  <JobStatePreview
                    job={selectedJob}
                    active={Boolean(selectedSource && activeOf(selectedSource))}
                    progress={selectedJob.pct}
                    onRetryAfterSignin={props.onRetryAfterSignin}
                    onError={onError}
                  />
                </details>
              </div>
            )}
            {!selectedSource && (
              <div className={!historyLoading && !allSources.length ? "welcome" : "state"}>
                {historyLoading ? (
                  <div className="halo plain">
                    <LoaderCircle size={26} />
                  </div>
                ) : (
                  <img className="icon" src="/favicon.svg" alt="" />
                )}
                <h2 className={!historyLoading && !allSources.length ? "wordmark" : undefined}>
                  {historyLoading ? (
                    "Loading your sources…"
                  ) : allSources.length ? (
                    "No item selected"
                  ) : (
                    <>
                      Scrape<span>Right</span>
                    </>
                  )}
                </h2>
                {!historyLoading && !allSources.length && (
                  <>
                    <p className="lede">
                      Get the text out of videos, reels, PDFs, screenshots, and web pages. Files are
                      processed on this {isMac ? "Mac" : "PC"} and never uploaded.
                    </p>
                    <div className="actions">
                      <button
                        type="button"
                        className="btn primary lg"
                        onClick={props.onPickFiles}
                        disabled={props.submitBlocked}
                      >
                        Choose Files…
                      </button>
                    </div>
                    <p className="hint">or paste a link, or drop files anywhere on this window</p>
                  </>
                )}
              </div>
            )}
          </div>
        </section>
    </AppShell>
  );
}

function parseDate(raw: string | undefined): Date | null {
  return raw ? parseTimestamp(raw) : null;
}
function historyOf(source: DesktopSource, history: HistoryRow[]) {
  return source.type === "saved" ? source.row : history.find((row) => row.id === source.id);
}
function rowDate(source: DesktopSource, history: HistoryRow[]): string {
  if (activeOf(source)) return "";
  const date = parseDate(historyOf(source, history)?.created_at);
  if (!date) return "";
  return date.toLocaleDateString() === new Date().toLocaleDateString()
    ? date.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" })
    : date.toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
/** Item-type label for a sidebar row (WS-4): what the job produced — media
 * actions first (Audio / Video / + transcript), then the non-media kinds. A
 * queue row reads its `media` spec; a saved row reads `media_action` on the
 * history record. */
function typeLabelForSource(source: DesktopSource, history: HistoryRow[]): string {
  const spec = source.type === "queue" ? source.job.media : undefined;
  const action =
    spec?.action === "images"
      ? "download_images"
      : spec && spec.action !== "transcribe"
        ? `${spec.action === "both" ? "both" : "download"}_${spec.video ? "video" : "audio"}`
        : (historyOf(source, history)?.media_action ?? "transcribe");
  switch (action) {
    case "download_images":
      return IMAGE_KIND_LABEL;
    case "download_audio":
      return "Audio";
    case "download_video":
      return "Video";
    case "both_audio":
      return "Audio + transcript";
    case "both_video":
      return "Video + transcript";
  }
  const kind = kindOf(source);
  if (kind === "carousel") return "Carousel";
  if (kind === "article") return "Article";
  if (kind === "local_file") {
    // "Image"/"PDF" are already the right item types for local files; any
    // other local file transcribes.
    const label = labelForSource(kind, inputOf(source));
    return label === "Image" || label === "PDF" ? label : "Transcript";
  }
  return "Transcript";
}

function sourceSubtitle(source: DesktopSource, history: HistoryRow[]): string {
  const row = historyOf(source, history);
  const duration = row?.duration_s;
  const label = labelForSource(kindOf(source), inputOf(source));
  const type = typeLabelForSource(source, history);
  // "YouTube · Video · 67:22" — but a local image stays "Image · 0:12", not
  // "Image · Image".
  const head = type === label ? label : `${label} · ${type}`;
  return duration
    ? `${head} · ${Math.floor(duration / 60)}:${String(Math.floor(duration % 60)).padStart(2, "0")}`
    : head;
}
function sourceSections(
  sources: DesktopSource[],
  history: HistoryRow[],
): [string, DesktopSource[]][] {
  const groups = new Map<string, DesktopSource[]>();
  const active = sources.filter((source) => source.type === "queue" && source.job.stage !== "done");
  const priority = (source: DesktopSource) =>
    source.type !== "queue"
      ? 4
      : source.job.needs_auth
        ? 0
        : ["error", "cancelled"].includes(source.job.stage)
          ? 1
          : source.job.stage === "queued"
            ? 3
            : 2;
  active.sort((left, right) => priority(left) - priority(right));
  if (active.length) groups.set("In Progress", active);
  const done = sources
    .filter((source) => source.type !== "queue" || source.job.stage === "done")
    .sort(
      (a, b) =>
        (parseDate(historyOf(b, history)?.created_at)?.getTime() ?? 0) -
        (parseDate(historyOf(a, history)?.created_at)?.getTime() ?? 0),
    );
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const yesterday = new Date(today);
  yesterday.setDate(yesterday.getDate() - 1);
  const week = new Date(today);
  week.setDate(week.getDate() - 7);
  for (const source of done) {
    const date = parseDate(historyOf(source, history)?.created_at);
    const group = !date
      ? "Recent"
      : date >= today
        ? "Today"
        : date >= yesterday
          ? "Yesterday"
          : date >= week
            ? "Previous 7 Days"
            : date.toLocaleDateString(undefined, {
                month: "long",
                ...(date.getFullYear() !== today.getFullYear() ? { year: "numeric" as const } : {}),
              });
    groups.set(group, [...(groups.get(group) ?? []), source]);
  }
  return [...groups];
}
function ProgressRing({ pct, size = 28 }: { pct: number; size?: number }) {
  return (
    <svg className="r" width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
      <circle className="trk" cx="16" cy="16" r="13.8" />
      <circle
        className="bar"
        cx="16"
        cy="16"
        r="13.8"
        pathLength="100"
        strokeDasharray="100"
        strokeDashoffset={100 - Math.max(0, Math.min(100, pct))}
      />
    </svg>
  );
}
function SourceBadge({ source }: { source: DesktopSource }) {
  const platform = sourcePlatform(kindOf(source), inputOf(source));
  if (activeOf(source))
    return (
      <span className="ring">
        <ProgressRing pct={source.type === "queue" ? source.job.pct : 0} />
        {platform ? <SourcePlatformIcon platform={platform} size={13} /> : <Video size={12} />}
      </span>
    );
  if (platform)
    return (
      <span className="badge source-platform-badge" style={{ background: platform.color }}>
        <SourcePlatformIcon platform={platform} />
        {source.type === "queue" && source.job.stage === "error" && <i className="bdot fail" />}
      </span>
    );
  const label = labelForSource(kindOf(source), inputOf(source));
  let code = "TXT",
    color = "#68707A";
  if (label === "YouTube") {
    code = "YT";
    color = "#B45424";
  } else if (label === "PDF") {
    code = "PDF";
    color = "#B83A2F";
  } else if (label === "Image") {
    code = "IMG";
    color = "#8A3B8E";
  } else if (categoryForKind(kindOf(source)) === "web") {
    code = "WEB";
    color = "#0F737A";
  } else if (["carousel", "reel", "instagram_post"].includes(kindOf(source))) {
    code = "IG";
    color = "#7A5CAE";
  } else if (kindOf(source) === "media_url") {
    code = "AUD";
    color = "#1F5FAE";
  } else if (kindOf(source) === "local_file") {
    const ext = inputOf(source).split(".").pop()?.toUpperCase();
    code = (ext ?? "TXT").slice(0, 4);
  } else if (kindOf(source) === "tiktok") {
    code = "TT";
    color = "#B45424";
  }
  return (
    <span
      className={`badge${code.length === 4 ? " q4" : ""}`}
      style={{ background: color }}
      aria-hidden="true"
    >
      {code.split("").map((letter, index) => (
        <span
          key={`${letter}:${code.slice(0, index + 1)}`}
          style={{ fontSize: code.length === 4 ? 7.5 : Math.min((23 / code.length) * 0.86, 13.2) }}
        >
          {letter}
        </span>
      ))}
      {source.type === "queue" && source.job.stage === "error" && <i className="bdot fail" />}
    </span>
  );
}
