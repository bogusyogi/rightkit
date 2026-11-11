import { useCallback, useDeferredValue, useEffect, useMemo, useState } from "react";
import { cx } from "@rightkit/react-shell/react";
import type { LibraryDetail, LibraryDetailView, LibrarySource } from "../contract.js";
import { buttonClass, errorTextClass, mutedTextClass } from "../ui.js";
import { countMatches, findTextForMarkdown, normalizeFindQuery } from "./find.js";
import { BackButton, DetailShell, TabButton } from "./ReaderChrome.js";
import { CarouselLayout, countWords, FindBar, MediaPlayer, readingMinutes, textForTab, TranscriptBody } from "./ReaderParts.js";

const VIEW_LABELS: Record<LibraryDetailView, string> = {
  txt: "Text",
  txtTime: "Text + times",
  md: "Markdown",
  mdTime: "Markdown + times",
  srt: "SRT",
};

/** The views a detail has content for; plain text is always present. */
export function availableViews(detail: LibraryDetail): LibraryDetailView[] {
  const views: LibraryDetailView[] = ["txt"];
  if (detail.timedText?.trim()) views.push("txtTime");
  if (detail.markdown?.trim()) views.push("md");
  if (detail.timedMarkdown?.trim()) views.push("mdTime");
  if (detail.srt?.trim()) views.push("srt");
  return views;
}

const EXTENSIONS: Record<LibraryDetailView, string> = { txt: "txt", txtTime: "txt", md: "md", mdTime: "md", srt: "srt" };

export interface LibraryReaderProps {
  source: LibrarySource;
  id: string;
  onBack?: () => void;
  /** First view shown. Default `"txt"`. */
  initialView?: LibraryDetailView;
}

/**
 * The library reader: one detail from `source.detail(id)` with its views (plain, timed, Markdown,
 * subtitles), find-in-text, media and images, copy and save. ScrapeRight's Detail reader is the
 * reference behaviour.
 */
export function LibraryReader({ source, id, onBack, initialView = "txt" }: LibraryReaderProps) {
  const [detail, setDetail] = useState<LibraryDetail | null>(null);
  const [error, setError] = useState("");
  const [view, setView] = useState<LibraryDetailView>(initialView);
  const [findOpen, setFindOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [matchIndex, setMatchIndex] = useState(0);
  const [activeSlide, setActiveSlide] = useState(0);
  const [notice, setNotice] = useState("");

  useEffect(() => {
    let live = true;
    setDetail(null);
    setError("");
    if (!source.detail) {
      setError("This library has no reader.");
      return;
    }
    source.detail(id).then(
      (next) => live && setDetail(next),
      (e) => live && setError(String(e)),
    );
    return () => {
      live = false;
    };
  }, [source, id]);

  const views = useMemo(() => (detail ? availableViews(detail) : ["txt" as const]), [detail]);
  const shownView = views.includes(view) ? view : "txt";
  const findQuery = normalizeFindQuery(useDeferredValue(query));
  const matchCount = useMemo(() => {
    if (!detail || !findQuery) return 0;
    const text = textForTab(detail, shownView);
    return countMatches(shownView === "md" || shownView === "mdTime" ? findTextForMarkdown(text) : text, findQuery);
  }, [detail, findQuery, shownView]);

  useEffect(() => setMatchIndex(0), [findQuery, shownView]);

  const step = useCallback(
    (delta: number) => setMatchIndex((current) => (matchCount === 0 ? 0 : (current + delta + matchCount) % matchCount)),
    [matchCount],
  );

  const copy = useCallback(async () => {
    if (!detail) return;
    try {
      await source.copyText(textForTab(detail, shownView));
      setNotice("Copied.");
    } catch (e) {
      setError(String(e));
    }
  }, [detail, shownView, source]);

  const save = useCallback(async () => {
    if (!detail || !source.saveText) return;
    try {
      const name = `${detail.title.replace(/[\\/:*?"<>|]+/g, " ").trim() || "untitled"}.${EXTENSIONS[shownView]}`;
      if (await source.saveText(name, textForTab(detail, shownView))) setNotice("Saved.");
    } catch (e) {
      setError(String(e));
    }
  }, [detail, shownView, source]);

  useEffect(() => {
    if (!notice) return;
    const timeout = window.setTimeout(() => setNotice(""), 1500);
    return () => window.clearTimeout(timeout);
  }, [notice]);

  if (!detail) {
    return (
      <DetailShell>
        {onBack ? <BackButton onBack={onBack} /> : null}
        <p className={error ? errorTextClass : mutedTextClass}>{error || "Loading…"}</p>
      </DetailShell>
    );
  }

  const words = countWords(detail.text);
  const images = detail.images ?? [];
  return (
    <DetailShell>
      <div className="flex flex-wrap items-center gap-2">
        {onBack ? <BackButton onBack={onBack} /> : null}
        <h2 className="m-0 min-w-0 flex-1 truncate text-base font-semibold text-[var(--paper)]">{detail.title}</h2>
        <span className={cx(mutedTextClass, "text-xs")}>
          {words.toLocaleString()} words · {readingMinutes(words)} min read
        </span>
      </div>
      <div className="flex flex-wrap items-center gap-1">
        {views.length > 1
          ? views.map((key) => (
              <TabButton key={key} active={shownView === key} onClick={() => setView(key)}>
                {VIEW_LABELS[key]}
              </TabButton>
            ))
          : null}
        <span className="ml-auto flex items-center gap-1">
          {notice ? <output className={cx(mutedTextClass, "text-xs")}>{notice}</output> : null}
          <button type="button" className={buttonClass} onClick={() => setFindOpen((open) => !open)}>
            Find
          </button>
          <button type="button" className={buttonClass} onClick={() => void copy()}>
            Copy
          </button>
          {source.saveText ? (
            <button type="button" className={buttonClass} onClick={() => void save()}>
              Save as…
            </button>
          ) : null}
        </span>
      </div>
      {error ? <p className={errorTextClass}>{error}</p> : null}
      {findOpen ? (
        <FindBar
          query={query}
          onQuery={setQuery}
          matchCount={matchCount}
          matchIndex={matchIndex}
          onStep={step}
          onClose={() => {
            setFindOpen(false);
            setQuery("");
          }}
        />
      ) : null}
      {detail.mediaUrl && detail.mediaKind ? (
        <MediaPlayer src={detail.mediaUrl} kind={detail.mediaKind} title={detail.title} />
      ) : null}
      {images.length > 1 ? (
        <CarouselLayout detail={detail} activeSlide={activeSlide} onSlide={setActiveSlide} />
      ) : (
        <TranscriptBody detail={detail} tab={shownView} query={findQuery} matchIndex={matchIndex} />
      )}
    </DetailShell>
  );
}
