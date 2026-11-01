/**
 * Finished edit workspace (docs/design/app-ui.md §3.4).
 *
 * Layout: app-sidebar · [ headerbar | stage col (stage + transport) + side
 * panel (Transcript / Changes / Inspect tabs) | timeline ].
 *
 * Stage shows real compositor frames (StagePlayer over the Rust playback
 * session), falling back to the nearest `media.thumbnail` while loading or if
 * playback fails — never a black box when media exists.
 *
 * `project.show` is the single source of truth: EditSession mutations carry
 * `expect_cursor` and the host refreshes after every commit; the preview
 * session reopens on revision change.
 */
import { IconButton, SegmentedControl } from "@rightkit/react-shell/react";
import { useSegmentedControl } from "@rightkit/react-shell/headless";
import { Icon } from "../ui/index.js";
import { useCallback, useEffect, useId, useMemo, useState } from "react";
import { engineCall, errorMessage } from "../lib/engine.js";
import { assetUrl } from "../lib/asset.js";
import {
  editplanList,
  thumbnails,
  type CaptionCue,
  type StoredPlan,
} from "../lib/media.js";
import { baseName, formatSeconds, type ProjectShow } from "../lib/project.js";

import { StagePlayer, useTimelinePreview } from "../components/Player.js";

export type PreviewHost = ReturnType<typeof useTimelinePreview>;
import { EditSession, errorText } from "./adapter.js";
import { ChangesPanel } from "./ChangesPanel.js";
import { Inspector } from "./Inspector.js";
import { CaptionInspector } from "./CaptionInspector.js";
import { buildEditorModel, type EditorModel } from "./model.js";
import { clipRanges } from "./transcript.js";
import type { CapabilityReport } from "./capabilities.js";
import { Timeline, type PreviewSeek } from "./Timeline.js";
import { TranscriptPanel } from "./TranscriptPanel.js";

type Tab = "transcript" | "changes" | "inspect" | "captions";

export function TimelineEditorView({
  show,
  previewHost,
  onRefresh,
  busy,
}: {
  show: ProjectShow;
  previewHost: PreviewHost | null;
  onRefresh: () => Promise<unknown>;
  busy: boolean;
}) {
  const revision = show.revision.cursor;
  const dir = show.project.dir;

  // project.show advertises engine extensions; each gates a gesture family.
  const caps: CapabilityReport = useMemo(
    () => ({ extensions: show.extensions }),
    [show.extensions],
  );
  const [tab, setTab] = useState<Tab>("transcript");
  const [mode, setMode] = useState<"review" | "edit">("review");
  const [cues, setCues] = useState<CaptionCue[]>([]);
  const [captionsOn, setCaptionsOn] = useState(true);
  const [plans, setPlans] = useState<StoredPlan[]>([]);
  const [plansError, setPlansError] = useState<string | null>(null);
  // Side panel tabs: the shell's headless segmented-control behaviour (roving focus, Arrow/Home/End
  // keys that move and select) with the tablist/tab roles applied on top of its radio props.
  const tabsId = useId();
  const sideTabs = useSegmentedControl<Tab>({
    value: tab,
    options: [{ value: "transcript" }, { value: "changes" }, { value: "inspect" }, { value: "captions" }],
    onChange: setTab,
    label: "Side panel tabs",
  });
  const [panelError, setPanelError] = useState<string | null>(null);
  const [playheadS, setPlayheadS] = useState(0);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [fallback, setFallback] = useState<string | null>(null);
  const [tlH, setTlH] = useState<number>(192);
  const [tlFull, setTlFull] = useState(false);
  const [sideHidden, setSideHidden] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [seekState, setSeekState] = useState<{ ms: number; n: number } | null>(null);

  const model: EditorModel | null = useMemo(() => {
    try {
      return buildEditorModel(show);
    } catch (e) {
      setPanelError(errorMessage(e));
      return null;
    }
  }, [show]);

  // Revision-bound cues include saved corrections from project.show.
  const refreshCues = useCallback(async () => {
    if (!model) { setCues([]); return; }
    setCues(model.captions.map((cue) => ({
      start_ms: cue.start_frame * 1000 / model.fps,
      end_ms: (cue.start_frame + cue.frames) * 1000 / model.fps,
      text: cue.text,
      lines: [cue.text],
    })).sort((a, b) => a.start_ms - b.start_ms));
  }, [model]);

  useEffect(() => {
    void refreshCues();
  }, [refreshCues, revision]);

  // A failed read is retried, then shown: an empty Changes tab must mean "no plan",
  // never "the plan could not be read".
  useEffect(() => {
    let cancelled = false;
    setPlansError(null);
    void (async () => {
      for (let attempt = 0; ; attempt += 1) {
        try {
          const r = await editplanList(dir);
          if (!cancelled) setPlans(r.plans);
          return;
        } catch (e) {
          if (cancelled) return;
          if (attempt === 2) {
            setPlans([]);
            setPlansError(errorText(e));
            return;
          }
          await new Promise((done) => setTimeout(done, 600));
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [dir, revision]);

  // Stage fallback frame: nearest real thumbnail at the playhead, mapped
  // back through the clip ranges. Keeps the stage truthful while the
  // playback session opens or if it fails.
  const ranges = useMemo(() => clipRanges(show), [show]);
  const firstSource = model?.lanes
    .find((l) => l.kind === "video")
    ?.clips[0]?.ref;
  const firstSourceId =
    firstSource && firstSource.kind === "video" ? firstSource.clip.source : null;
  useEffect(() => {
    if (!firstSourceId) return;
    const rs = ranges.get(firstSourceId) ?? [];
    const t = playheadS;
    const r =
      rs.find((x) => t >= x.outOffS && t < x.outOffS + (x.outS - x.inS)) ??
      rs.reduce<(typeof rs)[number] | null>(
        (best, x) => (!best || Math.abs(x.outOffS - t) < Math.abs(best.outOffS - t) ? x : best),
        null,
      );
    if (!r) return;
    const at = r.inS + Math.max(0, t - r.outOffS);
    const timer = setTimeout(() => {
      void thumbnails(dir, { source: firstSourceId }, { at_ms: Math.round(at * 1000), width: 1280 })
        .then((res) => {
          const p = res.frames[0]?.path;
          if (p) setFallback(assetUrl(p));
        })
        .catch(() => undefined);
    }, 200);
    return () => clearTimeout(timer);
  }, [dir, firstSourceId, playheadS, ranges]);

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      await onRefresh();
      await refreshCues();
    } finally {
      setRefreshing(false);
    }
  }, [onRefresh, refreshCues]);

  const committed = useCallback(() => {
    void refresh();
  }, [refresh]);

  const session = useMemo(
    () =>
      new EditSession(dir, revision, caps, engineCall, () => {
        /* stale cursor: the host refreshes, which re-seats via resync */
        void refresh();
      }),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [dir],
  );

  // Re-seat the session's cursor whenever the show's revision changes.
  useEffect(() => {
    session.resync(revision);
  }, [session, revision]);

  const onSeekPreview = useCallback<PreviewSeek>((_cursor, t) => {
    setSeekState((prev) => ({ ms: Math.round(t * 1000), n: (prev?.n ?? 0) + 1 }));
    previewHost?.seek(revision, t);
  }, [previewHost]);

  const currentCue = cues.find((c) => playheadS * 1000 >= c.start_ms && playheadS * 1000 < c.end_ms);

  // ---- header data ---------------------------------------------------------
  const end = model?.durationS ?? 0;
  const sources = show.sources;
  const captionsLabel = show.transcripts?.length
    ? `${show.transcripts.length} source${show.transcripts.length === 1 ? "" : "s"}`
    : null;

  // ---- render ---------------------------------------------------------------
  if (!model || model.lanes.every((l) => l.clips.length === 0)) {
    return (
      <div className="fe" style={{ display: "flex", alignItems: "center", justifyContent: "center" }}>
        <div style={{ textAlign: "center" }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, margin: 0 }}>No finished edit yet</h2>
          <p className="cr-muted" style={{ fontSize: 13, marginTop: 4 }}>
            Run an auto-edit to build the first cut. Its clip plan, captions, and changes will land here.
          </p>
        </div>
      </div>
    );
  }

  const aspect = model.lanes.length > 0 ? aspectLabel(show) : null;

  const dragTl = (e: React.PointerEvent) => {
    const startY = e.clientY;
    const start = tlH;
    const move = (ev: PointerEvent) => {
      const next = Math.max(120, Math.min(480, start + (startY - ev.clientY)));
      setTlH(next);
      setTlFull(false);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  return (
    <div className="fe">
      {/* Header bar */}
      <header className="fe-header">
        <div className="fe-header__left">
          <IconButton label="Panel" pressed={!sideHidden} onClick={() => setSideHidden((v) => !v)}><Icon name="layout-list" /></IconButton>
          <strong className="fe-header__name">Finished edit</strong>
        </div>
        <div className="fe-header__mid">
          <SegmentedControl
            label="Mode"
            value={mode}
            onChange={(v) => setMode(v as "review" | "edit")}
            options={[
              { value: "review", label: "Review" },
              { value: "edit", label: "Edit" },
            ]}
          />
        </div>
        <div className="fe-header__right">
          <span className="rk-badge">{baseName(dir) || "Project"}</span>
          <span className="rk-badge">{timecodeish(end)}</span>
          <span className="rk-badge">{sources.length} source{sources.length === 1 ? "" : "s"}</span>
          {captionsLabel && <span className="rk-badge">Captions {captionsLabel}</span>}
          <span className="rk-badge">
            Revision {revision}
            {show.revision.total > revision + 1 ? ` / ${show.revision.total}` : ""}
          </span>
          {(session.busy || busy || refreshing) && (
            <span role="status" className="cr-muted">Applying changes…</span>
          )}
        </div>
      </header>

      {/* Main row: stage col + side panel */}
      <div className="fe-main">
        <div className="fe-stage-col">
          <StagePlayer
            dir={dir}
            revision={revision}
            seekRequest={seekState}
            captionsOn={captionsOn}
            caption={currentCue?.text ?? null}
            aspect={aspect}
            fallback={fallback}
            onToggleCaptions={setCaptionsOn}
            onPosition={(ms) => setPlayheadS(ms / 1000)}
          />
        </div>

        {!sideHidden && (
          <aside className="fe-side" aria-label="Details">
            <div className="fe-side__tabs" {...sideTabs.groupProps} role="tablist">
              {(
                [
                  ["transcript", "Transcript"],
                  ["changes", `Changes${plans.length ? ` ${plans.flatMap((p) => p.items ?? []).length}` : ""}`],
                  ["inspect", "Inspect"],
                  ["captions", "Captions"],
                ] as [Tab, string][]
              ).map(([id, label]) => (
                <button
                  key={id}
                  type="button"
                  {...sideTabs.getOptionProps(id)}
                  role="tab"
                  id={`${tabsId}-tab-${id}`}
                  aria-controls={`${tabsId}-panel`}
                  aria-selected={tab === id}
                  aria-checked={undefined}
                  className={`fe-side__tab${tab === id ? " fe-side__tab--active" : ""}`}
                >
                  {label}
                </button>
              ))}
            </div>
            <div className="fe-side__body" role="tabpanel" id={`${tabsId}-panel`} aria-labelledby={`${tabsId}-tab-${tab}`}>
              {tab === "transcript" && (
                <TranscriptPanel
                  show={show}
                  session={session}
                  caps={caps}
                  mode={mode}
                  cues={cues}
                  playheadS={playheadS}
                  onSeek={(t) => {
                    setPlayheadS(t);
                    onSeekPreview(revision, t);
                  }}
                  onCommitted={committed}
                  onError={setPanelError}
                />
              )}
              {tab === "changes" && (
                <ChangesPanel
                  dir={dir}
                  plans={plans}
                  loadError={plansError}
                  session={session}
                  onCommitted={committed}
                  onSeek={(t) => {
                    setPlayheadS(t);
                    onSeekPreview(revision, t);
                  }}
                />
              )}
              {tab === "inspect" && (
                <Inspector
                  model={model}
                  selection={selected}
                  session={session}
                  caps={caps}
                  dir={dir}
                  playheadS={playheadS}
                  onCommitted={committed}
                  onError={(m) => setPanelError(m)}
                />
              )}
              {tab === "captions" && <CaptionInspector model={model} session={session}
                onCommitted={committed} onError={setPanelError} />}
            </div>
          </aside>
        )}
      </div>

      {panelError && (
        <p role="alert" className="cr-muted" style={{ margin: 0, padding: "0 var(--s4)", fontSize: 12 }}>
          {panelError}
        </p>
      )}

      {/* Timeline */}
      <div className="fe-tl" style={{ height: tlFull ? "55%" : tlH }}>
        <div
          className="fe-tl__resize"
          role="separator"
          aria-orientation="horizontal"
          aria-label="Resize timeline"
          onPointerDown={dragTl}
        />
        <div className="fe-tl__resizebtn">
          <IconButton
            label={tlFull ? "Shrink timeline" : "Expand timeline"}
            onClick={() => setTlFull((v) => !v)}
          ><Icon name={tlFull ? "chevron-down" : "chevron-up"} /></IconButton>
        </div>
        <Timeline
          model={model}
          session={session}
          caps={caps}
          busy={busy || refreshing}
          mode={mode}
          cues={cues}
          dir={dir}
          playheadS={playheadS}
          onPlayhead={setPlayheadS}
          onCommitted={committed}
          onSeek={onSeekPreview}
          onSelect={setSelected}
        />
      </div>
    </div>
  );
}

function timecodeish(s: number) {
  return formatSeconds(s);
}

function aspectLabel(show: ProjectShow): string | null {
  const v = show.timeline.video_tracks.flatMap((t) => t.clips)[0];
  if (!v) return null;
  const src = show.sources.find((s) => s.info.id === v.source);
  const w = src?.info.video?.width;
  const h = src?.info.video?.height;
  if (!w || !h) return null;
  const r = w / h;
  if (Math.abs(r - 16 / 9) < 0.02) return "16:9";
  if (Math.abs(r - 9 / 16) < 0.02) return "9:16";
  if (Math.abs(r - 1) < 0.02) return "1:1";
  if (Math.abs(r - 4 / 3) < 0.02) return "4:3";
  return `${w}×${h}`;
}
