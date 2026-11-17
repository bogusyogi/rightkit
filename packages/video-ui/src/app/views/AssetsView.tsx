// Assets — docs/design/app-ui.md §3.7. Generated media library + the host app jobs.
import { Badge, Button, EmptyState, Field, IconButton, Input, PageShell, SegmentedControl, Select, useToast } from "@rightkit/react-shell/react";
import { useState } from "react";
import { assetUrl } from "../../lib/asset.js";
import type { GeneratedAsset, HostJob, PlaceMode } from "../lib/genright.js";
import { formatUsd, type CatalogKind } from "../lib/models.js";
import {
  useVideoAssetGeneration,
  useVideoAssetPlacement,
  useVideoAssets,
  videoAssetIsAudio as isAudio,
  videoAssetIsImage as isImage,
  videoAssetJobAction,
  videoAssetPath as fullPath,
  type VideoAssetFilter,
} from "../hooks/useVideoAssets.js";
import { MediaPlayer } from "@rightkit/media-ui";
import { ImageViewer } from "@rightkit/media-ui/image";
import { Drawer, ErrorPanel, Icon, ProgressRow, Thumbnail, fileSize } from "../ui/index.js";
import { hostAppName } from "../appName.js";

export function jobActionLabel(job: HostJob): "Cancel" | "Import now" | "Retry" | null {
  const action = videoAssetJobAction(job);
  return action ? ({ cancel: "Cancel", import: "Import now", retry: "Retry" } as const)[action] : null;
}

const JOB_TONE: Record<string, "neutral" | "ok" | "warn" | "danger" | "info"> = {
  queued: "info",
  running: "info",
  ready: "ok",
  done: "ok",
  failed: "danger",
  cancelled: "warn",
  import_failed: "warn",
  unknown: "warn",
};

const FILTERS: { value: VideoAssetFilter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "broll", label: "B-roll" },
  { value: "image", label: "Images" },
  { value: "voice", label: "Voice" },
  { value: "music", label: "Music" },
  { value: "sfx", label: "SFX" },
];

/** Library tile: real image/video frame for visuals, waveform-style tile for audio. */
function AssetTile({ dir, asset }: { dir: string; asset: GeneratedAsset }) {
  const p = fullPath(dir, asset);
  if (!asset.exists) {
    return (
      <div className="assets-tile">
        <Icon name="triangle-alert" width={22} height={22} />
      </div>
    );
  }
  if (isImage(asset)) return <Thumbnail src={assetUrl(p)} aspect="16 / 9" />;
  if (isAudio(asset)) {
    return (
      <div className="assets-tile assets-tile--audio">
        <Icon name={asset.kind === "voice" ? "mic" : "music"} width={22} height={22} />
        <audio src={assetUrl(p)} preload="metadata" />
      </div>
    );
  }
  return (
    <div className="assets-tile">
      <video src={`${assetUrl(p)}#t=0.1`} preload="metadata" muted playsInline />
    </div>
  );
}

// R11: spec menu labels — engine place modes underneath.
const PLACE_LABEL: Record<PlaceMode, string> = {
  append: "At end of timeline",
  broll: "As B-roll over…",
  narration: "As narration",
  music: "As music bed",
};

/** Place-on-timeline menu + the fields the chosen mode needs. */
function PlaceMenu({ dir, asset, onChanged, onPlaced, onError }: {
  dir: string;
  asset: GeneratedAsset;
  onChanged: (dir: string) => void | Promise<void>;
  onPlaced: () => void;
  onError: (error: string) => void;
}) {
  const { open, container, toggle, modes, mode, chooseMode, back, start, setStart, link, setLink, gain, setGain,
    cues, selectCue, isCueSelected, range, needsRange, needsLink, needsGain, canPlace, busy, placeSelected } =
    useVideoAssetPlacement({ dir, asset, onChanged, onPlaced, onError });
  return (
    <div className="assets-place" ref={container}>
      <Button size="sm" variant="secondary" disabled={busy} onClick={toggle}>
        Place on timeline
      </Button>
      {open && (
        <div className="assets-place__pop">
          {!mode ? (
            modes.map((m) => (
              <button key={m} type="button" disabled={busy} onClick={() => void chooseMode(m)}>
                {PLACE_LABEL[m]}
              </button>
            ))
          ) : (
            <div className="assets-place__form">
              <span className="cr-overline">{PLACE_LABEL[mode]}</span>
              {needsRange && (
                <div className="assets-cues" data-testid="broll-cues">
                  {cues === null && <span className="assets-cues__meta">Loading caption cues…</span>}
                  {cues?.length === 0 && <span className="assets-cues__meta">No caption cues on the timeline yet.</span>}
                  {cues?.map((c, i) => {
                    return (
                      <button
                        key={`${c.start_ms}-${i}`}
                        type="button"
                        data-testid="broll-cue"
                        data-index={i}
                        data-start-ms={c.start_ms}
                        data-end-ms={c.end_ms}
                        aria-pressed={isCueSelected(i)}
                        title="Click the first cue, then shift-click the last"
                        onClick={(e) => selectCue(i, e.shiftKey)}
                      >
                        <span className="assets-cues__time">
                          {(c.start_ms / 1000).toFixed(2)}–{(c.end_ms / 1000).toFixed(2)} s
                        </span>{" "}
                        {c.text}
                      </button>
                    );
                  })}
                  <span className="assets-cues__meta" data-testid="broll-range">
                    {range ? `B-roll ${range.start.toFixed(2)} s to ${range.end.toFixed(2)} s` : "Pick a start cue, then shift-click an end cue."}
                  </span>
                </div>
              )}
              {needsLink && (
                <label>
                  Start (s)
                  <Input value={start} onChange={(e) => setStart(e.target.value)} placeholder="auto" />
                </label>
              )}
              {needsLink && (
                <label>
                  Link to clip
                  <Input value={link} onChange={(e) => setLink(e.target.value)} placeholder="clip id" />
                </label>
              )}
              {needsGain && (
                <label>
                  Bed level (dB under)
                  <Input value={gain} onChange={(e) => setGain(e.target.value)} placeholder="24" />
                </label>
              )}
              <div className="assets-place__actions">
                <Button size="sm" variant="ghost" disabled={busy} onClick={back}>
                  Back
                </Button>
                <Button
                  size="sm"
                  variant="primary"
                  onClick={() => void placeSelected()}
                  disabled={!canPlace}
                  aria-busy={(busy) || undefined}
                >
                  {busy ? "Placing…" : "Place"}
                </Button>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

const GEN_KINDS: { value: CatalogKind; label: string }[] = [
  { value: "video", label: "Video" },
  { value: "image", label: "Image" },
  { value: "voice", label: "Voice" },
  { value: "music", label: "Music" },
  { value: "sfx", label: "SFX" },
  { value: "avatar", label: "Avatar" },
  { value: "upscale", label: "Upscale" },
  { value: "lipsync", label: "Lipsync" },
];
function GenerateDrawer({ dir, open, onClose, onChanged, onSucceeded, onError }: {
  dir: string;
  open: boolean;
  onClose: () => void;
  onChanged: (dir: string) => void | Promise<void>;
  onSucceeded: () => void;
  onError: (error: string) => void;
}) {
  const { kind, setKind, prompt, setPrompt, models, model, chooseModel, shownModels: shown, duration, setDuration,
    durationControl: dur, needsSource, busy, canSubmit, submit } =
    useVideoAssetGeneration({ dir, open, onChanged, onSucceeded, onError });
  const submitAsset = async () => {
    if (await submit()) onClose();
  };

  const kindLabel = GEN_KINDS.find((k) => k.value === kind)?.label.toLowerCase() ?? kind;
  const modelHelp =
    models === null
      ? "Loading models…"
      : models.length === 0
        ? `${hostAppName()} reported no models.`
        : shown.length === 0
          ? `${hostAppName()} lists no ${kindLabel} models.`
          : needsSource
            ? "Upscale and lipsync need a source asset; this picker cannot send one yet."
            : undefined;

  return (
    <Drawer
      title="Generate"
      open={open}
      onClose={onClose}
      footer={
        <Button
          variant="primary"
          onClick={() => void submitAsset()}
          disabled={!canSubmit}
          aria-busy={(busy) || undefined}
        >{!(busy) && <Icon name="sparkles" />}
          {busy ? "Generating…" : "Generate"}
        </Button>
      }
    >
      <Field label="Kind">{() => (<SegmentedControl value={kind} options={GEN_KINDS} onChange={setKind} label="Asset kind" />)}</Field>
      <Field label="Prompt">{(ids) => (<textarea {...ids}
          className="rk-input cr-textarea"
          placeholder="Describe the asset…"
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
        />)}</Field>
      <Field label="Model" note={modelHelp}>{(ids) => (<select {...ids}
          className="rk-input cr-select"
          value={model ? `${model.provider}/${model.model}` : ""}
          onChange={(e) => chooseModel(e.target.value)}
        >
          {shown.map((m) => (
            <option key={`${m.provider}/${m.model}`} value={`${m.provider}/${m.model}`}>
              {m.label} · {m.provider}/{m.model}
              {m.estUsd !== null ? ` · ${formatUsd(m.estUsd)}` : ""}
            </option>
          ))}
        </select>)}</Field>
      {model && (
        <Field label="Duration" note={dur.type === "range" ? `${dur.min}–${dur.max} s` : undefined}>{() => (dur.type === "set" ? (
            <Select
              label="Duration"
              value={duration === undefined ? String(dur.options[0]) : String(duration)}
              options={dur.options.map((d) => ({ value: String(d), label: `${d} s` }))}
              onChange={(value) => setDuration(Number(value))}
            />
          ) : (
            <Input
              type="number"
              min={dur.type === "range" ? dur.min : 1}
              max={dur.type === "range" ? dur.max : 600}
              step={dur.type === "range" ? 1 : 0.5}
              value={duration ?? dur.initial}
              onChange={(e) => setDuration(e.target.value === "" ? undefined : Number(e.target.value))}
            />
          ))}</Field>
      )}
    </Drawer>
  );
}

interface AssetsViewProps {
  dir?: string;
  onNewVideo?: () => void;
  captureSelection?: () => () => boolean;
  refreshChangedProject?: (dir: string) => void | Promise<void>;
}

const skipProjectRefresh = () => {};

export function AssetsView(props: AssetsViewProps) {
  return <ProjectAssetsView key={props.dir} {...props} />;
}

function ProjectAssetsView({ dir, captureSelection, refreshChangedProject = skipProjectRefresh }: AssetsViewProps) {
  const toast = useToast();
  const [genOpen, setGenOpen] = useState(false);
  const { host: gr, jobs, assets, filter, setFilter, shown, preview, setPreview, previewPath, previewPlaybackId, busy,
    error, recovered, locate, jobAction, relink, reveal, isActive } = useVideoAssets({
    dir,
    onChanged: refreshChangedProject,
    captureSelection,
    onLocated: () => toast(`${hostAppName()} path saved — checking…`, { kind: "success" }),
    onActionError: (error) => toast(error, { kind: "error", ttl: 0 }),
    onRecovered: (n) => toast(`Recovered ${n} interrupted job${n === 1 ? "" : "s"}`, { kind: "success" }),
  });
  const canNotify = captureSelection?.() ?? isActive;
  const notify = (message: string) => { if (canNotify()) toast(message, { kind: "success" }); };
  const notifyError = (error: string) => { if (canNotify()) toast(error, { kind: "error", ttl: 0 }); };

  return (
    <PageShell
      title="Assets"
      actions={
        <>
          <SegmentedControl value={filter} options={FILTERS} onChange={setFilter} label="Filter assets" />
          <Button variant="primary" disabled={!dir || gr.state !== "ready"} onClick={() => setGenOpen(true)}><Icon name="sparkles" />
            Generate…
          </Button>
        </>
      }
      maxWidth={0}
      subtitle={assets ? `${assets.length} generated asset${assets.length === 1 ? "" : "s"}` : undefined}
    >
      <div className="assets-body">
        {error && <ErrorPanel title="Assets failed" detail={error} />}

        {/* Reserved height: loading, empty and grid states swap here after load, and the
            the host app notice below must not move when they do. */}
        <div className="assets-main">
        {!dir ? (
          <EmptyState title="No project open" icon={<Icon name="image" width={28} height={28} />}>
            Open a project to see its generated assets.
          </EmptyState>
        ) : assets === null ? (
          <ProgressRow label="Loading assets" status="Loading" fraction={undefined} />
        ) : shown.length === 0 ? (
          <EmptyState
            title={assets.length === 0 ? "No assets yet" : "Nothing in this filter"}
            action={
              assets.length === 0 && gr.state === "ready" ? (
                <Button variant="primary" onClick={() => setGenOpen(true)}>
                  Generate…
                </Button>
              ) : undefined
            }
            icon={<Icon name="image" width={28} height={28} />}
          >
            {assets.length === 0
              ? "Generated b-roll, images, voice and music land here."
              : "No generated assets of this kind yet."}
          </EmptyState>
        ) : (
          <div className="assets-grid">
            {shown.map((a) => (
              <div key={a.assetId} className="assets-card">
                <button
                  type="button"
                  className="assets-card__preview"
                  aria-label={`Preview ${a.prompt ?? a.assetId}`}
                  disabled={!a.exists}
                  onClick={() => setPreview(a)}
                >
                  <AssetTile dir={dir} asset={a} />
                </button>
                <div className="assets-card__name cr-clamp-1" title={a.prompt ?? a.assetId}>
                  {a.prompt ?? a.assetId}
                </div>
                <div className="assets-card__row">
                  <Badge tone={a.state === "ready" ? "ok" : "warn"}>{a.kind ?? "media"}</Badge>
                  {a.sourceId && <Badge tone="accent">On timeline</Badge>}
                </div>
                <div className="assets-card__prov cr-muted">
                  {hostAppName()} · {a.model}
                  {a.actualUsd !== null ? ` · ${formatUsd(a.actualUsd)}` : ""}
                  {a.bytes ? ` · ${fileSize(a.bytes)}` : ""}
                </div>
                <div className="assets-card__actions">
                  {a.state === "missing" ? (
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() => void relink(a)}
                      disabled={busy !== null}
                      aria-busy={(busy === a.assetId) || undefined}
                    >
                      {busy === a.assetId ? "Working…" : a.recovery === "relink" ? "Relink" : "Retry import"}
                    </Button>
                  ) : (
                    a.exists && <PlaceMenu
                      dir={dir}
                      asset={a}
                      onChanged={refreshChangedProject}
                      onPlaced={() => notify("Placed on the timeline")}
                      onError={notifyError}
                    />
                  )}
                  {a.exists && (
                    <IconButton
                      label="Reveal"
                      onClick={() => void reveal(a)}
                    ><Icon name="folder-open" /></IconButton>
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
        </div>

        {/* Below the grid: these panels appear and change while the user works the cards above,
            so putting them above would push the Preview/Place/Reveal controls under the pointer. */}
        {gr.state !== "ready" && gr.state !== "checking" && (
          <ErrorPanel
            title={gr.state === "missing" ? `${hostAppName()} not found` : `${hostAppName()} unavailable`}
            detail={gr.malformedStatus ? `${hostAppName()} did not report ready.` : gr.detail}
            actions={
              <Button size="sm" variant="secondary" onClick={() => void locate(`Locate the ${hostAppName()} engine`)}>
                Locate…
              </Button>
            }
          />
        )}

        {jobs !== null && jobs.length > 0 && (
          <div className="assets-jobs">
            <span className="cr-overline">Generation jobs</span>
            {jobs.map((j) => (
              <div key={j.requestId} className="assets-job">
                <ProgressRow
                  label={j.prompt || `${j.kind} · ${j.provider}/${j.model}`}
                  status={j.state}
                  tone={JOB_TONE[j.state] ?? "neutral"}
                  fraction={j.state === "queued" || j.state === "running" ? undefined : null}
                  error={j.error?.message ?? undefined}
                  onRetry={j.state === "failed" && busy === null ? () => void jobAction(j) : undefined}
                />
                {recovered.has(j.requestId) && <Badge tone="accent">Recovered</Badge>}
                {(j.actualUsd ?? j.estUsd) !== null && (
                  <span className="cr-mono cr-muted assets-job__cost">
                    {formatUsd(j.actualUsd ?? j.estUsd ?? 0)}
                  </span>
                )}
                {jobActionLabel(j) && (
                  <Button
                    size="sm"
                    variant="ghost"
                    onClick={() => void jobAction(j)}
                    disabled={busy !== null}
                    aria-busy={(busy === j.requestId) || undefined}
                  >
                    {busy === j.requestId ? "Working…" : jobActionLabel(j)}
                  </Button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>

      {dir && (
        <GenerateDrawer
          dir={dir}
          open={genOpen}
          onClose={() => setGenOpen(false)}
          onChanged={refreshChangedProject}
          onSucceeded={() => notify("Generation finished and imported")}
          onError={notifyError}
        />
      )}
      {previewPath && preview && (
        <Drawer title={preview.prompt ?? preview.assetId} open onClose={() => setPreview(null)}>
          {isImage(preview) ? (
            <div className="assets-image-viewer">
              <ImageViewer path={previewPath} src={assetUrl(previewPath)} displayName={preview.prompt ?? preview.assetId} readOnly />
            </div>
          ) : (
            <MediaPlayer
              key={previewPlaybackId ?? "pending"}
              src={previewPath}
              playbackId={previewPlaybackId}
              kind={isAudio(preview) ? "audio" : "video"}
              title={preview.prompt ?? preview.assetId}
              autoPlay
            />
          )}
        </Drawer>
      )}
    </PageShell>
  );
}
