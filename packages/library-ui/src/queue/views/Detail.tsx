import { createPortal } from "react-dom";
import {
  useCallback,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { JobEvent, TranscriptDetail, TranscriptVariant } from "../contract.js";
import { invoke, listen, save as saveDialog } from "../lib/ipc/index.js";
import { isTerminalStage } from "../lib/jobState.js";
import { formatInvokeError } from "../lib/errorMessage.js";
import { useFlash } from "../lib/flash.js";
import { runTranscriptFileAction } from "../lib/transcriptActions.js";
import { copyTextToClipboard } from "../lib/workbench.js";
import { labelForSource, categoryForKind } from "../lib/jobDisplay.js";
import { modKeyLabel } from "../lib/platform.js";
import { isImageRow, savedMediaKind } from "../lib/mediaSelection.js";
import { categoryIcon } from "../components/CategoryIcon.js";
import { cx, ContextMenu, type MenuItem } from "@rightkit/react-shell/react";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { moveIndex } from "@rightkit/platform-ui/headless";
import { buttonClass, errorTextClass, mutedTextClass } from "../../ui.js";
import { BackButton, DetailShell, TabButton } from "../../reader/ReaderChrome.js";
import {
  countEntryMatches,
  countMatches,
  FIND_HIGHLIGHT_CAP,
  findTextForMarkdown,
  normalizeFindQuery,
  transcriptEntries,
} from "../../reader/find.js";
import { toLibraryDetail } from "../lib/libraryDetail.js";
import {
  CarouselLayout,
  countWords,
  FindBar,
  ImageGrid,
  MediaPlayer,
  readingMinutes,
  textForTab,
  TranscriptBody,
  type DetailTab,
} from "../../reader/ReaderParts.js";
import { SourcePlatformIcon, sourcePlatform } from "../components/SourcePlatformIcon.js";
import {
  Check,
  ChevronUp,
  AudioLines,
  Clock3,
  Copy,
  ExternalLink,
  FileText,
  FolderOpen,
  MoreHorizontal,
  Link2,
  RefreshCcw,
  Save,
  Search,
  Text,
  Video,
} from "lucide-react";
import { libraryAppName as hostAppName } from "../../host.js";

/** Width of the Save / More transcript menus (right-aligned under their trigger). */
const MENU_WIDTH = 220;

function isHttpUrl(value: string): boolean {
  return /^https?:\/\//i.test(value.trim());
}

interface Props {
  jobId: string;
  onBack?: () => void;
  onRerun?: (input: string) => void;
  // S-6: optional error surface so the Open file / Open source / Show in
  // folder / Save As buttons can stop swallowing failures. App.tsx wires
  // `setErr` so detail-view errors land in the same banner as queue errors.
  onError?: (message: string) => void;
  desktop?: boolean;
  toolbarTarget?: HTMLElement | null;
  metadata?: { duration_s?: number | null; created_at?: string | null };
  onRemoveQueue?: () => void;
  /** Opens Settings on the Pro section (truncation banner / gated exports). */
  onOpenPro?: () => void;
  /** WS-6: the in-flight transcribe attempt targeting this item, if any. */
  attachAttempt?: { id: string; stage: string; pct: number; error?: string | null };
  /** WS-6: offered on saved download rows — starts transcribe_saved_media. */
  onTranscribe?: () => void;
  /** Pro entitlement (the host's licence state); gates timed exports and transcribe. */
  pro?: boolean;
}

export function Detail({
  jobId,
  onBack,
  onRerun,
  onError,
  desktop = false,
  toolbarTarget,
  metadata,
  onRemoveQueue,
  onOpenPro,
  attachAttempt,
  onTranscribe,
  pro = false,
}: Props) {
  const [detail, setDetail] = useState<TranscriptDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [tab, setTab] = useState<DetailTab>("txt");
  const [activeSlide, setActiveSlide] = useState(0);
  const [copied, flashCopied] = useFlash(1500);
  const [urlCopied, flashUrlCopied] = useFlash(1500);
  const [findOpen, setFindOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [matchIndex, setMatchIndex] = useState(0);
  const [showTimestamps, setShowTimestamps] = useState(true);
  // Save / More menus: an anchored ContextMenu opened from a toolbar trigger.
  const [menu, setMenu] = useState<{ kind: "export" | "more"; x: number; y: number } | null>(null);
  // WS-7 in-reader player: the media element lives in MediaPlayer; timestamped
  // transcript lines seek it via `seekMediaTo`, and `playheadS` (updated from
  // timeupdate/seeked events) drives the current-line highlight. `mediaBroken`
  // is set when the WebView can't decode the file — the player collapses to an
  // "Open in default player" fallback and lines stop offering seek.
  const mediaElRef = useRef<HTMLMediaElement | null>(null);
  const [playheadS, setPlayheadS] = useState<number | null>(null);
  const [mediaBroken, setMediaBroken] = useState(false);
  const findTriggerRef = useRef<HTMLButtonElement | null>(null);

  // Fetch sequencing: a terminal-event refetch issued after the initial
  // load must not be overwritten by that older, slower response — the
  // newest request wins regardless of arrival order.
  const loadSeqRef = useRef(0);
  // Tracks whether any response has ever landed: when the newest request
  // fails and nothing usable was ever loaded, the error must surface —
  // discarding a superseded success must not leave a silent blank reader.
  const hasDetailRef = useRef(false);
  const loadDetail = useCallback(
    (options?: { showLoading?: boolean }) => {
      if (options?.showLoading) {
        setLoading(true);
        setErr(null);
        // A fresh (re)mount load — previous detail no longer counts as
        // usable data for this view.
        hasDetailRef.current = false;
      }
      const seq = ++loadSeqRef.current;
      return invoke<TranscriptDetail>("get_transcript", { id: jobId })
        .then(async (slim) => {
          // The slim reply carries a text preview only; fetch each variant
          // the row has on its own request (each reply stays under the cap).
          const loaded: TranscriptDetail = { ...slim };
          const kinds = slim.variants ?? [];
          const fetched = await Promise.all(
            kinds.map((v) =>
              invoke<TranscriptVariant>("get_transcript_variant", {
                id: jobId,
                kind: v.kind,
              }).catch(() => null),
            ),
          );
          for (const v of fetched) {
            if (!v || v.text == null) continue;
            (loaded as unknown as Record<string, unknown>)[v.kind] = v.text;
          }
          return loaded;
        })
        .then((loaded) => {
          if (seq !== loadSeqRef.current) return;
          hasDetailRef.current = true;
          setDetail(loaded);
          setShowTimestamps(Boolean(loaded.timed_text?.trim()));
          setErr(null);
          setLoading(false);
        })
        .catch((e) => {
          if (seq !== loadSeqRef.current) return;
          if (options?.showLoading || !hasDetailRef.current) setErr(String(e));
          setLoading(false);
        });
    },
    [jobId],
  );

  useEffect(() => {
    setMediaBroken(false);
    setPlayheadS(null);
    void loadDetail({ showLoading: true });
  }, [loadDetail]);

  // A reader opened mid-run (or while a re-run is in flight) held a
  // pre-final snapshot — refetch when the job reaches a terminal stage so
  // the view converges on the artifacts that actually exist.
  useEffect(() => {
    const un = listen<JobEvent>("job://update", (e) => {
      // WS-6: an attach attempt carries its own job id — its terminal event
      // still means this row's transcript changed, so refetch on either id.
      const match = e.payload.id === jobId || e.payload.id === attachAttempt?.id;
      if (!match || !isTerminalStage(e.payload.stage)) return;
      void loadDetail();
    });
    return () => {
      un.then((f) => f());
    };
  }, [jobId, attachAttempt?.id, loadDetail]);

  // Find works on the deferred query so typing never blocks on re-highlighting,
  // and counts over the same text the active view renders (see reader/find.ts).
  const deferredQuery = useDeferredValue(query);
  const findQuery = normalizeFindQuery(deferredQuery);
  const matchCount = useMemo(() => {
    if (!detail || !findQuery) return 0;
    if (desktop) {
      return countEntryMatches(
        transcriptEntries(toLibraryDetail(detail), showTimestamps && Boolean(detail.timed_text?.trim())),
        findQuery,
      );
    }
    const text = textForTab(toLibraryDetail(detail), tab);
    const isMd = tab === "md" || tab === "mdTime";
    return countMatches(isMd ? findTextForMarkdown(text) : text, findQuery);
  }, [detail, desktop, findQuery, tab, showTimestamps]);

  // Reset the active match whenever the query or visible text changes so the
  // counter and scrolling stay in sync.
  // biome-ignore lint/correctness/useExhaustiveDependencies: query/tab are the intended triggers, not values read inside the effect
  useEffect(() => {
    setMatchIndex(0);
  }, [query, tab]);

  const stepMatch = useCallback(
    (delta: number) => {
      if (matchCount === 0) return;
      setMatchIndex((current) => moveIndex(current, matchCount, delta < 0 ? "prev" : "next", true));
    },
    [matchCount],
  );

  const handleCopy = useCallback(
    (format: DetailTab = tab) => {
      if (!detail) return;
      copyTextToClipboard(textForTab(toLibraryDetail(detail), format))
        .then(flashCopied)
        .catch((e) => onError?.(formatInvokeError(e)));
    },
    [detail, tab, flashCopied, onError],
  );

  const handleCopyUrl = useCallback(() => {
    if (!detail?.url) return;
    copyTextToClipboard(detail.url)
      .then(flashUrlCopied)
      .catch((e) => onError?.(formatInvokeError(e)));
  }, [detail, flashUrlCopied, onError]);

  const handleOpenUrl = useCallback(() => {
    if (!detail) return;
    // S-6: surface failures (no such transcript, OS handler refused, etc.) via
    // the same banner the queue uses instead of swallowing them.
    runTranscriptFileAction(invoke, "openSource", detail.id, (m) => onError?.(m));
  }, [detail, onError]);

  const handleOpenFile = useCallback(() => {
    if (!detail) return;
    runTranscriptFileAction(invoke, "openTranscript", detail.id, (m) => onError?.(m));
  }, [detail, onError]);

  const handleOpenMedia = useCallback(() => {
    if (!detail?.media_path) return;
    runTranscriptFileAction(invoke, "openMedia", detail.id, (m) => onError?.(m));
  }, [detail, onError]);

  const handleReRun = useCallback(() => {
    if (!detail?.url) return;
    onRerun?.(detail.url);
  }, [detail, onRerun]);

  const handleReveal = useCallback(() => {
    if (!detail) return;
    runTranscriptFileAction(invoke, "revealTranscript", detail.id, (m) => onError?.(m));
  }, [detail, onError]);

  const handleRevealMedia = useCallback(() => {
    if (!detail?.media_path) return;
    runTranscriptFileAction(invoke, "revealMedia", detail.id, (m) => onError?.(m));
  }, [detail, onError]);

  // Timestamp click → seek. Sets only currentTime; if the element is already
  // playing it simply continues from the new position (no forced autoplay).
  const seekMediaTo = useCallback((seconds: number) => {
    const el = mediaElRef.current;
    if (!el) return;
    try {
      el.currentTime = seconds;
    } catch {
      // Element detached mid-seek — harmless.
    }
  }, []);

  // Quantize playhead updates to whole seconds: the active-line lookup is per
  // timed line, so sub-second state churn from timeupdate (~4/s while playing)
  // would only re-render the transcript for nothing.
  const handleMediaTime = useCallback((seconds: number) => {
    setPlayheadS((prev) =>
      prev != null && Math.floor(prev) === Math.floor(seconds) ? prev : seconds,
    );
  }, []);

  // The transcript's SRT doubles as the media element's caption track:
  // re-timecoded as WebVTT and served from an in-memory blob URL.
  const captionsUrl = useMemo(() => {
    const srt = detail?.srt?.trim();
    if (!srt) return null;
    const vtt = `WEBVTT\n\n${srt.replace(/(\d{2}:\d{2}:\d{2}),(\d{3})/g, "$1.$2")}`;
    return URL.createObjectURL(new Blob([vtt], { type: "text/vtt" }));
  }, [detail?.srt]);
  useEffect(
    () => () => {
      if (captionsUrl) URL.revokeObjectURL(captionsUrl);
    },
    [captionsUrl],
  );

  const handleSaveAs = useCallback(
    async (format: DetailTab = tab) => {
      if (!detail) return;
      const ext = format === "srt" ? "srt" : format.startsWith("md") ? "md" : "txt";
      const defaultName = `${(detail.title ?? "transcript").replace(/[^a-zA-Z0-9 _-]/g, "_").slice(0, 60)}.${ext}`;
      const dest = await saveDialog({
        defaultPath: defaultName,
        filters: [{ name: "Text", extensions: [ext] }],
      });
      if (!dest) return;
      // Typed export: the backend resolves the recorded artifact body and
      // enforces the Free/Pro timed-format gate — the WebView can't widen it.
      invoke("save_transcript_as", { id: detail.id, dest, format }).catch((e) =>
        onError?.(formatInvokeError(e)),
      );
    },
    [detail, onError, tab],
  );

  const toggleMenu = useCallback((kind: "export" | "more", trigger: HTMLElement) => {
    const rect = trigger.getBoundingClientRect();
    setMenu((current) =>
      current?.kind === kind
        ? null
        : { kind, x: Math.max(8, rect.right - MENU_WIDTH), y: rect.bottom + 4 },
    );
  }, []);
  const closeMenu = useCallback(() => setMenu(null), []);

  // Memoized: ContextMenu resets its highlight whenever the items array changes,
  // and this view re-renders on every playhead tick while media plays.
  const menuItems = useMemo<MenuItem[]>(() => {
    if (!menu || !detail) return [];
    const hasTranscript = Boolean(detail.text.trim());
    const hasTimedText = Boolean(detail.timed_text?.trim());
    const hasTimedMd = Boolean(detail.timed_md_text?.trim());
    const items: MenuItem[] = [];
    if (menu.kind === "export") {
      const proTag = (ext: string) =>
        pro ? <span>{ext}</span> : <span className="claude-menu-pro">Pro</span>;
      items.push(
        { id: "txt", label: <>Plain Text <span>.txt</span></>, run: () => void handleSaveAs("txt") },
        { id: "md", label: <>Markdown <span>.md</span></>, run: () => void handleSaveAs("md") },
        {
          id: "txtTime",
          label: <>Timed Text {proTag(".txt")}</>,
          disabled: !hasTimedText || !pro,
          run: () => void handleSaveAs("txtTime"),
        },
        {
          id: "mdTime",
          label: <>Timed Markdown {proTag(".md")}</>,
          disabled: !hasTimedMd || !pro,
          run: () => void handleSaveAs("mdTime"),
        },
        {
          id: "srt",
          label: <>Subtitles {proTag(".srt")}</>,
          disabled: !detail.srt?.trim() || !pro,
          run: () => void handleSaveAs("srt"),
        },
      );
      return items;
    }
    if (hasTranscript && detail.media_path) {
      items.push(
        { id: "open-download", label: "Open Download", run: handleOpenMedia },
        { id: "reveal-download", label: "Show Download in Folder", run: handleRevealMedia },
      );
    }
    if (onRemoveQueue) items.push({ id: "remove-queue", label: "Remove from Queue", run: onRemoveQueue });
    return items;
  }, [menu, detail, pro, onRemoveQueue, handleSaveAs, handleOpenMedia, handleRevealMedia]);

  // A different item (or a refetched detail whose media changed — e.g. a
  // download that gained a transcript) starts the player fresh; a stale
  // playhead would otherwise highlight the wrong line.
  const seenDetailId = useRef<string | null>(null);
  useEffect(() => {
    const id = detail?.id ?? null;
    if (seenDetailId.current === id) return;
    seenDetailId.current = id;
    setPlayheadS(null);
    setMediaBroken(false);
  }, [detail]);

  // Ctrl/Cmd+F opens the in-transcript find bar (only where find applies).
  useEffect(() => {
    if (!detail || detail.slides.length > 0 || tab === "srt") return;
    const registry = createShortcutRegistry();
    registry.register({
      id: "detail.find",
      chord: "ctrlormeta+f",
      label: "Find in transcript",
      run: () => setFindOpen(true),
    });
    return registry.attach(window);
  }, [detail, tab]);

  if (loading) {
    return (
      <DetailShell>
        <div className={mutedTextClass}>Loading...</div>
      </DetailShell>
    );
  }

  if (err) {
    return (
      <DetailShell>
        {onBack && <BackButton onBack={onBack} />}
        <div className={errorTextClass}>Failed to load transcript: {err}</div>
      </DetailShell>
    );
  }

  if (!detail) return null;

  // Image jobs (#248) are recorded as carousels, so the image check must run
  // before the carousel check; `slides` lists the saved image files.
  const isImages = isImageRow(detail);
  const isCarousel = !isImages && detail.slides.length > 0;
  const hasTranscript = Boolean(detail.text.trim());
  const hasTimedText = Boolean(detail.timed_text?.trim());
  const hasTimedMd = Boolean(detail.timed_md_text?.trim());
  const findable = hasTranscript && !isCarousel && tab !== "srt";
  const words = countWords(detail.text);
  const hasUrl = Boolean(detail.url?.trim());
  const mediaKind = savedMediaKind(detail.media_meta, detail.media_path);
  const mediaParts = detail.media_path?.split(/[\\/]/);
  const mediaName = mediaParts?.[mediaParts.length - 1];
  const downloadOnly = Boolean(detail.media_path && !hasTranscript && !isCarousel);
  const canShowTimestamps = hasTimedText;
  const extendedDetail = detail as TranscriptDetail & {
    duration_s?: number | null;
    created_at?: string | null;
  };

  if (desktop) {
    const hasMore = Boolean((hasTranscript && detail.media_path) || onRemoveQueue);
    const toolbar = toolbarTarget
      ? createPortal(
          <div className="claude-toolbar">
            {findable && (
              <div className="claude-toolbar-find">
                {findOpen ? (
                  <FindBar
                    query={query}
                    onQuery={setQuery}
                    matchCount={matchCount}
                    matchIndex={matchIndex}
                    onStep={stepMatch}
                    onClose={() => {
                      setFindOpen(false);
                      setQuery("");
                      requestAnimationFrame(() => findTriggerRef.current?.focus());
                    }}
                  />
                ) : (
                  <button
                    type="button"
                    className="claude-find-field"
                    aria-label="Search transcript"
                    title={`Search transcript (${modKeyLabel}F)`}
                    ref={findTriggerRef}
                    onClick={() => setFindOpen(true)}
                  >
                    <Search size={14} strokeWidth={1.8} />
                    <span className="ph">Find in transcript</span>
                    <kbd>{modKeyLabel} F</kbd>
                  </button>
                )}
              </div>
            )}
            <button
              type="button"
              className="claude-tool"
              title={showTimestamps ? "Hide timestamps" : "Show timestamps"}
              aria-label="Timestamps"
              aria-pressed={showTimestamps && canShowTimestamps}
              disabled={!canShowTimestamps}
              onClick={() => setShowTimestamps((value) => !value)}
            >
              <Clock3 size={17} strokeWidth={1.8} />
            </button>
            <span className="claude-toolbar-spacer" />
            <button
              type="button"
              className="claude-tool claude-copy"
              onClick={() => handleCopy(showTimestamps && hasTimedText ? "txtTime" : "txt")}
              disabled={(!hasTranscript && !detail.media_path) || isCarousel}
              aria-label="Copy text"
            >
              {copied ? <Check size={16} /> : <Copy size={16} strokeWidth={1.8} />}
              <span>{copied ? "Copied" : "Copy"}</span>
            </button>
            <button
              type="button"
              className={`claude-tool ${menu?.kind === "export" ? "is-open" : ""}`}
              title="Save as"
              aria-label="Save"
              aria-haspopup="menu"
              aria-expanded={menu?.kind === "export"}
              disabled={!hasTranscript || isCarousel}
              onClick={(event) => toggleMenu("export", event.currentTarget)}
            >
              <Save size={16} strokeWidth={1.8} />
              <span>Save as</span>
              <ChevronUp size={13} strokeWidth={1.8} />
            </button>
            <span className="claude-toolbar-separator" />
            {isHttpUrl(detail.url) && (
              <button
                type="button"
                className="claude-tool is-secondary"
                title="Open Source in Browser"
                onClick={handleOpenUrl}
              >
                <ExternalLink size={16} strokeWidth={1.8} />
                <span>Open Source</span>
              </button>
            )}
            {(hasTranscript || detail.media_path) && (
              <button
                type="button"
                className="claude-tool is-secondary"
                title="Show in Folder"
                onClick={hasTranscript ? handleReveal : handleRevealMedia}
              >
                <FolderOpen size={16} strokeWidth={1.8} />
                <span>Show in Folder</span>
              </button>
            )}
            {(hasTranscript || detail.media_path) && (
              <button
                type="button"
                className="claude-tool is-secondary"
                title={hasTranscript ? "Open Transcript File" : "Open Download"}
                onClick={hasTranscript ? handleOpenFile : handleOpenMedia}
              >
                <FileText size={16} strokeWidth={1.8} />
                <span>Open File</span>
              </button>
            )}
            {hasUrl && (
              <button
                type="button"
                className="claude-tool is-secondary"
                title="Copy Source Link"
                onClick={handleCopyUrl}
              >
                <Link2 size={16} strokeWidth={1.8} />
                <span>Copy Link</span>
              </button>
            )}
            {hasUrl && hasTranscript && onRerun && (
              <button
                type="button"
                className="claude-tool is-secondary"
                title="Get Text Again"
                onClick={handleReRun}
              >
                <RefreshCcw size={16} strokeWidth={1.8} />
                <span>Get Text Again</span>
              </button>
            )}
            {hasMore && (
              <button
                type="button"
                className={`claude-tool ${menu?.kind === "more" ? "is-open" : ""}`}
                title="More"
                aria-label="More"
                aria-haspopup="menu"
                aria-expanded={menu?.kind === "more"}
                onClick={(event) => toggleMenu("more", event.currentTarget)}
              >
                <MoreHorizontal size={18} strokeWidth={1.8} />
              </button>
            )}
            {menu && menuItems.length > 0 && (
              <ContextMenu
                x={menu.x}
                y={menu.y}
                width={MENU_WIDTH}
                items={menuItems}
                itemAs="button"
                showLead={false}
                label={menu.kind === "export" ? "Save transcript" : "More transcript actions"}
                onClose={closeMenu}
              />
            )}
          </div>,
          toolbarTarget,
        )
      : null;
    const durationValue = metadata?.duration_s ?? extendedDetail.duration_s;
    const dateValue = metadata?.created_at ?? extendedDetail.created_at;
    const duration = durationValue == null ? null : formatDuration(durationValue);
    const date = dateValue ? formatDetailDate(dateValue) : null;
    const platform = sourcePlatform(detail.kind, detail.url);
    const SourceIcon = categoryIcon(categoryForKind(detail.kind)) ?? FileText;
    return (
      <DetailShell>
        <article className={`claude-doc${downloadOnly ? " claude-download-doc" : ""}`}>
          <div className="claude-doc-content">
            {downloadOnly ? (
              <div className="claude-download-state">
                <span className="claude-download-halo">
                  {mediaKind === "audio" ? <AudioLines size={26} /> : <Video size={26} />}
                </span>
                <h2>{mediaKind === "audio" ? "Audio saved" : "Video saved"}</h2>
                <p title={detail.media_path ?? undefined}>{mediaName}</p>
                <div className="claude-download-actions">
                  <button
                    type="button"
                    className="claude-download-primary"
                    onClick={handleOpenMedia}
                  >
                    <ExternalLink size={14} /> Open Download
                  </button>
                  <button
                    type="button"
                    className="claude-download-secondary"
                    onClick={handleRevealMedia}
                  >
                    <FolderOpen size={14} /> Show in Folder
                  </button>
                </div>
                {detail.media_path && (
                  <div className="claude-download-player">
                    <MediaPlayer
                      key={`${detail.id}:${detail.media_path}`}
                      playbackId={detail.id}
                      src={detail.media_path}
                      kind={mediaKind}
                      captionsUrl={captionsUrl}
                      mediaRef={mediaElRef}
                      onTimeChange={handleMediaTime}
                      onPlaybackError={() => setMediaBroken(true)}
                      onReady={() => setMediaBroken(false)}
                      onOpenExternal={handleOpenMedia}
                    />
                  </div>
                )}
                {hasUrl && isHttpUrl(detail.url) && (
                  <button
                    className="claude-source"
                    type="button"
                    onClick={handleOpenUrl}
                    title={detail.url}
                  >
                    <ExternalLink size={13} />
                    <span>{detail.url}</span>
                  </button>
                )}
              </div>
            ) : (
              <>
                <div className="claude-kind">
                  {platform ? (
                    <SourcePlatformIcon platform={platform} size={14} />
                  ) : (
                    <SourceIcon size={14} strokeWidth={1.8} />
                  )}
                  <span>{labelForSource(detail.kind, detail.url)}</span>
                </div>
                <h1 title={detail.title ?? detail.id}>{detail.title ?? detail.id}</h1>
                {hasTranscript && (
                  <div className="claude-meta">
                    {duration && <span>{duration}</span>}
                    <span>{words.toLocaleString()} words</span>
                    <span>{readingMinutes(words)} min read</span>
                    {date && <span>{date}</span>}
                  </div>
                )}
              </>
            )}
            {!downloadOnly && hasUrl && isHttpUrl(detail.url) && (
              <button
                className="claude-source"
                type="button"
                onClick={handleOpenUrl}
                title={detail.url}
              >
                <ExternalLink size={13} />
                <span>{detail.url}</span>
              </button>
            )}
            {!downloadOnly && detail.media_path && (
              <div className="desktop-reader-media flex min-w-0 flex-wrap items-center gap-3 border-b border-[color-mix(in_srgb,var(--line)_55%,transparent)] py-2">
                <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-[8px] bg-[color-mix(in_srgb,var(--accent)_14%,transparent)] text-[var(--accent)]">
                  {mediaKind === "audio" ? <FileText size={19} /> : <Video size={19} />}
                </span>
                <span className="min-w-0 flex-1">
                  <span className="block text-sm font-medium text-[var(--paper)]">
                    {mediaKind === "audio" ? "Downloaded audio" : "Downloaded video + audio"}
                  </span>
                  <span className="block truncate font-[family-name:var(--mono)] text-[11px] text-[var(--muted)]">
                    {mediaName}
                  </span>
                </span>
                <button type="button" className={buttonClass} onClick={handleOpenMedia}>
                  <ExternalLink size={15} />
                  <span>Open download</span>
                </button>
                <button type="button" className={buttonClass} onClick={handleRevealMedia}>
                  <FolderOpen size={15} />
                  <span>Folder</span>
                </button>
              </div>
            )}
            {!downloadOnly && detail.media_path && (
              <div className="desktop-reader-player">
                <MediaPlayer
                  key={`${detail.id}:${detail.media_path}`}
                  playbackId={detail.id}
                  src={detail.media_path}
                  kind={mediaKind}
                  captionsUrl={captionsUrl}
                  mediaRef={mediaElRef}
                  onTimeChange={handleMediaTime}
                  onPlaybackError={() => setMediaBroken(true)}
                  onReady={() => setMediaBroken(false)}
                  onOpenExternal={handleOpenMedia}
                />
              </div>
            )}
            {isImages ? (
              <ImageGrid files={detail.slides} onReveal={handleRevealMedia} />
            ) : isCarousel ? (
              <CarouselLayout detail={toLibraryDetail(detail)} activeSlide={activeSlide} onSlide={setActiveSlide} />
            ) : hasTranscript ? (
              <DesktopTranscript
                detail={detail}
                timestamps={showTimestamps && hasTimedText}
                query={findable ? findQuery : ""}
                matchIndex={matchIndex}
                playheadS={playheadS}
                onSeekTime={detail.media_path && !mediaBroken ? seekMediaTo : undefined}
              />
            ) : null}
            {detail.truncated_at_s != null && (
              <div className="claude-truncation-banner" role="note">
                <span>
                  {hostAppName()} Free transcribes the first {Math.round(detail.truncated_at_s / 60)}{" "}
                  minutes — this transcript stops early.
                </span>
                <button type="button" onClick={() => onOpenPro?.()}>
                  Get Pro for the full transcript
                </button>
              </div>
            )}
          </div>
        </article>
        {toolbar}
      </DetailShell>
    );
  }

  return (
    <DetailShell>
      <header className="desktop-reader-header flex min-w-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-[color-mix(in_srgb,var(--line)_55%,transparent)] pb-2">
        {onBack && <BackButton onBack={onBack} />}
        <span className="desktop-reader-title flex min-w-[180px] flex-1 flex-col gap-0.5">
          <span
            className="truncate text-[15px] font-medium text-[var(--paper)]"
            title={detail.title ?? detail.id}
          >
            {detail.title ?? detail.id}
          </span>
          <span className="desktop-reader-meta flex flex-wrap items-center gap-x-1.5 gap-y-0.5 font-[family-name:var(--mono)] text-[10px] text-[var(--muted)]">
            {(() => {
              const cat = categoryForKind(detail.kind);
              const CatIcon = categoryIcon(cat);
              return CatIcon ? (
                <span className="inline-flex items-center text-[var(--accent)]">
                  <CatIcon size={12} />
                </span>
              ) : null;
            })()}
            <span>{labelForSource(detail.kind, detail.url)}</span>
            <span>·</span>
            <span>
              {hasTranscript
                ? `${words.toLocaleString()} words · ${readingMinutes(words)} min read`
                : `Downloaded ${mediaKind}`}
            </span>
          </span>
        </span>
        <div className="desktop-reader-toolbar flex flex-wrap items-center justify-end gap-1.5">
          {!isCarousel && hasTranscript && (
            <div className="desktop-reader-formats flex flex-wrap items-center gap-0.5">
              <TabButton active={tab === "txt"} onClick={() => setTab("txt")}>
                TXT
              </TabButton>
              <TabButton active={tab === "md"} onClick={() => setTab("md")}>
                MD
              </TabButton>
              <TabButton
                active={tab === "txtTime"}
                disabled={!hasTimedText}
                onClick={() => setTab("txtTime")}
              >
                TXT+Time
              </TabButton>
              <TabButton
                active={tab === "mdTime"}
                disabled={!hasTimedMd}
                onClick={() => setTab("mdTime")}
              >
                MD+Time
              </TabButton>
              <TabButton
                active={tab === "srt"}
                disabled={!detail.srt}
                onClick={() => setTab("srt")}
              >
                SRT
              </TabButton>
            </div>
          )}
          {findable && (
            <button
              type="button"
              className={cx(buttonClass, findOpen && "border-[var(--accent)] text-[var(--accent)]")}
              onClick={() => setFindOpen((open) => !open)}
              title="Find in transcript"
              aria-label="Find in transcript"
              aria-pressed={findOpen}
            >
              <Search size={15} strokeWidth={1.8} />
              <span>Find</span>
            </button>
          )}
        </div>
      </header>

      {findable && findOpen && (
        <FindBar
          query={query}
          onQuery={setQuery}
          matchCount={matchCount}
          matchIndex={matchIndex}
          onStep={stepMatch}
          onClose={() => {
            setFindOpen(false);
            setQuery("");
          }}
        />
      )}

      {detail.media_path ? (
        <div className="desktop-reader-media flex min-w-0 flex-wrap items-center gap-3 border-b border-[color-mix(in_srgb,var(--line)_55%,transparent)] py-2">
          <span className="flex h-10 w-10 shrink-0 items-center justify-center rounded-[8px] bg-[color-mix(in_srgb,var(--accent)_14%,transparent)] text-[var(--accent)]">
            {mediaKind === "audio" ? <FileText size={19} /> : <Video size={19} />}
          </span>
          <span className="min-w-0 flex-1">
            <span className="block text-sm font-medium text-[var(--paper)]">
              {mediaKind === "audio" ? "Downloaded audio" : "Downloaded video + audio"}
            </span>
            <span className="block truncate font-[family-name:var(--mono)] text-[11px] text-[var(--muted)]">
              {mediaName}
            </span>
          </span>
          {downloadOnly &&
            (attachAttempt ? (
              <span className="attach-progress" role="status" aria-live="polite">
                Transcribing {attachAttempt.pct}%
              </span>
            ) : onTranscribe ? (
              <button
                type="button"
                className="btn primary"
                onClick={pro ? onTranscribe : onOpenPro}
              >
                <Text size={15} strokeWidth={1.8} />
                <span>Transcribe{pro ? "" : " · Pro"}</span>
              </button>
            ) : null)}
          <button type="button" className={buttonClass} onClick={handleOpenMedia}>
            <ExternalLink size={15} strokeWidth={1.8} />
            <span>Open download</span>
          </button>
          <button type="button" className={buttonClass} onClick={handleRevealMedia}>
            <FolderOpen size={15} strokeWidth={1.8} />
            <span>Folder</span>
          </button>
        </div>
      ) : null}

      {detail.media_path ? (
        <div className="desktop-reader-player">
          <MediaPlayer
            key={`${detail.id}:${detail.media_path}`}
            playbackId={detail.id}
            src={detail.media_path}
            kind={mediaKind}
            captionsUrl={captionsUrl}
            mediaRef={mediaElRef}
            onTimeChange={handleMediaTime}
            onPlaybackError={() => setMediaBroken(true)}
            onReady={() => setMediaBroken(false)}
            onOpenExternal={handleOpenMedia}
          />
        </div>
      ) : null}

      {isImages ? (
        <ImageGrid files={detail.slides} onReveal={handleRevealMedia} />
      ) : isCarousel ? (
        <CarouselLayout detail={toLibraryDetail(detail)} activeSlide={activeSlide} onSlide={setActiveSlide} />
      ) : hasTranscript ? (
        <TranscriptBody
          detail={toLibraryDetail(detail)}
          tab={tab}
          query={findable ? findQuery : ""}
          matchIndex={matchIndex}
          playheadS={playheadS}
          onSeekTime={detail.media_path && !mediaBroken ? seekMediaTo : undefined}
        />
      ) : null}

      <footer className="desktop-reader-footer flex flex-wrap items-center justify-end gap-1.5 border-t border-[color-mix(in_srgb,var(--line)_55%,transparent)] pt-2">
        {hasTranscript && (
          <button
            type="button"
            className={buttonClass}
            onClick={() => handleCopy()}
            aria-label="Copy transcript text"
          >
            {copied ? <Check size={15} strokeWidth={2} /> : <Copy size={15} strokeWidth={1.9} />}
            <span>{copied ? "Copied" : "Copy"}</span>
          </button>
        )}
        {hasUrl && (
          <>
            <button
              type="button"
              className={buttonClass}
              onClick={handleCopyUrl}
              title={detail.url}
              aria-label="Copy source URL"
            >
              {urlCopied ? (
                <Check size={15} strokeWidth={2} />
              ) : (
                <Link2 size={15} strokeWidth={1.8} />
              )}
              <span>{urlCopied ? "Copied" : "Copy URL"}</span>
            </button>
            {isHttpUrl(detail.url) && (
              <button
                type="button"
                className={buttonClass}
                onClick={handleOpenUrl}
                title={`Open ${detail.url}`}
                aria-label="Open source URL"
              >
                <ExternalLink size={15} strokeWidth={1.8} />
                <span>Source</span>
              </button>
            )}
          </>
        )}
        {hasTranscript && (
          <button
            type="button"
            className={buttonClass}
            onClick={handleOpenFile}
            title="Open the saved transcript file"
            aria-label="Open transcript file"
          >
            <FileText size={15} strokeWidth={1.8} />
            <span>Open file</span>
          </button>
        )}
        {hasUrl && hasTranscript && onRerun && (
          <button
            type="button"
            className={buttonClass}
            onClick={handleReRun}
            aria-label="Re-run source"
          >
            <RefreshCcw size={15} strokeWidth={1.8} />
            <span>Re-run</span>
          </button>
        )}
        {hasTranscript && (
          <button
            type="button"
            className={buttonClass}
            onClick={handleReveal}
            title="Show the saved files in their folder"
            aria-label="Show in folder"
          >
            <FolderOpen size={15} strokeWidth={1.8} />
            <span>Folder</span>
          </button>
        )}
        {hasTranscript && (
          <button
            type="button"
            className={buttonClass}
            onClick={() => void handleSaveAs()}
            title="Save transcript to a custom location"
            aria-label="Save as"
          >
            <Save size={15} strokeWidth={1.8} />
            <span>Save As</span>
          </button>
        )}
      </footer>
    </DetailShell>
  );
}

function formatDuration(seconds: number): string {
  const total = Math.max(0, Math.floor(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const secs = total % 60;
  return hours
    ? `${hours}:${String(minutes).padStart(2, "0")}:${String(secs).padStart(2, "0")}`
    : `${minutes}:${String(secs).padStart(2, "0")}`;
}

function formatDetailDate(value: string): string | null {
  const numeric = Number(value);
  const date = new Date(
    Number.isFinite(numeric) && numeric > 0 ? (numeric < 1e12 ? numeric * 1000 : numeric) : value,
  );
  if (Number.isNaN(date.getTime())) return null;
  return new Intl.DateTimeFormat(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(date);
}

function DesktopTranscript({
  detail,
  timestamps,
  query,
  matchIndex,
  playheadS,
  onSeekTime,
}: {
  detail: TranscriptDetail;
  timestamps: boolean;
  query: string;
  matchIndex: number;
  /** Seconds into the attached media; drives the current-line highlight. */
  playheadS?: number | null;
  /** Seek handler — present only while a playable media element is mounted. */
  onSeekTime?: (seconds: number) => void;
}) {
  const entries = useMemo(() => transcriptEntries(toLibraryDetail(detail), timestamps), [detail, timestamps]);

  // Active line = the last timestamped entry at or before the playhead.
  // Untimed lines (headings, wrapped prose) never light up on their own.
  const activeEntry = useMemo(() => {
    if (playheadS == null) return -1;
    let index = -1;
    entries.forEach((entry, i) => {
      if (entry.seconds != null && entry.seconds <= playheadS) index = i;
    });
    return index;
  }, [entries, playheadS]);

  let nextMatch = 0;
  const keyCounts = new Map<string, number>();
  return (
    <div className={`claude-body${timestamps ? " has-timestamps" : ""}`}>
      {entries.length ? (
        entries.map((entry, index) => {
          const marked = renderQueryMatches(entry.text, query, matchIndex, () => nextMatch++);
          const baseKey = `${entry.time ?? "p"}-${entry.text}`;
          const occurrence = keyCounts.get(baseKey) ?? 0;
          keyCounts.set(baseKey, occurrence + 1);
          const seekable = onSeekTime != null && entry.seconds != null;
          return (
            <p
              key={`${baseKey}-${occurrence}`}
              data-time={timestamps ? entry.time : undefined}
              className={
                cx(seekable && "has-seek", index === activeEntry && "is-playing") || undefined
              }
            >
              {seekable && (
                <button
                  type="button"
                  className="claude-line-time"
                  title={`Play from ${entry.time}`}
                  aria-label={`Play from ${entry.time}`}
                  onClick={() => onSeekTime(entry.seconds ?? 0)}
                >
                  {entry.time}
                </button>
              )}
              {marked}
            </p>
          );
        })
      ) : (
        <p>No transcript text.</p>
      )}
    </div>
  );
}

function renderQueryMatches(
  text: string,
  query: string,
  activeIndex: number,
  nextIndex: () => number,
): ReactNode {
  const needle = query.trim();
  if (!needle) return text;
  const result: ReactNode[] = [];
  const pattern = new RegExp(needle.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"), "ig");
  let end = 0;
  for (const match of text.matchAll(pattern)) {
    const start = match.index ?? 0;
    if (start > end) result.push(text.slice(end, start));
    const index = nextIndex();
    // Past the highlight cap the match stays plain text; the count still includes it.
    result.push(
      index >= FIND_HIGHLIGHT_CAP ? (
        match[0]
      ) : (
        <mark key={`${start}-${index}`} className={index === activeIndex ? "active" : ""}>
          {match[0]}
        </mark>
      ),
    );
    end = start + match[0].length;
  }
  if (end < text.length) result.push(text.slice(end));
  return result;
}
