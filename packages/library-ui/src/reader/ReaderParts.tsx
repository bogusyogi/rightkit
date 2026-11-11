import { cx } from "@rightkit/react-shell/react";
import { MarkdownRenderer } from "@rightkit/doc-render";
import { MediaPlayer as SharedMediaPlayer, type MediaPlayerProps } from "@rightkit/media-ui";
import {
  ChevronDown,
  ChevronUp,
  ExternalLink,
  Maximize,
  Pause,
  Play,
  RotateCcw,
  Search,
  Volume2,
  VolumeX,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, type ReactNode } from "react";
import type { LibraryDetail, LibraryDetailView } from "../contract.js";
import { buttonClass, inputClass, mutedTextClass, panelClass } from "../ui.js";
import {
  parseSrt,
  findSegments,
  findTextForMarkdown,
  normalizeFindQuery,
  type SrtCue,
} from "./find.js";

export {
  countEntryMatches,
  countMatches,
  stampToSeconds,
  transcriptEntries,
  normalizeFindQuery,
} from "./find.js";

export type DetailTab = LibraryDetailView;

// Text shown for a given tab — kept in one place so copy, word count, and
// find-in-transcript all operate on exactly what the reader renders.
export function textForTab(detail: LibraryDetail, tab: DetailTab): string {
  switch (tab) {
    case "txt":
      return detail.text;
    case "txtTime":
      return detail.timedText ?? detail.text;
    case "md":
      return detail.markdown ?? detail.text;
    case "mdTime":
      return detail.timedMarkdown ?? detail.markdown ?? detail.text;
    case "srt":
      return detail.srt ?? detail.text;
  }
}

/** Whitespace-delimited word count. */
export function countWords(text: string | null | undefined): number {
  if (!text) return 0;
  return text.trim().split(/\s+/).filter(Boolean).length;
}

// ~200 wpm average reading speed; rounded up so very short transcripts read "1 min".
export function readingMinutes(words: number): number {
  return Math.max(1, Math.round(words / 200));
}

const PLAYER_ICONS: MediaPlayerProps["icons"] = {
  play: <Play size={16} />,
  pause: <Pause size={16} />,
  replay: <RotateCcw size={15} />,
  volume: <Volume2 size={16} />,
  mute: <VolumeX size={16} />,
  fullscreen: <Maximize size={16} />,
};

/** The shared @rightkit/media-ui player dressed as the host app reader player:
 * slim audio strip, lucide icons, speed menu, and the reader's own error card
 * (originals open from the reader toolbar, not the player bar). */
export function MediaPlayer(props: MediaPlayerProps) {
  const { onOpenExternal } = props;
  return (
    <SharedMediaPlayer
      {...props}
      showArtwork={false}
      showOpenOriginal={false}
      icons={PLAYER_ICONS}
      speedControl="menu"
      labels={{ playbackSpeed: "Playback speed" }}
      renderError={({ message }) => (
        <>
          <p role="alert" className="rk-media-error">
            {message}
          </p>
          <div className="rk-media-fallback">
            <span>Media unavailable, corrupt or unsupported. Original download is unchanged.</span>
            {onOpenExternal && (
              <button type="button" className="sr-player-open" onClick={onOpenExternal}>
                <ExternalLink size={14} /> Open in default player
              </button>
            )}
          </div>
        </>
      )}
    />
  );
}

// The SRT tab re-renders on every playhead tick; parse each transcript's SRT once.
let srtCache: { source: string; cues: SrtCue[] } | null = null;
function cachedSrtCues(source: string): SrtCue[] {
  if (srtCache?.source !== source) srtCache = { source, cues: parseSrt(source) };
  return srtCache.cues;
}

export function TranscriptBody({
  detail,
  tab,
  query,
  matchIndex,
  playheadS,
  onSeekTime,
}: {
  detail: LibraryDetail;
  tab: DetailTab;
  query: string;
  matchIndex: number;
  playheadS?: number | null;
  onSeekTime?: (seconds: number) => void;
}) {
  if (tab === "srt") {
    const cues = cachedSrtCues(detail.srt ?? "");
    return (
      <div className="desktop-reader-body desktop-reader-srt min-h-0 flex-1 overflow-auto px-2 py-3">
        {cues.length === 0 ? (
          <span className={mutedTextClass}>No SRT data.</span>
        ) : (
          cues.map((cue, index) => {
            const playing =
              playheadS != null &&
              cue.startS != null &&
              cue.startS <= playheadS &&
              (cue.endS == null || playheadS < cue.endS);
            const seekable = onSeekTime != null && cue.startS != null;
            return (
              <div
                // biome-ignore lint/suspicious/noArrayIndexKey: static, non-reordering parsed SRT cue list
                key={index}
                data-playing={playing || undefined}
                style={{ contentVisibility: "auto" }}
                className="grid grid-cols-[minmax(116px,180px)_minmax(0,1fr)] gap-x-4 border-b border-[color-mix(in_srgb,var(--line)_55%,transparent)] py-3 last:border-b-0 data-[playing]:bg-[color-mix(in_srgb,var(--accent)_18%,transparent)]"
              >
                <span className="font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
                  {seekable ? (
                    <button
                      type="button"
                      className="cursor-pointer text-[var(--accent)] hover:underline"
                      title="Play from here"
                      onClick={() => onSeekTime(cue.startS ?? 0)}
                    >
                      {cue.timecode}
                    </button>
                  ) : (
                    cue.timecode
                  )}
                </span>
                <span className="text-sm leading-6 text-[var(--paper)]">{cue.text}</span>
              </div>
            );
          })
        )}
      </div>
    );
  }

  const text = textForTab(detail, tab);
  const isMd = tab === "md" || tab === "mdTime";

  return (
    <div className="desktop-reader-body min-h-0 flex-1 overflow-auto px-2 py-4 md:px-4 md:py-5">
      {text ? (
        isMd ? (
          <MarkdownBody text={text} query={query} matchIndex={matchIndex} />
        ) : (
          <HighlightedText
            text={text}
            query={query}
            matchIndex={matchIndex}
            asPre={tab === "txtTime"}
          />
        )
      ) : (
        <p className={readerClass}>
          <span className={mutedTextClass}>No transcript text.</span>
        </p>
      )}
    </div>
  );
}

export function FindBar({
  query,
  onQuery,
  matchCount,
  matchIndex,
  onStep,
  onClose,
}: {
  query: string;
  onQuery: (value: string) => void;
  matchCount: number;
  matchIndex: number;
  onStep: (delta: number) => void;
  onClose: () => void;
}) {
  return (
    <div className="desktop-reader-find flex items-center gap-2 py-1">
      <Search className="ml-1 text-[var(--muted)]" size={16} strokeWidth={1.9} aria-hidden="true" />
      <input
        type="search"
        className={cx(inputClass, "h-9 flex-1")}
        value={query}
        onChange={(e) => onQuery(e.target.value)}
        placeholder="Find in transcript..."
        // biome-ignore lint/a11y/noAutofocus: find bar focuses on open so the user can type immediately
        autoFocus
        aria-label="Find in transcript"
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            onStep(e.shiftKey ? -1 : 1);
          } else if (e.key === "Escape") {
            e.preventDefault();
            // Stop here so the window-level Escape handler (which closes Detail) never sees it.
            e.stopPropagation();
            onClose();
          }
        }}
      />
      <span className="min-w-[56px] text-center font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
        {query.trim() ? `${matchCount === 0 ? 0 : matchIndex + 1}/${matchCount}` : ""}
      </span>
      <button
        type="button"
        className={buttonClass}
        onClick={() => onStep(-1)}
        disabled={matchCount === 0}
        title="Previous match"
        aria-label="Previous match"
      >
        <ChevronUp size={15} strokeWidth={1.9} />
      </button>
      <button
        type="button"
        className={buttonClass}
        onClick={() => onStep(1)}
        disabled={matchCount === 0}
        title="Next match"
        aria-label="Next match"
      >
        <ChevronDown size={15} strokeWidth={1.9} />
      </button>
      <button
        type="button"
        className={buttonClass}
        onClick={onClose}
        title="Close find"
        aria-label="Close find"
      >
        <X size={15} strokeWidth={1.9} />
      </button>
    </div>
  );
}

// Renders transcript text with case-insensitive match highlighting. The active
// match gets a distinct fill and is scrolled into view as the user steps.
function HighlightedText({
  text,
  query,
  matchIndex,
  asPre,
}: {
  text: string;
  query: string;
  matchIndex: number;
  asPre: boolean;
}) {
  const activeRef = useRef<HTMLElement>(null);

  // biome-ignore lint/correctness/useExhaustiveDependencies: re-scroll when the active match changes; effect reads only the ref
  useEffect(() => {
    activeRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [matchIndex, query, text]);

  const q = normalizeFindQuery(query);
  const Wrapper = asPre ? "pre" : "p";

  // One pass per (text, query); only the active-mark styling depends on matchIndex.
  const segments = useMemo(() => findSegments(text, q), [text, q]);

  if (!q) {
    return <Wrapper className={readerClass}>{text}</Wrapper>;
  }

  const nodes: ReactNode[] = segments.map((seg, i) => {
    if (seg.type === "text") return seg.value;
    const isActive = seg.index === matchIndex;
    return (
      <mark
        // biome-ignore lint/suspicious/noArrayIndexKey: segments are a static, non-reordering split of the rendered text
        key={`m${i}`}
        ref={isActive ? activeRef : undefined}
        className={cx(
          "rounded-[3px] px-0.5",
          isActive
            ? "bg-[var(--accent)] text-[var(--ink)]"
            : "bg-[color-mix(in_srgb,var(--accent)_30%,transparent)] text-[var(--paper)]",
        )}
      >
        {seg.value}
      </mark>
    );
  });

  return <Wrapper className={readerClass}>{nodes}</Wrapper>;
}

const readerClass =
  "m-0 max-w-[80ch] whitespace-pre-wrap break-words font-[family-name:var(--body)] text-[15px] leading-7 text-[var(--paper)]";

function MarkdownBody({
  text,
  query,
  matchIndex,
}: {
  text: string;
  query: string;
  matchIndex: number;
}) {
  // Find counts and highlights run on the same stripped text this tab renders.
  const stripped = useMemo(() => findTextForMarkdown(text), [text]);
  if (normalizeFindQuery(query)) {
    // Rendered Markdown has no highlight anchors, so an active find keeps the
    // plain stripped text (its match counts stay exact).
    return (
      <HighlightedText text={stripped} query={query} matchIndex={matchIndex} asPre={false} />
    );
  }
  return (
    <div className={markdownReaderClass}>
      <MarkdownRenderer source={text} />
    </div>
  );
}

const markdownReaderClass =
  "m-0 max-w-[80ch] break-words font-[family-name:var(--body)] text-[15px] leading-7 text-[var(--paper)]";

export function CarouselLayout({
  detail,
  activeSlide,
  onSlide,
}: {
  detail: LibraryDetail;
  activeSlide: number;
  onSlide: (i: number) => void;
}) {
  const slideTexts = splitSlideText(detail.text, (detail.images ?? []).length);

  return (
    <div className="grid min-h-0 flex-1 grid-cols-[168px_minmax(0,1fr)] gap-4">
      <div className={cx(panelClass, "flex flex-col gap-3 overflow-auto p-3")}>
        {(detail.images ?? []).map((src, index) => (
          <button
            type="button"
            key={src}
            className={cx(
              "relative overflow-hidden rounded-[8px] border bg-[var(--ink)] transition-colors focus:outline-none focus:ring-2 focus:ring-[var(--accent)]/35",
              index === activeSlide
                ? "border-[var(--accent)]"
                : "border-[var(--line)] hover:border-[var(--accent)]",
            )}
            onClick={() => onSlide(index)}
            title={`Slide ${index + 1}`}
          >
            <img
              className="aspect-[4/5] w-full object-cover"
              src={src}
              alt={`Slide ${index + 1}`}
            />
            <span className="absolute right-2 top-2 inline-flex h-6 min-w-6 items-center justify-center rounded-full bg-[color-mix(in_srgb,var(--ink)_82%,transparent)] px-2 font-[family-name:var(--mono)] text-xs text-[var(--paper)]">
              {index + 1}
            </span>
          </button>
        ))}
      </div>

      <div className={cx(panelClass, "min-h-0 overflow-auto p-5")}>
        <div className="mb-4 font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
          Slide {activeSlide + 1} of {(detail.images ?? []).length}
        </div>
        <p className={readerClass}>
          {slideTexts[activeSlide] ?? (
            <span className={mutedTextClass}>No OCR text for this slide.</span>
          )}
        </p>
      </div>
    </div>
  );
}

/** Thumbnail grid for an image job (#248). `files` are the saved image paths
 * from `slides` (the engine lists the slide_dir files there). */
export function ImageGrid({ files, onReveal }: { files: string[]; onReveal: () => void }) {
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-3">
      <div className="flex items-center justify-between gap-3">
        <span className="font-[family-name:var(--mono)] text-xs text-[var(--muted)]">
          {files.length === 1 ? "1 image" : `${files.length} images`}
        </span>
        <button type="button" className={buttonClass} onClick={onReveal}>
          Show in folder
        </button>
      </div>
      {files.length === 0 ? (
        <p className={mutedTextClass}>No image files saved for this post.</p>
      ) : (
        <div className="grid min-h-0 flex-1 grid-cols-[repeat(auto-fill,minmax(160px,1fr))] content-start gap-3 overflow-auto">
          {files.map((src, index) => (
            <img
              key={src}
              className="aspect-square w-full rounded-[8px] border border-[var(--line)] bg-[var(--ink)] object-cover"
              src={src}
              alt={`${index + 1} of ${files.length}`}
              loading="lazy"
            />
          ))}
        </div>
      )}
    </div>
  );
}

function splitSlideText(fullText: string, count: number): string[] {
  if (count === 0) return [];
  // The engine writes the carousel text as `[caption]` and `[slide N]`
  // marker blocks — map on the markers, not on blank lines, so a slide
  // whose OCR contains empty lines can't shift text onto the wrong slide
  // (and the caption can't land on slide 1 as if it were OCR).
  const result = Array.from({ length: count }, () => "");
  const markers = [...fullText.matchAll(/^\[(caption|slide)(?:\s+(\d+))?\]\s*$/gm)];
  let caption = "";
  for (let i = 0; i < markers.length; i++) {
    const m = markers[i];
    const start = (m.index ?? 0) + m[0].length;
    const end =
      i + 1 < markers.length ? (markers[i + 1].index ?? fullText.length) : fullText.length;
    const body = fullText.slice(start, end).trim();
    if (m[1] === "caption") {
      caption = body;
      continue;
    }
    const n = m[2] ? Number.parseInt(m[2], 10) : 0;
    if (n >= 1 && n <= count) {
      result[n - 1] = body;
    }
  }
  // The caption belongs to the post rather than any slide; prepend it to
  // the first pane so the reader doesn't lose it entirely.
  if (caption) result[0] = result[0] ? `${caption}

${result[0]}` : caption;
  return result;
}
