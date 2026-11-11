// Folds engine events into UI progress. Event names and fields are the engine's
// (the video engine's commands, rightkit-media-gen-client events, `progress_line`):
//
//   assemble.started  {shots, narration, resumed, requests:{label:request_id}, assemble_id}
//   media_gen.request  {request_id, state: journaled|submitted, job_id?}
//   media_gen.job      {request_id, job_id, status, attempts, error}
//   media_gen.job_progress {request_id?, job_id, status, stage, attempt, at, message?}
//   media_gen.import   {request_id, job_id, asset_id, media_path, sha256}
//   import.done       {item (label), request_id, job_id, path, blake3, needs_probe} (assemble)
//                     {request_id, job_id, asset_id, path, source_id, needs_probe}  (generate)
//   timeline.built    {video_clips, added_clips, titles, total_frames, revision}
//   render.progress   {frame, out_time_ms, total_ms}   (inside assemble; tagged assemble_id)
//   progress          {job, frame, out_time_ms, total_ms}   (standalone render)
//   assemble.done     {result:{status, assemble_id, render?:{output, ...}}}
//   assemble.failed   {code, message}
//
// Frames arrive as `{id, tag, event}` (engine request id, caller tag, engine event).

export type EngineEvent = { event?: string } & Record<string, unknown>;
export type EngineFrame = { id: number | null; tag: string | null; event: EngineEvent };

export type JobView = {
  /** request id (stable across retries). */
  id: string;
  label: string;
  jobId?: string;
  /** journaled | submitted */
  request?: string;
  status?: string;
  stage?: string;
  message?: string;
  imported: boolean;
};

export type ProgressState = {
  /** Correlation: frames are accepted only for this tag (then this engine frame id). */
  tag: string | null;
  frameId: number | null;
  assembleId: string | null;
  running: boolean;
  resumed: boolean;
  stages: string[];
  jobs: Record<string, JobView>;
  imports: number;
  render: { job?: string; frame?: number; out_time_ms?: number; total_ms?: number } | null;
  timeline: { video_clips?: number; titles?: number; total_frames?: number } | null;
  error: { code?: string; message: string } | null;
  cancelled: boolean;
  output: string | null;
  done: boolean;
  log: EngineEvent[];
};

export const initialProgress: ProgressState = {
  tag: null,
  frameId: null,
  assembleId: null,
  running: false,
  resumed: false,
  stages: [],
  jobs: {},
  imports: 0,
  render: null,
  timeline: null,
  error: null,
  cancelled: false,
  output: null,
  done: false,
  log: [],
};

export const assembleRenderJob = (assembleId: string) => `assemble-${assembleId}-render`;

const str = (v: unknown) => (typeof v === "string" ? v : undefined);
const num = (v: unknown) => (typeof v === "number" ? v : undefined);
const isObj = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null;

/** Starts tracking one engine call. */
export function beginProgress(tag: string, assembleId: string | null): ProgressState {
  return { ...initialProgress, tag, assembleId, running: true };
}

function labelOf(state: ProgressState, requestId: string): string {
  return state.jobs[requestId]?.label ?? requestId;
}

function upsertJob(state: ProgressState, requestId: string, patch: Partial<JobView>): ProgressState["jobs"] {
  const prev = state.jobs[requestId] ?? { id: requestId, label: labelOf(state, requestId), imported: false };
  return { ...state.jobs, [requestId]: { ...prev, ...patch } };
}

const withStage = (state: ProgressState, name: string) =>
  state.stages.includes(name) ? state.stages : [...state.stages, name];

export function reduceFrame(state: ProgressState, frame: EngineFrame): ProgressState {
  if (state.tag === null || frame.tag !== state.tag) return state;
  if (state.frameId !== null && frame.id !== null && frame.id !== state.frameId) return state;
  const ev = frame.event;
  const next: ProgressState = {
    ...state,
    frameId: state.frameId ?? frame.id,
    log: [...state.log.slice(-99), ev],
  };
  const requestId = str(ev.request_id);
  switch (str(ev.event)) {
    case "assemble.started": {
      const jobs: ProgressState["jobs"] = {};
      if (isObj(ev.requests)) {
        for (const [label, rid] of Object.entries(ev.requests)) {
          if (typeof rid === "string") jobs[rid] = { id: rid, label, imported: false };
        }
      }
      const assembleId = str(ev.assemble_id) ?? state.assembleId;
      return {
        ...next,
        assembleId,
        running: true,
        resumed: ev.resumed === true,
        jobs,
        stages: ["assemble.started"],
        error: null,
        cancelled: false,
      };
    }
    case "media_gen.request":
      if (!requestId) return next;
      next.jobs = upsertJob(state, requestId, { request: str(ev.state), jobId: str(ev.job_id) });
      return next;
    case "media_gen.job":
      if (!requestId) return next;
      next.jobs = upsertJob(state, requestId, {
        jobId: str(ev.job_id),
        status: str(ev.status),
        message: isObj(ev.error) ? str(ev.error.message) : str(ev.error),
      });
      return next;
    case "media_gen.job_progress": {
      // job_progress carries request_id only when the orchestrator forwards it; fall back
      // to matching the job id.
      const jobId = str(ev.job_id);
      const rid = requestId ?? Object.values(state.jobs).find((j) => j.jobId === jobId)?.id;
      if (!rid) return next;
      next.jobs = upsertJob(state, rid, {
        jobId: jobId ?? state.jobs[rid]?.jobId,
        status: str(ev.status) ?? state.jobs[rid]?.status,
        stage: str(ev.stage) ?? state.jobs[rid]?.stage,
        message: str(ev.message) ?? state.jobs[rid]?.message,
      });
      return next;
    }
    case "media_gen.import":
    case "import.done": {
      if (requestId && state.jobs[requestId]) next.jobs = upsertJob(state, requestId, { imported: true });
      if (str(ev.event) === "import.done") next.imports = state.imports + 1;
      return next;
    }
    case "timeline.built":
      next.stages = withStage(state, "timeline.built");
      next.timeline = {
        video_clips: num(ev.video_clips),
        titles: num(ev.titles),
        total_frames: num(ev.total_frames),
      };
      return next;
    case "render.progress":
    case "progress":
      next.stages = withStage(state, "render");
      next.render = {
        job: str(ev.job) ?? state.render?.job ?? (state.assembleId ? assembleRenderJob(state.assembleId) : undefined),
        frame: num(ev.frame),
        out_time_ms: num(ev.out_time_ms),
        total_ms: num(ev.total_ms) ?? state.render?.total_ms,
      };
      return next;
    case "assemble.done": {
      const result = isObj(ev.result) ? ev.result : {};
      const render = isObj(result.render) ? result.render : {};
      next.running = false;
      next.done = true;
      next.output = str(render.output) ?? state.output;
      next.stages = withStage(state, "assemble.done");
      return next;
    }
    case "assemble.failed": {
      const code = str(ev.code);
      next.running = false;
      if (code === "cancelled" || code === "render_cancelled") {
        next.cancelled = true;
        next.error = null;
      } else {
        next.error = { code, message: str(ev.message) ?? "assembly failed" };
      }
      return next;
    }
    default:
      return next;
  }
}

/** Rebuilds what is known from an `assemble.status` result (after an app restart). */
export function seedFromStatus(
  state: ProgressState,
  status: {
    assemble_id: string;
    status: string;
    stage: string;
    error: { code?: string; message?: string } | null;
    timeline_built: boolean;
    render: { output?: string } | null;
    render_exists?: boolean;
    requests: { item: string; request_id: string; state: string | null; job_id: string | null; imported: boolean }[];
  },
): ProgressState {
  // A stored render whose file is gone is not a finished render: the render step must run again.
  const renderMissing = !!status.render && status.render_exists === false;
  const jobs: ProgressState["jobs"] = {};
  for (const r of status.requests) {
    jobs[r.request_id] = {
      id: r.request_id,
      label: r.item,
      jobId: r.job_id ?? undefined,
      request: r.state ?? undefined,
      imported: r.imported,
    };
  }
  const cancelled = status.status === "cancelled";
  return {
    ...state,
    assembleId: status.assemble_id,
    jobs,
    stages: [
      ...(status.timeline_built ? ["timeline.built"] : []),
      ...(status.status === "done" && !renderMissing ? ["assemble.done"] : []),
    ],
    done: status.status === "done" && !renderMissing,
    cancelled,
    output: renderMissing ? null : (status.render?.output ?? null),
    error: status.status === "failed" && status.error ? { code: status.error.code, message: status.error.message ?? "assembly failed" } : null,
  };
}
