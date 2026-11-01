// Exports — docs/design/app-ui.md §3.5.
import { Badge, Button, Checkbox, ContextMenu, EmptyState, Field, IconButton, Input, PageShell, Popover, Select, useToast } from "@rightkit/react-shell/react";
import "./Exports.css";
import { type FormEvent, useCallback, useEffect, useRef, useState } from "react";
import { engineCall, errorMessage, onEngineEvent, revealPath } from "../lib/engine";
import {
  type QaReportRow,
  type QaResult,
  type RenderFile,
  type SourceRow,
  exportFcpxml,
  exportOtio,
  qaList,
  qaRun,
  framingApproval,
  framingLabel,
  packageBuild,
  type PackagePlatform,
  type ReframeSummary,
  reframeShow,
  rendersDelete,
  rendersList,
  settingsGet,
  sourceList,
  thumbnails,
} from "../lib/media";
import { assetUrl } from "../lib/asset";
import { projectShow, type ProjectShow } from "../lib/project";
import { MediaPlayer } from "@rightkit/media-ui";
import { isPlayable, mediaKind, useMediaPlaybackId } from "../lib/mediaAdapters";
import { Drawer, ErrorPanel, Icon, ProgressRow, Thumbnail, fileSize, timecode } from "../ui";
import type { ProgressState } from "../hooks/useProgress";
import { BatchDelivery } from "./BatchDelivery";
import { BundleExport } from "./BundleExport";

type Preset = "youtube" | "reels" | "tiktok" | "square";

type Format = {
  preset: Preset;
  title: string;
  sub: string;
  aspect: string;
  vertical: boolean;
  supported: boolean;
};

/** Recipe aspect chips (Edit recipe → Format) → export presets. */
const RECIPE_PRESET: Record<string, string> = { "16:9": "youtube", "9:16": "reels", "1:1": "square" };

const FORMATS: Format[] = [
  { preset: "youtube", title: "YouTube 16:9", sub: "1080p", aspect: "16 / 9", vertical: false, supported: true },
  {
    preset: "reels",
    title: "Shorts / Reels / TikTok",
    sub: "9:16",
    aspect: "9 / 16",
    vertical: true,
    supported: true,
  },
  { preset: "square", title: "Square", sub: "1:1", aspect: "1 / 1", vertical: true, supported: true },
];

const PRESET_ASPECT: Record<string, string> = { youtube: "16 / 9", reels: "9 / 16", tiktok: "9 / 16", square: "1 / 1" };

type QueueJob = {
  job: string;
  preset: Preset;
  label: string;
  status: "queued" | "running" | "done" | "failed" | "cancelled";
  fraction?: number;
  error?: string;
};

/** Cached JPEG from `media.thumbnail` for a render (or a source range). */
function useThumb(dir: string, target: { render: string } | { source: string }, atMs?: number) {
  const [src, setSrc] = useState<string | undefined>();
  const key = "render" in target ? `r:${target.render}` : `s:${target.source}:${atMs ?? ""}`;
  useEffect(() => {
    let cancelled = false;
    setSrc(undefined);
    thumbnails(dir, target, atMs != null ? { at_ms: atMs, width: 480 } : { width: 480 })
      .then((r) => !cancelled && setSrc(r.frames[0] ? assetUrl(r.frames[0].path) : undefined))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- key captures target
  }, [dir, key]);
  return src;
}

function RenderThumb({ dir, render, aspect }: { dir: string; render: string; aspect: string }) {
  const src = useThumb(dir, { render });
  return <Thumbnail src={src} aspect={aspect} />;
}

/** ⋯ menu per export card. */
function CardMenu({
  onCopy,
  onDelete,
  onImportRetention,
}: {
  onCopy: () => void;
  onDelete: () => void;
  onImportRetention?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement | null>(null);
  return (
    <div className="exports-menu">
      <IconButton ref={ref} label="More actions" onClick={() => setOpen((v) => !v)}><Icon name="more-horizontal" /></IconButton>
      {open && ref.current && (
        <ContextMenu
          anchor={ref.current}
          placement="top-end"
          label="Export actions"
          width={160}
          className="exports-menu__pop"
          itemAs="button"
          keyboard="escape-only"
          autoFocus={false}
          onClose={() => setOpen(false)}
          items={[
            { id: "copy", label: "Copy path", icon: <Icon name="copy" />, run: () => { setOpen(false); onCopy(); } },
            { id: "retention", label: "Import retention…", icon: <Icon name="chart-line" />, disabled: !onImportRetention, run: () => { setOpen(false); onImportRetention?.(); } },
            { id: "delete", label: "Delete export", icon: <Icon name="trash-2" />, danger: true, run: () => { setOpen(false); onDelete(); } },
          ]}
          renderItem={(item) => (
            <span title={item.id === "retention" && item.disabled ? "Retention screen routing pending" : undefined}>
              {item.icon} {item.label}
            </span>
          )}
        />
      )}
    </div>
  );
}

/** Key for a render's QA result in `qaByHash`: its output hash when recorded, else its file name.
 *  Used by both the store (`runQa`) and the lookup (the export card). */
const qaKey = (r: RenderFile) => r.record?.output_blake3 ?? r.name;

/** QA chip + issues popover (§3.5). */
function QaChip({ report, running, onRun }: { report?: QaReportRow; running: boolean; onRun: () => void }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLButtonElement | null>(null);
  if (!report) {
    return (
      <Button variant="ghost" size="sm" className="cr-chip-btn" onClick={onRun} title={"Run QA checks on this export"}><Badge tone="neutral">
        {running ? "Checking…" : "Run QA"}
      </Badge></Button>
    );
  }
  const ok = report.status === "pass" && report.issues === 0;
  const failed = (report.checks ?? []).filter((c) => c.status !== "pass");
  return (
    <div className="exports-menu">
      <Button variant="ghost" size="sm" ref={ref} className="cr-chip-btn" onClick={() => setOpen((v) => !v)}><Badge tone={ok ? "ok" : "warn"}>
        {ok ? "QA passed" : `${report.issues} issues`}
      </Badge></Button>
      {open && (
        <Popover anchor={ref.current} placement="top-start" width={280} label="Export QA checks" className="exports-qa__pop" onClose={() => setOpen(false)}>
          {failed.length === 0 ? (
            <p className="cr-muted">All checks passed.</p>
          ) : (
            <ul>
              {failed.map((c, i) => (
                <li key={c.id ?? i}>
                  <span className="cr-mono cr-muted">{c.id}</span>
                  {c.detail ? ` — ${c.detail}` : ""}
                </li>
              ))}
            </ul>
          )}
          <Button size="sm" variant="ghost" onClick={onRun} disabled={(running)} aria-busy={(running) || undefined}>
            {running ? "Running QA…" : "Re-run QA"}
          </Button>
        </Popover>
      )}
    </div>
  );
}

const PACKAGE_PLATFORMS: { value: PackagePlatform; label: string }[] = [
  { value: "youtube", label: "YouTube" },
  { value: "shorts", label: "YouTube Shorts" },
  { value: "reels", label: "Reels" },
  { value: "tiktok", label: "TikTok" },
];

/** Comma-separated tag input → the array `package.build` takes. */
const splitTags = (text: string) =>
  text
    .split(",")
    .map((t) => t.trim())
    .filter(Boolean);

/** Inline `package.build` form for one export: platform, title and optional copy. */
function PackageForm({ dir, render, onClose }: { dir: string; render: string; onClose: () => void }) {
  const [platform, setPlatform] = useState<PackagePlatform>("youtube");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [tags, setTags] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [folder, setFolder] = useState<string | null>(null);

  const submit = (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    if (!title.trim()) return;
    setBusy(true);
    setError(null);
    setFolder(null);
    void packageBuild(dir, {
      platform,
      render,
      title: title.trim(),
      description: description.trim(),
      tags: splitTags(tags),
    })
      // The reply names the manifest inside the package folder; the folder is its parent.
      .then((res) => setFolder(res.manifest.replace(/[\\/][^\\/]+$/, "")))
      .catch((e) => {
        const msg = errorMessage(e);
        // Drop only the engine's `qa_failed:` / `qa_missing:` code and the whitespace after it.
        const detail = msg.replace(/^qa_(?:failed|missing):\s*/, "");
        setError(
          msg.startsWith("qa_failed:")
            ? `This render has not passed QA — ${detail}`
            : msg.startsWith("qa_missing:")
              ? `This render has not been checked yet; run QA first — ${detail}`
              : msg,
        );
      })
      .finally(() => setBusy(false));
  };

  return (
    <form className="exports-package" onSubmit={submit}>
      <div className="exports-package__head">
        <span className="cr-overline">Package</span>
        <IconButton label="Close package form" onClick={onClose}><Icon name="x" /></IconButton>
      </div>
      <Field label="Platform">{() => (<Select<PackagePlatform>
          label="Platform"
          value={platform}
          onChange={setPlatform}
          options={PACKAGE_PLATFORMS}
        />)}</Field>
      <Field label="Title">{(ids) => (<Input {...ids} value={title} onChange={(e) => setTitle(e.target.value)} required />)}</Field>
      <Field label="Description">{(ids) => (<Input {...ids} value={description} onChange={(e) => setDescription(e.target.value)} />)}</Field>
      <Field label="Tags" note={"Separate with commas"}>{(ids) => (<Input {...ids} value={tags} onChange={(e) => setTags(e.target.value)} />)}</Field>
      <div className="exports-package__actions">
        <Button type="submit" variant="primary" size="sm" disabled={(!title.trim()) || (busy)} aria-busy={(busy) || undefined}>
          {busy ? "Packaging…" : "Build package"}
        </Button>
        {folder && (
          <Button size="sm" variant="ghost" onClick={() => void revealPath(folder)}>
            Reveal package
          </Button>
        )}
      </div>
      {folder && (
        <p role="status" className="exports-package__result cr-muted">
          Packaged to <span className="cr-mono">{folder}</span>
        </p>
      )}
      {error && (
        <p role="alert" className="exports-package__error">
          {error}
        </p>
      )}
    </form>
  );
}

export function ExportsView({
  dir,
  progress,
  rendering,
  onRender,
  onChanged,
  onImportRetention,
}: {
  dir: string;
  progress?: ProgressState;
  rendering?: boolean;
  outputPath?: string | null;
  onRender?: (preset: Preset) => void;
  onChanged: () => Promise<void>;
  onImportRetention?: (render: string) => void;
}) {
  const toast = useToast();
  const [renders, setRenders] = useState<RenderFile[] | null>(null);
  const [qaByHash, setQaByHash] = useState<Record<string, QaReportRow>>({});
  // QA results this session obtained through "Run QA"; the engine's reports win on lookup.
  const [qaRan, setQaRan] = useState<Record<string, QaReportRow>>({});
  const [sources, setSources] = useState<SourceRow[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<string | null>(null);
  const previewPlaybackId = useMediaPlaybackId(preview);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set(["youtube"]));
  const [interchange, setInterchange] = useState(false);
  const [framing, setFraming] = useState<Map<string, ReframeSummary | undefined>>(new Map());
  const [autoFraming, setAutoFraming] = useState(false);
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [queue, setQueue] = useState<QueueJob[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  // Name of the export whose package form is open.
  const [packaging, setPackaging] = useState<string | null>(null);
  const seq = useRef(0);
  const tags = useRef<Set<string>>(new Set());

  const refresh = useCallback(async () => {
    const [r, s, q, settings, project] = await Promise.all([
      rendersList(dir),
      sourceList(dir),
      qaList(dir).catch(() => ({ reports: [] as QaReportRow[] })),
      settingsGet(dir),
      projectShow(dir),
    ]);
    setShow(project);
    setAutoFraming(framingApproval(settings) === "auto");
    setRenders(r.renders);
    setSources(s.sources);
    setQaByHash(Object.fromEntries(q.reports.map((x) => [x.render_hash, x])));
    // Keep session results only for renders still in the list (a deleted render's result goes).
    const live = new Set(r.renders.flatMap((x) => [x.name, x.record?.output_blake3].filter((k): k is string => !!k)));
    setQaRan((m) => Object.fromEntries(Object.entries(m).filter(([k]) => live.has(k))));
  }, [dir]);

  useEffect(() => {
    refresh().catch((e) => setError(errorMessage(e)));
  }, [refresh]);
  useEffect(() => {
    if (progress?.done) void refresh().catch(() => undefined);
  }, [progress?.done, refresh]);

  // Track render progress events for this view's own render calls.
  useEffect(() => {
    let off: (() => void) | undefined;
    let dead = false;
    void onEngineEvent((frame) => {
      if (!frame.tag || !tags.current.has(frame.tag)) return;
      const ev = frame.event;
      if (ev.event === "progress" && typeof ev.job === "string") {
        const frac =
          typeof ev.out_time_ms === "number" && typeof ev.total_ms === "number" && ev.total_ms > 0
            ? ev.out_time_ms / ev.total_ms
            : undefined;
        setQueue((qs) => qs.map((j) => (j.job === ev.job ? { ...j, fraction: frac } : j)));
      }
    }).then((fn) => {
      if (dead) fn();
      else off = fn;
    });
    return () => {
      dead = true;
      off?.();
    };
  }, []);

  // Framing status per source, loaded when the drawer opens (vertical/square).
  useEffect(() => {
    if (!drawerOpen) return;
    let cancelled = false;
    void Promise.all(
      sources.flatMap((s) => ["9:16", "1:1"].map(async (aspect) => {
        const key = `${s.id}:${aspect}`;
        try {
          return [key, await reframeShow(dir, s.id, aspect)] as const;
        } catch {
          return [key, undefined] as const;
        }
      })),
    ).then((rows) => !cancelled && setFraming(new Map(rows)));
    return () => {
      cancelled = true;
    };
  }, [drawerOpen, dir, sources]);

  const mp4s = (renders ?? []).filter((r) => r.name.endsWith(".mp4"));
  const docs = (renders ?? []).filter((r) => /\.(srt|vtt|fcpxml|otio|json)$/.test(r.name));

  const runJob = async (j: QueueJob) => {
    const tag = `exports-${j.job}`;
    tags.current.add(tag);
    setQueue((qs) => qs.map((x) => (x.job === j.job ? { ...x, status: "running" } : x)));
    try {
      await engineCall("render", { dir, preset: j.preset, job: j.job }, tag);
      setQueue((qs) => qs.map((x) => (x.job === j.job ? { ...x, status: "done", fraction: 1 } : x)));
    } catch (e) {
      const code = (e as { code?: string })?.code;
      const cancelled = code === "cancelled" || code === "render_cancelled";
      setQueue((qs) =>
        qs.map((x) =>
          x.job === j.job
            ? { ...x, status: cancelled ? "cancelled" : "failed", error: cancelled ? undefined : errorMessage(e) }
            : x,
        ),
      );
    } finally {
      tags.current.delete(tag);
    }
  };

  // The drawer opens on the formats the recipe asks for; the user can still change them.
  const openDrawer = () => {
    setDrawerOpen(true);
    settingsGet(dir)
      .then((st) => {
        setAutoFraming(framingApproval(st) === "auto");
        const formats = (st?.project?.recipe as { formats?: string[] } | undefined)?.formats ?? [];
        const presets = formats.map((f) => RECIPE_PRESET[f]).filter(Boolean);
        if (presets.length) setPicked(new Set(presets));
      })
      .catch(() => undefined);
  };

  const startExport = async () => {
    const formats = FORMATS.filter((f) => f.supported && picked.has(f.preset));
    if (formats.length === 0 && !interchange) return;
    setDrawerOpen(false);
    const jobs: QueueJob[] = formats.map((f) => {
      seq.current += 1;
      return { job: `ui-x${seq.current}`, preset: f.preset as Preset, label: f.title, status: "queued" };
    });
    setQueue((qs) => [...qs.filter((j) => j.status === "queued" || j.status === "running"), ...jobs]);
    for (const j of jobs) {
      // Sequential: renders share the timeline and media workers.
      // eslint-disable-next-line no-await-in-loop
      await runJob(j);
    }
    if (interchange) {
      try {
        await exportFcpxml(dir);
        await exportOtio(dir);
      } catch (e) {
        toast(`Interchange export failed: ${errorMessage(e)}`, { kind: "error", ttl: 0 });
      }
    }
    await refresh();
    await onChanged();
  };

  const cancelJob = (job: string) => {
    void engineCall("render.cancel", { job }).catch((e) => toast(errorMessage(e), { kind: "error", ttl: 0 }));
    setQueue((qs) => qs.map((x) => (x.job === job && x.status === "queued" ? { ...x, status: "cancelled" } : x)));
  };

  const runQa = (r: RenderFile) => {
    const tag = `qa-${r.name}`;
    setBusy(tag);
    void qaRun(dir, r.path)
      .then((res: QaResult) => {
        const checks = res.report?.checks ?? [];
        const issues = checks.filter((c) => c.status !== "pass").length;
        const key = qaKey(r);
        const row: QaReportRow = { render_hash: key, status: res.status, issues, checks };
        // The run's receipt carries the output hash the engine keys its report by (absent on a dry run).
        const hash = (res.receipt as { render_blake3?: string } | undefined)?.render_blake3;
        setQaRan((m) => ({ ...m, [key]: row, ...(hash ? { [hash]: { ...row, render_hash: hash } } : {}) }));
      })
      .catch((e) => toast(errorMessage(e), { kind: "error", ttl: 0 }))
      .finally(() => setBusy(null));
  };

  const del = (r: RenderFile) => {
    setBusy(`del-${r.name}`);
    void rendersDelete(dir, r.name)
      .then(async () => {
        toast(`Deleted ${r.name}`, { kind: "success" });
        await refresh();
      })
      .catch((e) => toast(errorMessage(e), { kind: "error", ttl: 0 }))
      .finally(() => setBusy(null));
  };

  const activeQueue = queue.filter((j) => j.status === "queued" || j.status === "running");
  const finishedQueue = queue.filter((j) => j.status === "failed" || j.status === "cancelled");
  void onRender; // shell-owned render path kept for signature compatibility

  return (
    <PageShell
      title="Exports"
      actions={
        <Button variant="primary" onClick={openDrawer} disabled={sources.length === 0}><Icon name="download" />
          Export…
        </Button>
      }
      maxWidth={0}
      subtitle={renders
          ? `${mp4s.length} export${mp4s.length === 1 ? "" : "s"}`
          : undefined}
    >
      <div className="exports-body">
        {error && <ErrorPanel title="Exports failed to load" detail={error} />}

        {(activeQueue.length > 0 || finishedQueue.length > 0 || rendering) && (
          <div className="exports-queue">
            <span className="cr-overline">Queue</span>
            {queue.map((j) => (
              <div key={j.job} className="exports-queue__row">
                <ProgressRow
                  label={`Render ${j.label}`}
                  status={
                    j.status === "running"
                      ? "Rendering"
                      : j.status === "queued"
                        ? "Queued"
                        : j.status === "done"
                          ? "Done"
                          : j.status === "cancelled"
                            ? "Cancelled"
                            : "Failed"
                  }
                  tone={j.status === "failed" ? "danger" : j.status === "done" ? "ok" : "info"}
                  fraction={j.status === "done" ? 1 : j.status === "failed" || j.status === "cancelled" ? null : j.fraction}
                  error={j.error}
                />
                {(j.status === "queued" || j.status === "running") && (
                  <Button size="sm" variant="ghost" onClick={() => cancelJob(j.job)}>
                    Cancel
                  </Button>
                )}
                {(j.status === "failed" || j.status === "cancelled" || j.status === "done") && (
                  <IconButton
                    label="Dismiss"
                    onClick={() => setQueue((qs) => qs.filter((x) => x.job !== j.job))}
                  ><Icon name="x" /></IconButton>
                )}
              </div>
            ))}
          </div>
        )}

        {preview && (
          <div className="exports-preview">
            <div className="exports-preview__head">
              <span className="cr-overline">Playing</span>
              <IconButton label="Close preview" onClick={() => setPreview(null)}><Icon name="x" /></IconButton>
            </div>
            <MediaPlayer
              key={previewPlaybackId ?? "pending"}
              src={preview}
              playbackId={previewPlaybackId}
              kind={mediaKind(preview)}
              title={preview.split(/[\\/]/).pop()}
              autoPlay
            />
          </div>
        )}

        {renders === null ? (
          <ProgressRow label="Loading exports" status="Loading" fraction={undefined} />
        ) : mp4s.length === 0 && docs.length === 0 ? (
          <EmptyState
            title="No exports yet"
            action={<Button variant="primary" onClick={openDrawer}>Export…</Button>}
            icon={<Icon name="download" width={28} height={28} />}
          >
            Render your finished edit to share it.
          </EmptyState>
        ) : (
          <div className="exports-grid">
            {mp4s.map((r) => {
              const rec = r.record;
              const aspect = PRESET_ASPECT[rec?.preset ?? ""] ?? "16 / 9";
              const qa = qaByHash[qaKey(r)] ?? qaRan[qaKey(r)];
              const lufs = rec?.loudness?.after?.input_i ?? rec?.loudness?.measured?.input_i;
              const meta = [
                rec?.duration_ms != null ? timecode(rec.duration_ms) : null,
                rec?.preset === "reels" || rec?.preset === "tiktok" ? "9:16" : "16:9",
                fileSize(r.bytes),
                rec?.origin?.timeline_cursor != null ? `rev ${rec.origin.timeline_cursor}` : null,
              ]
                .filter(Boolean)
                .join(" · ");
              return (
                <div key={r.path} className="exports-card">
                  <button
                    type="button"
                    className="exports-card__thumb"
                    aria-label={`Play ${r.name}`}
                    onClick={() => isPlayable(r.path) && setPreview(r.path)}
                  >
                    <RenderThumb dir={dir} render={r.name} aspect={aspect} />
                  </button>
                  <div className="exports-card__name cr-clamp-1" title={r.name}>
                    {r.name}
                  </div>
                  <div className="exports-card__meta cr-muted">{meta}</div>
                  <div className="exports-card__row">
                    <QaChip report={qa} running={busy === `qa-${r.name}`} onRun={() => runQa(r)} />
                    <Button
                      size="sm"
                      variant="ghost"
                      aria-label={`Package ${r.name}`}
                      aria-expanded={packaging === r.name}
                      onClick={() => setPackaging((n) => (n === r.name ? null : r.name))}
                    >
                      Package
                    </Button>
                    {lufs != null && Number.isFinite(lufs) && (
                      <span className="exports-lufs">{lufs.toFixed(1)} LUFS</span>
                    )}
                    <span className="exports-card__spacer" />
                    <Button size="sm" variant="ghost" onClick={() => void revealPath(r.path)}>
                      Reveal
                    </Button>
                    <CardMenu
                      onCopy={() => void navigator.clipboard.writeText(r.path).then(() => toast("Path copied", { kind: "success" }))}
                      onDelete={() => del(r)}
                      onImportRetention={
                        onImportRetention ? () => onImportRetention(r.name) : undefined
                      }
                    />
                  </div>
                  {packaging === r.name && (
                    <PackageForm dir={dir} render={r.name} onClose={() => setPackaging(null)} />
                  )}
                </div>
              );
            })}
          </div>
        )}

        {docs.length > 0 && (
          <div className="exports-docs">
            <span className="cr-overline">Files</span>
            {docs.map((d) => (
              <div key={d.path} className="exports-doc">
                <Icon name="captions" />
                <span className="cr-clamp-1" title={d.path}>
                  {d.name}
                </span>
                <span className="cr-mono cr-muted">{fileSize(d.bytes)}</span>
                <IconButton label="Reveal" onClick={() => void revealPath(d.path)}><Icon name="folder-open" /></IconButton>
              </div>
            ))}
          </div>
        )}

        <BatchDelivery dir={dir} />
        <BundleExport dir={dir} />
      </div>

      <Drawer
        title="Export"
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        footer={
          <Button
            variant="primary"
            disabled={picked.size === 0 && !interchange}
            onClick={() => void startExport()}
          >
            Export {picked.size > 0 ? picked.size : ""}
          </Button>
        }
      >
        <span className="cr-overline">Formats</span>
        {FORMATS.map((f) => {
          const on = picked.has(f.preset);
          const aspect = f.preset === "square" ? "1:1" : "9:16";
          const [aw, ah] = aspect.split(":").map(Number);
          const placed = new Set(show?.timeline.video_tracks.flatMap((t) => t.clips.map((c) => c.source)));
          const plans = (show?.timeline.sources ?? [])
            .filter((s) => placed.has(s.id) && s.video && Math.abs(s.video.width / s.video.height / (aw / ah) - 1) >= 0.05)
            .map((s) => framing.get(`${s.id}:${aspect}`));
          const approved = show !== null && plans.every((p) => p?.approved);
          const automatic = approved && plans.some((p) => p?.approval?.approver === "policy:auto");
          return (
            <Checkbox
              key={f.preset}
              className={`exports-format ${f.supported ? "" : "exports-format--off"}`}
              checked={on && f.supported}
              disabled={!f.supported}
              onChange={(checked) =>
                setPicked((s) => {
                  const n = new Set(s);
                  if (checked) n.add(f.preset);
                  else n.delete(f.preset);
                  return n;
                })
              }
              label={<span className="exports-format__body">
                <span className="exports-format__title">
                  {f.title}
                  <span className="cr-muted"> {f.sub}</span>
                </span>
                {!f.supported && (
                  <span title={"Square output is not supported by the render engine yet"}><Badge tone="warn">
                    Not available
                  </Badge></span>
                )}
                {f.supported && f.vertical && (
                  <span title={autoFraming && !approved
                        ? "Saved policy approves analysed framing during export"
                        : !approved
                        ? "Vertical exports need an approved reframe plan per source"
                        : "Framing approved for all sources"}><Badge
                    tone={approved || autoFraming ? "ok" : "warn"}
                  >
                    {automatic ? "Approved automatically" : approved ? "Framing approved" : framingLabel(undefined, autoFraming)}
                  </Badge></span>
                )}
              </span>}
            />
          );
        })}
        <Checkbox
          className="exports-format"
          checked={interchange}
          onChange={setInterchange}
          label={<span className="exports-format__body">
            <span className="exports-format__title">Also export for Resolve / Premiere</span>
            <span className="cr-muted">FCPXML, OTIO and captions next to the renders</span>
          </span>}
        />
      </Drawer>
    </PageShell>
  );
}
