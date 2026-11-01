// Assets — docs/design/app-ui.md §3.7. Generated media library + the host app jobs.
import { Badge, Button, EmptyState, Field, IconButton, Input, PageShell, SegmentedControl, Select, useToast } from "@rightkit/react-shell/react";
import "./Assets.css";
import { useCallback, useEffect, useRef, useState } from "react";
import { engineCall, errorMessage, revealPath } from "../lib/engine";
import { type CaptionCue, captionsShow } from "../lib/media";
import {
  type GeneratedAsset,
  type HostInfo,
  type HostJob,
  type PlaceMode,
  genRightCallFailed,
  interpretAssetLibrary,
  interpretHost,
  interpretJobs,
  interpretRecover,
  placeModesFor,
} from "../lib/genright";
import { type ModelRow, durationControl, formatUsd, ofKind, parseCatalog, pickModel } from "../lib/models";
import { settingsSet } from "../lib/media";
import { assetUrl } from "../lib/asset";
import { pickFile } from "../lib/dialog";
import { MediaPlayer } from "@rightkit/media-ui";
import { ImageViewer } from "@rightkit/media-ui/image";
import { useMediaPlaybackId } from "../lib/mediaAdapters";
import { Drawer, ErrorPanel, Icon, ProgressRow, Thumbnail, fileSize } from "../ui";
import { hostAppName } from "../appName";

export function jobActionLabel(j: HostJob): "Cancel" | "Import now" | "Retry" | null {
  switch (j.state) {
    case "queued":
    case "running":
      return "Cancel";
    case "ready":
      return "Import now";
    case "failed":
      return "Retry";
    default:
      return null;
  }
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

type Filter = "all" | "broll" | "image" | "voice" | "music" | "sfx";
const FILTERS: { value: Filter; label: string }[] = [
  { value: "all", label: "All" },
  { value: "broll", label: "B-roll" },
  { value: "image", label: "Images" },
  { value: "voice", label: "Voice" },
  { value: "music", label: "Music" },
  { value: "sfx", label: "SFX" },
];

function filterMatch(kind: string | null, f: Filter): boolean {
  if (f === "all") return true;
  const k = (kind ?? "").toLowerCase();
  if (f === "image") return k === "image" || k === "still";
  if (f === "voice") return k === "voice" || k === "narration";
  if (f === "music") return k === "music" || k === "song";
  if (f === "sfx") return k === "sfx" || k === "sound";
  if (f === "broll") return k === "broll" || k === "video";
  return true;
}

const isAudio = (a: GeneratedAsset) => /^audio\//.test(a.mime) || /\.(wav|mp3|m4a|flac|aac|ogg)$/i.test(a.path);
const isImage = (a: GeneratedAsset) => /^image\//.test(a.mime) || /\.(png|jpe?g|webp)$/i.test(a.path);

const fullPath = (dir: string, a: GeneratedAsset) => (a.path.startsWith("/") ? a.path : `${dir}/${a.path}`);

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
function PlaceMenu({ dir, asset, onPlaced }: { dir: string; asset: GeneratedAsset; onPlaced: () => void }) {
  const toast = useToast();
  const modes = placeModesFor(asset.kind);
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<PlaceMode | null>(null);
  const [start, setStart] = useState("");
  const [link, setLink] = useState("");
  const [gain, setGain] = useState("");
  const [busy, setBusy] = useState(false);
  // B-roll range: caption cues of the timeline, and the anchor/last indices picked from them.
  const [cues, setCues] = useState<CaptionCue[] | null>(null);
  const [cueSel, setCueSel] = useState<{ a: number; b: number } | null>(null);
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (mode !== "broll") return;
    let live = true;
    setCues(null);
    setCueSel(null);
    captionsShow(dir)
      .then((r) => live && setCues(r.cues))
      .catch((e) => {
        if (!live) return;
        setCues([]);
        toast(errorMessage(e), { kind: "error", ttl: 0 });
      });
    return () => {
      live = false;
    };
    // toast is stable per provider; the cue list reloads only when the mode or project changes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode, dir]);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) {
        setOpen(false);
        setMode(null);
      }
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const num = (s: string) => {
    const n = Number(s);
    return s.trim() !== "" && Number.isFinite(n) ? n : undefined;
  };
  // The contiguous run of cues picked for B-roll, as timeline seconds (start of first, end of last).
  const cueRange = (): { start: number; end: number } | null => {
    if (!cues || !cueSel) return null;
    const lo = Math.min(cueSel.a, cueSel.b);
    const hi = Math.max(cueSel.a, cueSel.b);
    return { start: cues[lo].start_ms / 1000, end: cues[hi].end_ms / 1000 };
  };
  const place = (m: PlaceMode) => {
    const range = m === "broll" ? cueRange() : null;
    setBusy(true);
    engineCall("assets.place", {
      dir,
      asset_id: asset.assetId,
      mode: m,
      ...(range ? { start_s: range.start, end_s: range.end } : {}),
      ...(!range && num(start) !== undefined ? { start_s: num(start) } : {}),
      ...(link.trim() ? { link_clip: link.trim() } : {}),
      ...(num(gain) !== undefined ? { gain_db: num(gain) } : {}),
    })
      .then(() => {
        toast("Placed on the timeline", { kind: "success" });
        setOpen(false);
        setMode(null);
        onPlaced();
      })
      .catch((e) => toast(errorMessage(e), { kind: "error", ttl: 0 }))
      .finally(() => setBusy(false));
  };

  const needsRange = mode === "broll";
  const needsLink = mode === "narration";
  return (
    <div className="assets-place" ref={ref}>
      <Button size="sm" variant="secondary" onClick={() => setOpen((v) => !v)}>
        Place on timeline
      </Button>
      {open && (
        <div className="assets-place__pop">
          {!mode ? (
            modes.map((m) => (
              <button key={m} type="button" onClick={() => (m === "append" ? place(m) : setMode(m))}>
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
                    const lo = cueSel ? Math.min(cueSel.a, cueSel.b) : -1;
                    const hi = cueSel ? Math.max(cueSel.a, cueSel.b) : -1;
                    const on = i >= lo && i <= hi;
                    return (
                      <button
                        key={`${c.start_ms}-${i}`}
                        type="button"
                        data-testid="broll-cue"
                        data-index={i}
                        data-start-ms={c.start_ms}
                        data-end-ms={c.end_ms}
                        aria-pressed={on}
                        title="Click the first cue, then shift-click the last"
                        onClick={(e) =>
                          setCueSel((prev) => (e.shiftKey && prev ? { a: prev.a, b: i } : { a: i, b: i }))
                        }
                      >
                        <span className="assets-cues__time">
                          {(c.start_ms / 1000).toFixed(2)}–{(c.end_ms / 1000).toFixed(2)} s
                        </span>{" "}
                        {c.text}
                      </button>
                    );
                  })}
                  <span className="assets-cues__meta" data-testid="broll-range">
                    {(() => {
                      const r = cueRange();
                      return r ? `B-roll ${r.start.toFixed(2)} s to ${r.end.toFixed(2)} s` : "Pick a start cue, then shift-click an end cue.";
                    })()}
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
              {mode === "music" && (
                <label>
                  Bed level (dB under)
                  <Input value={gain} onChange={(e) => setGain(e.target.value)} placeholder="24" />
                </label>
              )}
              <div className="assets-place__actions">
                <Button size="sm" variant="ghost" onClick={() => setMode(null)}>
                  Back
                </Button>
                <Button
                  size="sm"
                  variant="primary"
                  onClick={() => place(mode)}
                  disabled={(needsRange && cueRange() === null) || (busy)}
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

/** The eight the host app media kinds; each value is the `kind` string `generate` takes. */
type GenKind = "image" | "video" | "voice" | "music" | "sfx" | "avatar" | "upscale" | "lipsync";
const GEN_KINDS: { value: GenKind; label: string }[] = [
  { value: "video", label: "Video" },
  { value: "image", label: "Image" },
  { value: "voice", label: "Voice" },
  { value: "music", label: "Music" },
  { value: "sfx", label: "SFX" },
  { value: "avatar", label: "Avatar" },
  { value: "upscale", label: "Upscale" },
  { value: "lipsync", label: "Lipsync" },
];
/** Transform kinds re-process an existing source; the engine request has no input-reference field yet. */
const NEEDS_SOURCE: ReadonlySet<GenKind> = new Set<GenKind>(["upscale", "lipsync"]);

function GenerateDrawer({ dir, open, onClose, onDone }: { dir: string; open: boolean; onClose: () => void; onDone: () => void }) {
  const toast = useToast();
  const [kind, setKind] = useState<GenKind>("video");
  const [prompt, setPrompt] = useState("");
  const [models, setModels] = useState<ModelRow[] | null>(null);
  const [model, setModel] = useState<ModelRow | null>(null);
  const [duration, setDuration] = useState<number | undefined>(undefined);
  const [busy, setBusy] = useState(false);
  const reqSeq = useRef(0);

  useEffect(() => {
    if (!open) return;
    engineCall("genright.models", {})
      .then((r) => setModels(parseCatalog(r).models))
      .catch(() => setModels([]));
  }, [open]);

  // Models are listed per kind from the host app own catalog; a kind with no rows has no model.
  useEffect(() => {
    const wanted = models ? pickModel({ models, structuredDurations: true }, kind) : undefined;
    setModel(wanted ?? null);
    setDuration(wanted ? durationControl(wanted).initial : undefined);
  }, [kind, models]);

  const dur = durationControl(model ?? undefined);
  const needsSource = NEEDS_SOURCE.has(kind);
  const submit = async () => {
    if (!model || !prompt.trim() || needsSource) return;
    setBusy(true);
    reqSeq.current += 1;
    const requestId = `ui-gen-${Date.now().toString(36)}-${reqSeq.current}`;
    try {
      await engineCall("generate", {
        dir,
        kind,
        input: {
          provider: model.provider,
          model: model.model,
          prompt: prompt.trim(),
          request_id: requestId,
          ...(duration !== undefined ? { duration_s: duration } : {}),
        },
      });
      toast("Generation finished and imported", { kind: "success" });
      setPrompt("");
      onClose();
      onDone();
    } catch (e) {
      toast(errorMessage(e), { kind: "error", ttl: 0 });
      onDone();
    } finally {
      setBusy(false);
    }
  };

  const shown = models ? ofKind({ models, structuredDurations: true }, kind) : [];
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
          onClick={() => void submit()}
          disabled={(!model || !prompt.trim() || needsSource) || (busy)}
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
          onChange={(e) => {
            const m = shown.find((x) => `${x.provider}/${x.model}` === e.target.value) ?? null;
            setModel(m);
            setDuration(m ? durationControl(m).initial : undefined);
          }}
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
              onChange={(e) => setDuration(Number(e.target.value))}
            />
          ))}</Field>
      )}
    </Drawer>
  );
}

export function AssetsView({ dir, onNewVideo }: { dir?: string; onNewVideo?: () => void }) {
  const toast = useToast();
  const [gr, setGr] = useState<HostInfo>({ state: "checking" });
  const [jobs, setJobs] = useState<HostJob[] | null>(null);
  const [assets, setAssets] = useState<GeneratedAsset[] | null>(null);
  const [filter, setFilter] = useState<Filter>("all");
  const [genOpen, setGenOpen] = useState(false);
  const [preview, setPreview] = useState<GeneratedAsset | null>(null);
  const previewPath = dir && preview && !isImage(preview) ? fullPath(dir, preview) : null;
  const previewPlaybackId = useMediaPlaybackId(previewPath);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [recovered, setRecovered] = useState<Set<string>>(new Set());
  void onNewVideo;

  const checkStatus = useCallback(() => {
    engineCall("genright.status", {})
      .then((r) => setGr(interpretHost(r)))
      .catch((e) => setGr(genRightCallFailed(errorMessage(e))));
  }, []);

  useEffect(() => {
    let cancelled = false;
    engineCall("genright.status", {})
      .then((r) => !cancelled && setGr(interpretHost(r)))
      .catch((e) => !cancelled && setGr(genRightCallFailed(errorMessage(e))));
    return () => {
      cancelled = true;
    };
  }, []);

  const refresh = useCallback(() => {
    if (!dir) return;
    engineCall("genright.jobs", { dir, live: gr.state === "ready" })
      .then((r) => setJobs(interpretJobs(r)))
      .catch((e) => setError(errorMessage(e)));
    engineCall("assets.list", { dir })
      .then((r) => setAssets(interpretAssetLibrary(r).assets))
      .catch((e) => setError(errorMessage(e)));
  }, [dir, gr.state]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  // Poll while jobs are in flight.
  const jobsActive = (jobs ?? []).some((j) => j.state === "queued" || j.state === "running");
  useEffect(() => {
    if (!jobsActive) return;
    const t = window.setInterval(refresh, 3000);
    return () => window.clearInterval(t);
  }, [jobsActive, refresh]);

  const locate = async () => {
    const file = await pickFile(`Locate the ${hostAppName()} engine`, []);
    if (!file) return;
    try {
      await settingsSet({ genright_engine_path: file });
      toast(`${hostAppName()} path saved — checking…`, { kind: "success" });
      checkStatus();
    } catch (e) {
      toast(errorMessage(e), { kind: "error", ttl: 0 });
    }
  };

  const jobAction = (j: HostJob) => {
    if (!dir) return;
    const label = jobActionLabel(j);
    if (!label) return;
    setBusy(j.requestId);
    const call =
      label === "Cancel"
        ? engineCall("genright.cancel", { dir, request_id: j.requestId })
        : engineCall("genright.recover", { dir, request_id: j.requestId, retry_failed: label === "Retry" });
    call
      .then(() => refresh())
      .catch((e) => setError(errorMessage(e)))
      .finally(() => setBusy(null));
  };

  const relink = (a: GeneratedAsset) => {
    if (!dir) return;
    setBusy(a.assetId);
    engineCall("assets.relink", { dir, asset_id: a.assetId })
      .then(() => refresh())
      .catch((e) => setError(errorMessage(e)))
      .finally(() => setBusy(null));
  };

  // Recover interrupted jobs once per project open; recovered request ids get
  // the `Recovered` chip in the jobs list.
  const recoveredOnce = useRef<string | null>(null);
  useEffect(() => {
    if (!dir || gr.state !== "ready" || recoveredOnce.current === dir) return;
    recoveredOnce.current = dir;
    engineCall("genright.recover", { dir })
      .then((r) => {
        const done = interpretRecover(r);
        setRecovered(new Set(done.map((d) => d.requestId)));
        const n = done.filter((d) => d.state === "done").length;
        if (n > 0) toast(`Recovered ${n} interrupted job${n === 1 ? "" : "s"}`, { kind: "success" });
        refresh();
      })
      .catch(() => undefined);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- once per dir
  }, [dir, gr.state]);

  const shown = (assets ?? []).filter((a) => filterMatch(a.kind, filter));

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
                      onClick={() => relink(a)}
                      disabled={(busy === a.assetId)}
                      aria-busy={(busy === a.assetId) || undefined}
                    >
                      {busy === a.assetId ? "Working…" : a.recovery === "relink" ? "Relink" : "Retry import"}
                    </Button>
                  ) : (
                    a.exists && <PlaceMenu dir={dir} asset={a} onPlaced={refresh} />
                  )}
                  {a.exists && (
                    <IconButton
                      label="Reveal"
                      onClick={() => void revealPath(fullPath(dir, a)).catch((e) => toast(errorMessage(e), { kind: "error", ttl: 0 }))}
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
            detail={gr.detail}
            actions={
              <Button size="sm" variant="secondary" onClick={() => void locate()}>
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
                  onRetry={j.state === "failed" ? () => jobAction(j) : undefined}
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
                    onClick={() => jobAction(j)}
                    disabled={(busy === j.requestId)}
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

      {dir && <GenerateDrawer dir={dir} open={genOpen} onClose={() => setGenOpen(false)} onDone={refresh} />}
      {dir && preview && (
        <Drawer title={preview.prompt ?? preview.assetId} open onClose={() => setPreview(null)}>
          {isImage(preview) ? (
            <div className="assets-image-viewer">
              <ImageViewer path={fullPath(dir, preview)} src={assetUrl(fullPath(dir, preview))} displayName={preview.prompt ?? preview.assetId} readOnly />
            </div>
          ) : (
            <MediaPlayer
              key={previewPlaybackId ?? "pending"}
              src={fullPath(dir, preview)}
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
