import { IconButton, SegmentedControl } from "@rightkit/react-shell/react";
import { useSegmentedControl } from "@rightkit/react-shell/headless";
import { useId } from "react";
import { baseName, formatSeconds, type ProjectShow } from "../lib/project.js";
import { Icon } from "../ui/index.js";
import { StagePlayer, useTimelinePreview } from "../components/Player.js";
import { Timeline } from "./Timeline.js";
import { TranscriptPanel } from "./TranscriptPanel.js";
import { ChangesPanel } from "./ChangesPanel.js";
import { Inspector } from "./Inspector.js";
import { CaptionInspector } from "./CaptionInspector.js";
import { useVideoTimelineEditor, type VideoTimelineTab } from "./useVideoTimelineEditor.js";

export type PreviewHost = ReturnType<typeof useTimelinePreview>;

export function TimelineEditorView({ show, previewHost, onRefresh, busy }: {
  show: ProjectShow;
  previewHost: PreviewHost | null;
  onRefresh: () => Promise<unknown>;
  busy: boolean;
}) {
  const {
    dir, revision, caps, model, modelError, empty, aspect,
    tab, setTab, mode, setMode, cues, captionsOn, setCaptionsOn,
    plans, plansError, changeCount, panelError, setPanelError, playheadS, setPlayheadS,
    selected, setSelected, fallback, timelineHeight, timelineFull, setTimelineFull,
    sideHidden, setSideHidden, editingBusy, seekState, session, committed, onSeekPreview,
    onPosition, seek, currentCue, dragTimeline,
  } = useVideoTimelineEditor({ show, previewHost, onRefresh, busy });
  const tabsId = useId();
  const sideTabs = useSegmentedControl<VideoTimelineTab>({
    value: tab,
    options: [{ value: "transcript" }, { value: "changes" }, { value: "inspect" }, { value: "captions" }],
    onChange: setTab,
    label: "Side panel tabs",
  });
  const captionSourceCount = show.transcripts?.length ?? 0;
  const captionsLabel = captionSourceCount ? `${captionSourceCount} source${captionSourceCount === 1 ? "" : "s"}` : null;
  if (!model || empty) {
    return (
      <div className="fe" style={{ display: "flex", alignItems: "center", justifyContent: "center" }}>
        <div style={{ textAlign: "center" }}>
          <h2 style={{ fontSize: 15, fontWeight: 600, margin: 0 }}>No finished edit yet</h2>
          {modelError ? (
            <p role="alert" className="cr-muted">{modelError}</p>
          ) : (
            <p className="cr-muted" style={{ fontSize: 13, marginTop: 4 }}>
              Run an auto-edit to build the first cut. Its clip plan, captions, and changes will land here.
            </p>
          )}
        </div>
      </div>
    );
  }

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
          <span className="rk-badge">{formatSeconds(model.durationS)}</span>
          <span className="rk-badge">{show.sources.length} source{show.sources.length === 1 ? "" : "s"}</span>
          {captionsLabel && <span className="rk-badge">Captions {captionsLabel}</span>}
          <span className="rk-badge">
            Revision {revision}
            {show.revision.total > revision + 1 ? ` / ${show.revision.total}` : ""}
          </span>
          {editingBusy && (
            <span role="status" className="cr-muted">Applying changes…</span>
          )}
        </div>
      </header>

      {/* Main row: stage col + side panel */}
      <div className="fe-main">
        <div className="fe-stage-col">
          <StagePlayer
            key={dir}
            dir={dir}
            revision={revision}
            seekRequest={seekState}
            captionsOn={captionsOn}
            caption={currentCue?.text ?? null}
            aspect={aspect}
            fallback={fallback}
            onToggleCaptions={setCaptionsOn}
            onPosition={onPosition}
          />
        </div>

        {!sideHidden && (
          <aside className="fe-side" aria-label="Details">
            <div className="fe-side__tabs" {...sideTabs.groupProps} role="tablist">
              {(
                [
                  ["transcript", "Transcript"],
                  ["changes", `Changes${changeCount ? ` ${changeCount}` : ""}`],
                  ["inspect", "Inspect"],
                  ["captions", "Captions"],
                ] as [VideoTimelineTab, string][]
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
                  onSeek={seek}
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
                  onSeek={seek}
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
                  onError={setPanelError}
                />
              )}
              {tab === "captions" && <CaptionInspector model={model} session={session}
                onCommitted={committed} onError={setPanelError} />}
            </div>
          </aside>
        )}
      </div>

      {panelError && (
        <p role="alert" className="cr-muted" style={{ margin: 0, padding: "0 var(--s4, 16px)", fontSize: 12 }}>
          {panelError}
        </p>
      )}

      {/* Timeline */}
      <div className="fe-tl" style={{ height: timelineFull ? "55%" : timelineHeight }}>
        <div
          className="fe-tl__resize"
          role="separator"
          aria-orientation="horizontal"
          aria-label="Resize timeline"
          onPointerDown={dragTimeline}
        />
        <div className="fe-tl__resizebtn">
          <IconButton
            label={timelineFull ? "Shrink timeline" : "Expand timeline"}
            onClick={() => setTimelineFull((v) => !v)}
          ><Icon name={timelineFull ? "chevron-down" : "chevron-up"} /></IconButton>
        </div>
        <Timeline
          model={model}
          session={session}
          caps={caps}
          busy={editingBusy}
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
