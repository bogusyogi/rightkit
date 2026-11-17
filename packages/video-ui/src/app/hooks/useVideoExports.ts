import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { engineCall, errorCode, errorMessage, onEngineEvent, revealPath } from "../../lib/engine.js";
import {
  exportFcpxml, exportOtio, framingApproval, packageBuild, qaList, qaRun,
  reframeShow, rendersDelete, rendersList, settingsGet, sourceList, thumbnails,
  type PackagePlatform, type QaReportRow, type RenderFile, type ReframeSummary,
} from "../../lib/media.js";
import { assetUrl } from "../../lib/asset.js";
import { isPlayable, mediaKind, useMediaPlaybackId } from "../../lib/mediaAdapters.js";
import { projectShow, type ProjectShow } from "../../lib/project.js";

export type VideoExportPreset = "youtube" | "reels" | "tiktok" | "square";
export type VideoExportJob = {
  job: string;
  preset: VideoExportPreset;
  status: "queued" | "running" | "done" | "failed" | "cancelled";
  fraction?: number;
  error?: string;
  cancelling?: boolean;
  cancelError?: string;
  cancelDeclined?: boolean;
};

type QueueJob = VideoExportJob & { registered?: boolean; cancelInFlight?: boolean };
type InterchangeFailure = { format: "fcpxml" | "otio"; detail: string };
type Operation = "export" | "qa" | "delete" | "package";
type Queue = { jobs: QueueJob[]; failures: InterchangeFailure[]; progressAvailable?: boolean; error?: string };
type PackageFailure = { kind: "qa_failed" | "qa_missing" | "other"; detail: string };
type PackageResult = { folder?: string; error?: PackageFailure };
type ExportSnapshot = {
  queue?: Queue;
  active?: Operation;
  target?: string;
  revision: number;
  packages?: Record<string, PackageResult>;
};
const emptySnapshot: ExportSnapshot = { revision: 0 };
const projects = new Map<string, ExportSnapshot>();
const listeners = new Map<string, Set<() => void>>();

function subscribe(dir: string, listener: () => void) {
  const set = listeners.get(dir) ?? new Set();
  set.add(listener);
  listeners.set(dir, set);
  return () => {
    set.delete(listener);
    if (!set.size) listeners.delete(dir);
  };
}

function update(dir: string, values: Partial<ExportSnapshot>) {
  projects.set(dir, { ...getSnapshot(dir), ...values });
  for (const listener of listeners.get(dir) ?? []) listener();
}

const getSnapshot = (dir: string) => projects.get(dir) ?? emptySnapshot;
function useExportSnapshot(dir: string) {
  return useSyncExternalStore((listener) => subscribe(dir, listener), () => getSnapshot(dir), () => emptySnapshot);
}

function claim(dir: string, active: Operation, target?: string) {
  if (getSnapshot(dir).active) return false;
  update(dir, { active, target });
  return true;
}

function release(dir: string, changed: boolean) {
  const { revision } = getSnapshot(dir);
  update(dir, { active: undefined, target: undefined, revision: revision + Number(changed) });
}

function publishQueue(dir: string, queue: Queue) {
  update(dir, { queue: { ...queue, jobs: queue.jobs.map((job) => ({ ...job })), failures: [...queue.failures] } });
}

const liveQueues = new Map<string, Queue>();
const isPending = (job: VideoExportJob) => job.status === "queued" || job.status === "running";

async function dispatchCancellation(dir: string, queue: Queue, job: QueueJob): Promise<void> {
  if (job.status !== "running" || job.cancelInFlight) return;
  const registered = job.registered;
  job.cancelling = true;
  job.cancelInFlight = true;
  job.cancelError = undefined;
  job.cancelDeclined = undefined;
  publishQueue(dir, queue);
  try {
    const reply = await engineCall<{ cancelled: boolean }>("render.cancel", { job: job.job });
    if (reply.cancelled || job.status !== "running") return;
    job.cancelInFlight = false;
    if (!registered && job.registered) {
      await dispatchCancellation(dir, queue, job);
    } else if (registered || !queue.progressAvailable) {
      job.cancelling = false;
      job.cancelDeclined = true;
    }
  } catch (error) {
    if (job.status === "running") {
      job.cancelling = false;
      job.cancelError = errorMessage(error);
    }
  } finally {
    job.cancelInFlight = false;
    if (job.status === "running") publishQueue(dir, queue);
  }
}

const recipePresets: Record<string, VideoExportPreset> = { "16:9": "youtube", "9:16": "reels", "1:1": "square" };
const renderPresets: VideoExportPreset[] = ["youtube", "reels", "square"];
const initialPresets = () => new Set<VideoExportPreset>(["youtube"]);
const renderIdentity = (render: RenderFile) => JSON.stringify([render.path, render.bytes, render.record?.output_blake3]);
type ExportData = { renders: RenderFile[]; reports: Record<string, QaReportRow>; project: ProjectShow; hasSources: boolean; autoFraming: boolean };
type ExportScope = { dir: string; reads: number; drawer: number; picks: number };
type Feedback = { kind: "deleted"; name: string } | { kind: "error"; detail: string };

export function useVideoExports({ dir, progressDone, rendering, onChanged, captureSelection, onFeedback }: {
  dir: string;
  progressDone?: boolean;
  rendering?: boolean;
  onChanged: (dir: string) => Promise<void>;
  captureSelection?: () => () => boolean;
  onFeedback?: (feedback: Feedback) => void;
}) {
  const snapshot = useExportSnapshot(dir);
  const scope = useRef<ExportScope | null>(null);
  const [data, setData] = useState<ExportData>();
  const [loadError, setLoadError] = useState<string>();
  const [sessionQa, setSessionQa] = useState<Record<string, QaReportRow>>({});
  const [preview, setPreview] = useState<string | null>(null);
  const playbackId = useMediaPlaybackId(preview);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [picked, setPicked] = useState(initialPresets);
  const [interchange, setInterchange] = useState(false);
  const [framing, setFraming] = useState<Map<string, ReframeSummary | undefined>>(new Map());
  const [autoFraming, setAutoFraming] = useState(false);

  useLayoutEffect(() => {
    const token = { dir, reads: 0, drawer: 0, picks: 0 };
    scope.current = token;
    setData(undefined);
    setLoadError(undefined);
    setSessionQa({});
    setPreview(null);
    setDrawerOpen(false);
    setPicked(initialPresets());
    setInterchange(false);
    setFraming(new Map());
    setAutoFraming(false);
    return () => { if (scope.current === token) scope.current = null; };
  }, [dir]);

  useEffect(() => {
    const token = scope.current;
    if (!token) return;
    const generation = ++token.reads;
    const current = () => scope.current === token && token.reads === generation;
    void Promise.all([
      rendersList(dir),
      qaList(dir).catch(() => ({ reports: [] })),
      settingsGet(dir),
      projectShow(dir),
      sourceList(dir),
    ]).then(([{ renders }, { reports }, settings, project, { sources }]) => {
      if (!current()) return;
      const reportIndex = Object.fromEntries(reports.map((row) => [row.render_hash, row]));
      setData({ renders, reports: reportIndex, project, hasSources: sources.length > 0, autoFraming: framingApproval(settings) === "auto" });
      setLoadError(undefined);
      const live = new Set(renders.map(renderIdentity));
      setSessionQa((old) => Object.fromEntries(Object.entries(old).filter(([key]) => live.has(key))));
      setPreview((path) => path && renders.some((render) => render.path === path) ? path : null);
    }).catch((error) => { if (current()) setLoadError(errorMessage(error)); });
  }, [dir, progressDone, snapshot.revision]);

  const openDrawer = async () => {
    const token = scope.current;
    if (!token || token.dir !== dir) return;
    const generation = ++token.drawer;
    const picks = token.picks;
    setAutoFraming(data?.autoFraming ?? false);
    setDrawerOpen(true);
    try {
      const settings = await settingsGet(dir);
      if (scope.current !== token || token.drawer !== generation) return;
      setAutoFraming(framingApproval(settings) === "auto");
      if (token.picks !== picks) return;
      const formats = (settings.project.recipe as { formats?: string[] } | undefined)?.formats ?? [];
      const defaults = formats.map((format) => recipePresets[format]).filter((preset) => preset !== undefined);
      if (defaults.length) setPicked(new Set(defaults));
    } catch { /* Retain the current formats if settings cannot be read. */ }
  };

  const closeDrawer = () => {
    if (scope.current) scope.current.drawer += 1;
    setDrawerOpen(false);
  };

  const project = data?.project;
  useEffect(() => {
    if (!drawerOpen || !project) return;
    const token = scope.current;
    let live = true;
    setFraming(new Map());
    const placed = new Set(project.timeline.video_tracks.flatMap((track) => track.clips.map((clip) => clip.source)));
    void Promise.all(project.timeline.sources.filter(({ id }) => placed.has(id)).flatMap(({ id }) =>
      ["9:16", "1:1"].map(async (aspect) => {
        const key = id + ":" + aspect;
        try { return [key, await reframeShow(dir, id, aspect)] as const; }
        catch { return [key, undefined] as const; }
      }),
    )).then((rows) => { if (live && scope.current === token) setFraming(new Map(rows)); });
    return () => { live = false; };
  }, [dir, drawerOpen, project]);

  const framingStatus = (preset: VideoExportPreset) => {
    const aspect = preset === "square" ? "1:1" : "9:16";
    const ratio = preset === "square" ? 1 : 9 / 16;
    const placed = new Set(project?.timeline.video_tracks.flatMap((track) => track.clips.map((clip) => clip.source)));
    const plans = (project?.timeline.sources ?? [])
      .filter((source) => placed.has(source.id) && source.video && Math.abs(source.video.width / source.video.height / ratio - 1) >= 0.05)
      .map((source) => framing.get(source.id + ":" + aspect));
    if (project && plans.every((plan) => plan?.approved)) {
      return plans.some((plan) => plan?.approval?.approver === "policy:auto") ? "approved_automatically" : "approved";
    }
    return autoFraming ? "automatic" : "needs_approval";
  };

  const setPreset = (preset: VideoExportPreset, checked: boolean) => {
    if (scope.current) scope.current.picks += 1;
    setPicked((old) => {
      const next = new Set(old);
      if (checked) next.add(preset);
      else next.delete(preset);
      return next;
    });
  };

  const presets = renderPresets.filter((preset) => picked.has(preset));
  const startExport = async () => {
    if ((!presets.length && !interchange) || rendering || scope.current?.dir !== dir || !claim(dir, "export")) return;
    const prefix = "ui-x" + Date.now().toString(36) + "-" + Math.random().toString(36).slice(2);
    const queue: Queue = { jobs: presets.map((preset, index) => ({ job: prefix + "-" + index, preset, status: "queued" })), failures: [] };
    liveQueues.set(dir, queue);
    publishQueue(dir, queue);
    closeDrawer();

    let off: (() => void) | undefined;
    try {
      if (queue.jobs.length) {
        // Run even if the progress feed is unavailable. The render reply is authoritative.
        try {
          off = await onEngineEvent((frame) => {
            const event = frame.event;
            if (event.event !== "progress") return;
            const job = queue.jobs.find((row) => row.status === "running" && row.job === event.job && frame.tag === "exports-" + row.job);
            if (!job) return;
            const first = !job.registered;
            job.registered = true;
            if (first && job.cancelling && !job.cancelInFlight) void dispatchCancellation(dir, queue, job);
            job.fraction = typeof event.out_time_ms === "number" && typeof event.total_ms === "number" && event.total_ms > 0
              ? Math.max(0, Math.min(1, event.out_time_ms / event.total_ms))
              : undefined;
            publishQueue(dir, queue);
          });
          queue.progressAvailable = true;
        } catch (error) { queue.error = errorMessage(error); }
      }
      for (const job of queue.jobs) {
        if (job.status === "cancelled") continue;
        job.status = "running";
        publishQueue(dir, queue);
        try {
          await engineCall("render", { dir, preset: job.preset, job: job.job }, "exports-" + job.job);
          job.status = "done";
          job.fraction = 1;
        } catch (error) {
          const code = errorCode(error);
          job.status = code === "cancelled" || code === "render_cancelled" ? "cancelled" : "failed";
          if (job.status === "failed") job.error = errorMessage(error);
        }
        job.cancelling = false;
        publishQueue(dir, queue);
      }
      if (interchange) {
        for (const [format, write] of [["fcpxml", exportFcpxml], ["otio", exportOtio]] as const) {
          try { await write(dir); }
          catch (error) { queue.failures.push({ format, detail: errorMessage(error) }); }
        }
      }
      try { await onChanged(dir); }
      catch (error) { queue.error = errorMessage(error); }
    } finally {
      try { off?.(); }
      catch (error) { queue.error ??= errorMessage(error); }
      finally {
        liveQueues.delete(dir);
        publishQueue(dir, queue);
        release(dir, true);
      }
    }
  };

  const cancelJob = (id: string) => {
    const queue = liveQueues.get(dir);
    const job = queue?.jobs.find((row) => row.job === id);
    if (!queue || !job || job.cancelling) return;
    if (job.status === "queued") {
      job.status = "cancelled";
      publishQueue(dir, queue);
    } else if (job.status === "running") {
      void dispatchCancellation(dir, queue, job);
    }
  };

  const dismissJob = (id: string) => {
    const queue = liveQueues.get(dir) ?? getSnapshot(dir).queue;
    if (!queue) return;
    const jobs = queue.jobs.filter((row) => row.job !== id || isPending(row));
    if (liveQueues.has(dir)) queue.jobs = jobs;
    publishQueue(dir, { ...queue, jobs });
  };

  const act = async (kind: "qa" | "delete", render: RenderFile, action: (token: ExportScope, notify: (feedback: Feedback) => void) => Promise<void>) => {
    const token = scope.current;
    if (!token || token.dir !== dir || !claim(dir, kind, render.name)) return;
    const canNotify = captureSelection?.() ?? (() => scope.current === token);
    const notify = (feedback: Feedback) => { if (canNotify()) onFeedback?.(feedback); };
    try {
      await action(token, notify);
    } catch (error) {
      notify({ kind: "error", detail: errorMessage(error) });
    } finally {
      release(dir, true);
    }
  };

  const runQa = (render: RenderFile) => act("qa", render, async (token) => {
    const result = await qaRun(dir, render.path);
    if (scope.current !== token || result.render !== render.path) return;
    const checks = result.report?.checks ?? [];
    const hash = (result.receipt as { render_blake3?: string } | undefined)?.render_blake3;
    const row = { render_hash: hash ?? "", status: result.status, issues: checks.filter((check) => check.status !== "pass").length, checks };
    setSessionQa((old) => ({ ...old, [renderIdentity(render)]: row }));
  });

  const deleteRender = (render: RenderFile) => act("delete", render, async (_token, notify) => {
    await rendersDelete(dir, render.name);
    const packages = getSnapshot(dir).packages;
    const identity = renderIdentity(render);
    if (packages && Object.hasOwn(packages, identity)) {
      const remaining = { ...packages };
      delete remaining[identity];
      update(dir, { packages: remaining });
    }
    notify({ kind: "deleted", name: render.name });
    await onChanged(dir);
  });

  const renders = data?.renders;
  const jobs: VideoExportJob[] = snapshot.queue?.jobs ?? [];
  const qaFor = (render: RenderFile) => {
    const fresh = sessionQa[renderIdentity(render)];
    const hash = fresh?.render_hash || render.record?.output_blake3;
    return (hash ? data?.reports[hash] : undefined) ?? fresh;
  };
  return {
    renders, mp4s: (renders ?? []).filter((row) => row.name.endsWith(".mp4")),
    documents: (renders ?? []).filter((row) => /\.(srt|vtt|fcpxml|otio|json)$/.test(row.name)),
    loadError, hasSources: data?.hasSources ?? false, qaFor,
    preview: preview ? { path: preview, kind: mediaKind(preview), playbackId } : null,
    previewRender: (render: RenderFile) => { if (isPlayable(render.path)) setPreview(render.path); },
    closePreview: () => setPreview(null),
    drawerOpen, openDrawer, closeDrawer, picked, setPreset, framingStatus, interchange, setInterchange,
    canStart: !snapshot.active && !rendering && (presets.length > 0 || interchange),
    jobs, showQueue: jobs.some((job) => job.status !== "done") || Boolean(rendering) || snapshot.active === "export",
    active: snapshot.active, target: snapshot.target, queueError: snapshot.queue?.error,
    interchangeFailures: snapshot.queue?.failures ?? [], startExport, cancelJob, dismissJob,
    notifyBatchChanged: (changedDir: string) => {
      update(changedDir, { revision: getSnapshot(changedDir).revision + 1 });
      return onChanged(changedDir);
    },
    runQa, deleteRender, reveal: revealPath, copyPath: (path: string) => navigator.clipboard.writeText(path),
  };
}

export function useVideoExportThumbnail(dir: string, render: string) {
  const [src, setSrc] = useState<string>();
  useEffect(() => {
    let live = true;
    setSrc(undefined);
    void thumbnails(dir, { render }, { width: 480 }).then(({ frames }) => {
      if (live && frames[0]) setSrc(assetUrl(frames[0].path));
    }).catch(() => undefined);
    return () => { live = false; };
  }, [dir, render]);
  return src;
}

export function useVideoExportPackage(dir: string, render: RenderFile) {
  const [platform, setPlatform] = useState<PackagePlatform>("youtube");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [tags, setTags] = useState("");
  const snapshot = useExportSnapshot(dir);
  const identity = renderIdentity(render);
  const { folder, error } = snapshot.packages?.[identity] ?? {};
  const storeResult = (result: PackageResult) => {
    update(dir, { packages: { ...getSnapshot(dir).packages, [identity]: result } });
  };
  const submit = async () => {
    if (!title.trim() || !claim(dir, "package", render.name)) return;
    storeResult({});
    try {
      const result = await packageBuild(dir, {
        platform, render: render.name, title: title.trim(), description: description.trim(),
        tags: tags.split(",").map((tag) => tag.trim()).filter(Boolean),
      });
      storeResult({ folder: result.manifest.replace(/[\\/][^\\/]+$/, "") });
    } catch (cause) {
      const message = errorMessage(cause);
      const code = message.match(/^qa_(?:failed|missing)(?=:)/)?.[0] ?? errorCode(cause);
      const kind = code === "qa_failed" || code === "qa_missing" ? code : "other";
      storeResult({ error: { kind, detail: message.replace(/^qa_(?:failed|missing):\s*/, "") } });
    } finally { release(dir, false); }
  };
  return {
    platform, setPlatform, title, setTitle, description, setDescription, tags, setTags,
    folder, error, canSubmit: !!title.trim() && !snapshot.active,
    busy: snapshot.active === "package" && snapshot.target === render.name,
    submit, reveal: revealPath,
  };
}
