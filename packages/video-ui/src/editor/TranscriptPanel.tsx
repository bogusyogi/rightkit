/**
 * Transcript tab of the Finished edit side panel.
 *
 * Review mode: lines are the timeline-mapped caption cues (`captions.show`) —
 * stamp + text, current line highlighted, click seeks.
 *
 * Edit mode: word-level surface. Words come from the engine's validated
 * `transcript.show` (independent word-edge evidence — the locked invariant for
 * destructive word cuts); each word maps through the live clip ranges, so
 * removed words show struck-through. Selecting kept words + Delete
 * ripple-deletes them through the EditSession; selecting struck words +
 * Restore re-inserts the source span as a clip at the boundary where it was
 * cut. Both land as engine revisions — undoable, never local state.
 */
import { Button } from "@rightkit/react-shell/react";
import { useEffect, useMemo, useState } from "react";
import { errorMessage } from "../lib/engine.js";
import { transcriptShow, type CaptionCue, type Transcript } from "../lib/media.js";
import { baseName, formatSeconds, newClipId, secondsToTicks, type ProjectShow } from "../lib/project.js";

import type { EditSession } from "./adapter.js";
import { insertClip, rippleDeleteRange, type EditAction } from "./actions.js";
import { supports, unsupported, type CapabilityReport } from "./capabilities.js";
import { clipRanges, coveredOutputRanges, sourceToOutput, type ClipRange } from "./transcript.js";

type WordExt = {
  text: string;
  start_ms: number;
  end_ms: number;
  confidence?: number;
  end_source?: string;
};

type Selection = { source: string; anchor: number; focus: number } | null;

export function TranscriptPanel({
  show,
  session,
  caps,
  mode,
  cues,
  playheadS,
  onSeek,
  onCommitted,
  onError,
}: {
  show: ProjectShow;
  session: EditSession;
  caps: CapabilityReport | null;
  mode: "review" | "edit";
  /** Timeline-mapped caption cues (Review mode rows). */
  cues: CaptionCue[];
  /** Current playhead in seconds — drives the current-line highlight. */
  playheadS: number;
  /** Seek the timeline playhead (seconds). */
  onSeek: (timeS: number) => void;
  onCommitted: () => void;
  onError?: (message: string | null) => void;
}) {
  if (mode === "review") {
    return <CueList cues={cues} playheadS={playheadS} onSeek={onSeek} />;
  }
  return (
    <WordEditor
      show={show}
      session={session}
      caps={caps}
      onSeek={onSeek}
      onCommitted={onCommitted}
      onError={onError}
    />
  );
}

/** Review-mode transcript: one row per caption cue on the current timeline. */
function CueList({
  cues,
  playheadS,
  onSeek,
}: {
  cues: CaptionCue[];
  playheadS: number;
  onSeek: (timeS: number) => void;
}) {
  const current = cues.findIndex((c) => playheadS * 1000 >= c.start_ms && playheadS * 1000 < c.end_ms);
  if (cues.length === 0) {
    return <p className="cr-muted" style={{ margin: 0, fontSize: 12 }}>No captions on this edit yet.</p>;
  }
  return (
    <>
      {cues.map((c, i) => (
        <button
          key={i}
          type="button"
          className={`fe-cue${i === current ? " fe-cue--current" : ""}`}
          aria-current={i === current || undefined}
          onClick={() => onSeek(c.start_ms / 1000)}
        >
          <span className="fe-cue__stamp">{formatSeconds(c.start_ms / 1000)}</span>
          <span>{c.text}</span>
        </button>
      ))}
    </>
  );
}

function WordEditor({
  show,
  session,
  caps,
  onSeek,
  onCommitted,
  onError,
}: {
  show: ProjectShow;
  session: EditSession;
  caps: CapabilityReport | null;
  onSeek: (timeS: number) => void;
  onCommitted: () => void;
  onError?: (message: string | null) => void;
}) {
  const dir = show.project.dir;
  const transcripted = useMemo(
    () => new Set((show.transcripts ?? []).map((t) => t.source)),
    [show.transcripts],
  );
  const ranges = useMemo(() => clipRanges(show), [show]);
  const [transcripts, setTranscripts] = useState<Record<string, Transcript>>({});
  const [loadError, setLoadError] = useState<string | null>(null);
  const [sel, setSel] = useState<Selection>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (transcripted.size === 0) return;
    let cancelled = false;
    (async () => {
      try {
        const ts = await Promise.all([...transcripted].map((id) => transcriptShow(dir, id)));
        if (cancelled) return;
        const next: Record<string, Transcript> = {};
        [...transcripted].forEach((id, i) => {
          next[id] = ts[i]!.transcript;
        });
        setTranscripts(next);
        setLoadError(null);
      } catch (e) {
        if (!cancelled) setLoadError(errorMessage(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [dir, transcripted]);

  const selInfo = (() => {
    if (!sel) return null;
    const t = transcripts[sel.source];
    if (!t) return null;
    const lo = Math.min(sel.anchor, sel.focus);
    const hi = Math.max(sel.anchor, sel.focus);
    const words = t.words.slice(lo, hi + 1) as WordExt[];
    if (words.length === 0) return null;
    const startMs = words[0]!.start_ms;
    const endMs = words[words.length - 1]!.end_ms;
    const rs = ranges.get(sel.source) ?? [];
    const spans = coveredOutputRanges(rs, startMs, endMs);
    // A struck selection is one whose words are all unplaced.
    const allCut = words.every((w) => sourceToOutput(rs, w.start_ms) == null);
    return { lo, hi, words, startMs, endMs, spans, allCut, source: sel.source };
  })();

  const canDelete = supports("edit.ripple_delete", caps);

  const clickWord = (source: string, index: number, shift: boolean) => {
    setSel((prev) =>
      shift && prev && prev.source === source
        ? { source, anchor: prev.anchor, focus: index }
        : { source, anchor: index, focus: index },
    );
    const w = (transcripts[source]?.words ?? [])[index] as WordExt | undefined;
    if (!w) return;
    const out = sourceToOutput(ranges.get(source) ?? [], w.start_ms);
    if (out != null) onSeek(out);
  };

  const deleteSelection = async () => {
    if (!selInfo || selInfo.spans.length === 0 || !canDelete) return;
    setBusy(true);
    try {
      await session.apply(
        selInfo.spans.map((s) => rippleDeleteRange(s.startS, s.endS)),
        `Delete ${selInfo.words.length} word${selInfo.words.length === 1 ? "" : "s"}`,
      );
      setSel(null);
      onCommitted();
    } catch (e) {
      onError?.(errorMessage(e));
      onCommitted();
    } finally {
      setBusy(false);
    }
  };

  /**
   * Restore: re-insert the struck span's source range into the footage track
   * at the boundary where it was cut. The insert index is the position of the
   * first clip (of the same source) starting at/after the span, else after the
   * last clip ending at/before it — the point the ripple-delete collapsed.
   */
  const restoreSelection = async () => {
    if (!selInfo || !selInfo.allCut) return;
    const track = show.timeline.video_tracks[0];
    if (!track) return;
    const { source, startMs, endMs } = selInfo;
    const inT = secondsToTicks(startMs / 1000);
    const outT = secondsToTicks(endMs / 1000);
    const sameSource = track.clips.filter((c) => c.source === source);
    let index = track.clips.length;
    const after = sameSource.find((c) => c.source_in >= outT);
    if (after) {
      index = track.clips.indexOf(after);
    } else {
      const before = [...sameSource].reverse().find((c) => c.source_out <= inT);
      if (before) index = track.clips.indexOf(before) + 1;
    }
    setBusy(true);
    try {
      const action: EditAction = insertClip(track.id, index, {
        id: newClipId(),
        source,
        source_in: inT,
        source_out: outT,
      });
      await session.apply([action], `Restore ${selInfo.words.length} word${selInfo.words.length === 1 ? "" : "s"}`);
      setSel(null);
      onCommitted();
    } catch (e) {
      onError?.(errorMessage(e));
      onCommitted();
    } finally {
      setBusy(false);
    }
  };

  const entries = Object.entries(transcripts);
  const placed = (id: string) => (ranges.get(id)?.length ?? 0) > 0;

  return (
    <>
      {loadError && (
        <p role="alert" className="cr-muted" style={{ margin: 0, fontSize: 12 }}>
          {loadError}
        </p>
      )}
      {transcripted.size === 0 && (
        <p className="cr-muted" style={{ margin: 0, fontSize: 12 }}>
          No transcripts yet — transcribe a source to edit from its words.
        </p>
      )}
      {entries.length === 0 && transcripted.size > 0 && !loadError && (
        <p className="cr-muted" style={{ margin: 0, fontSize: 12 }}>Loading transcripts…</p>
      )}

      {entries.map(([source, t]) => {
        const name = show.sources.find((s) => s.info.id === source)?.name ?? baseName(source);
        const rs = ranges.get(source) ?? [];
        const isSel = (i: number) =>
          sel != null &&
          sel.source === source &&
          i >= Math.min(sel.anchor, sel.focus) &&
          i <= Math.max(sel.anchor, sel.focus);
        return (
          <div key={source}>
            <div className="cr-muted" style={{ fontSize: 11, marginBottom: 4 }} title={source}>
              {name}
              {!placed(source) && " (not on the timeline)"}
            </div>
            <p className="fe-words" style={{ margin: 0 }}>
              {(t.words as WordExt[]).map((w, i) => {
                const out = sourceToOutput(rs, w.start_ms);
                const approx = w.end_source && w.end_source !== "model";
                return (
                  <button
                    key={i}
                    type="button"
                    className={`fe-word${out == null ? " fe-word--cut" : ""}${isSel(i) ? " fe-word--sel" : ""}`}
                    title={`${formatSeconds(w.start_ms / 1000)}–${formatSeconds(w.end_ms / 1000)} source${
                      out != null ? ` · ${formatSeconds(out)} timeline` : " · cut"
                    }${approx ? " · word end approximate" : ""}`}
                    onClick={(e) => clickWord(source, i, e.shiftKey)}
                  >
                    {w.text}
                  </button>
                );
              })}
            </p>
          </div>
        );
      })}

      {selInfo && (
        <div style={{ display: "flex", flexWrap: "wrap", gap: "var(--s2, 8px)", alignItems: "center" }}>
          {selInfo.allCut ? (
            <Button
              size="sm"
              disabled={busy}
              onClick={() => void restoreSelection()}
            >
              Restore {selInfo.words.length} word{selInfo.words.length === 1 ? "" : "s"}
            </Button>
          ) : (
            <Button
              size="sm"
              disabled={busy || !canDelete || selInfo.spans.length === 0}
              title={!canDelete ? unsupported("edit.ripple_delete").reason : undefined}
              onClick={() => void deleteSelection()}
            >
              Delete {selInfo.words.length} word{selInfo.words.length === 1 ? "" : "s"}
            </Button>
          )}
          {selInfo.spans.length === 0 && !selInfo.allCut && (
            <span className="cr-muted" style={{ fontSize: 11 }}>Selected words are not on the timeline.</span>
          )}
        </div>
      )}
    </>
  );
}

export type { ClipRange };
