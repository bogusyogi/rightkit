import { invoke } from "@tauri-apps/api/core";
import { engineCall } from "./engine";

// Typed client for the media workflow methods served by cutright-engine
// (crates/cutright-engine/src/media_cmd.rs).

export type CutPreset = "tight" | "natural" | "punchy" | "fast";

export type TranscriptWord = { text: string; start_ms: number; end_ms: number };
export type Transcript = {
  duration_ms: number;
  words: TranscriptWord[];
  model?: { id?: string };
};

export type TranscribeResult = {
  status: string;
  job: string;
  source: string;
  duration_ms: number;
  words: number;
  vad_spans: number;
  report?: { backend?: string; vad_backend?: string; elapsed_ms?: number };
};

export type EditorialItem = {
  id: string;
  kind: string;
  tier?: string;
  confidence?: number;
  start_ms: number;
  end_ms: number;
  reason: string;
  text?: string;
};

export type TakeInfo = {
  index: number;
  start_ms: number;
  end_ms: number;
  text: string;
  rank: number;
  chosen: boolean;
  explanation?: string[];
};

export type TakeGroup = { id: string; chosen: number; takes: TakeInfo[] };

export type SourcePlan = {
  source: string;
  mode: "speech" | "visual";
  plan_id: string;
  duration_ms?: number;
  items: EditorialItem[];
  take_groups: TakeGroup[];
  beats: unknown[];
};

export type AnalysisResult = {
  status: string;
  preset: string;
  /** Present on single-source runs (flat shape) — also mirrored in plans. */
  plan_id?: string;
  source?: string;
  mode?: "speech" | "visual";
  items?: EditorialItem[];
  take_groups?: TakeGroup[];
  beats?: unknown[];
  plans?: SourcePlan[];
  cross_file_retakes?: number;
};

export type ApplyResult = {
  status: string;
  revision?: { cursor: number; total: number };
  kept_ranges?: number;
  removed?: number;
  output_frames?: number;
  mode?: "speech" | "visual";
  group?: string;
  applied?: {
    source: string;
    mode: "speech" | "visual";
    kept_ranges: number;
    removed: number;
    output_frames: number;
  }[];
};

export type SourceRow = {
  id: string;
  name: string;
  path?: string;
  duration?: number;
  verified?: boolean;
  error?: string;
};

export type PhraseMatch = {
  source: string;
  name: string;
  word_index: number;
  start_ms: number;
  end_ms: number;
  context: { start_ms: number; end_ms: number; text: string };
};

export type ShortProposal = {
  id: string;
  rank: number;
  start_ms: number;
  end_ms: number;
  duration_ms: number;
  clip_start_ms?: number;
  clip_end_ms?: number;
  transcript: string;
  title: string;
  rationale: string;
  /** Ranker output; `composite` is 0–1. */
  scores?: { composite?: number };
  platform_fit?: { platform: string; duration_fit?: number; fits: boolean }[];
};

/** How the proposals were ranked (`RankingInfo` in cutright-shorts). */
export type ShortsRanking = {
  /** `"llm"` or `"heuristic"`. */
  method: string;
  provider?: string | null;
  model?: string | null;
  /** Why the heuristic scorer ran instead of the LLM; null when the LLM ranked. */
  fallback_reason?: string | null;
  llm_moments_returned?: number;
  llm_moments_accepted?: number;
  prompt_budget?: {
    action: string;
    budget_tokens: number;
    requests: number;
    sentences_offered: number;
    sentences_sent: number;
    requests_cut_by_budget: number;
    note: string;
  } | null;
};

export type ShortsResult = {
  status: string;
  proposals: ShortProposal[];
  ranking?: ShortsRanking;
  rejected?: number;
  windows_considered?: number;
};

export type RenderFile = {
  name: string;
  path: string;
  bytes: number;
  /** Render-lane receipt (`cutright.render.v1`) attached to mp4 entries. */
  record?: {
    schema?: string;
    name?: string;
    preset?: string;
    encoder?: string;
    created_at?: string;
    output_blake3?: string;
    duration_ms?: number;
    loudness?: {
      after?: { input_i?: number; input_tp?: number };
      measured?: { input_i?: number };
      within_tolerance?: boolean;
      skipped_silent?: boolean;
    };
    origin?: { kind?: string; variant?: string; timeline_cursor?: number; timeline_head_hash?: string };
    time_map?: {
      kind?: string;
      clip_id?: string;
      source?: string;
      source_in_ms: number;
      source_out_ms: number;
      out_in_ms: number;
      out_end_ms: number;
    }[];
  };
};

export type QaResult = {
  status: string;
  render: string;
  report?: { checks?: { id?: string; status?: string; detail?: string }[] };
  receipt?: unknown;
};

export type ProjectCard = {
  dir: string;
  id?: string;
  name?: string;
  created?: string;
  /** Epoch seconds; project.json mtime. */
  edited?: number | null;
  sources?: number;
  missing_sources?: number;
  duration_ms?: number;
  aspect?: string;
  status?: "finished" | "needs_review" | "media_only" | "missing_media";
  /** `media.thumbnail` target for the card image (latest render, else first video source). */
  thumb?: { render?: string; source?: string } | null;
  error?: string;
};

export const projectsList = (roots: string[]) =>
  engineCall<{ projects: ProjectCard[] }>("projects.list", {
    roots,
  });

export const sourceList = (dir: string) =>
  engineCall<{ sources: SourceRow[] }>("source.list", { dir });

export const sourceVerify = (dir: string, tag?: string) =>
  engineCall<{
    ok: boolean;
    checks: { source_id: string; status: string; found_at?: string | null }[];
  }>("source.verify", { dir }, tag);

export const sourceRelink = (dir: string, source: string, path: string) =>
  engineCall<{ status: string }>("source.relink", { dir, source, path });

export const transcribe = (dir: string, source: string, job?: string, install?: boolean, tag?: string) =>
  engineCall<TranscribeResult>(
    "transcribe",
    { dir, source, job, install: install ?? true },
    tag,
  );

export const transcribeCancel = (job: string) =>
  engineCall<{ cancelled: boolean }>("transcribe.cancel", { job });

export const transcriptShow = (dir: string, source: string) =>
  engineCall<{ transcript: Transcript }>("transcript.show", { dir, source });

export const phraseSearch = (dir: string, query: string) =>
  engineCall<{ query: string; matches: PhraseMatch[] }>("phrase.search", { dir, query });

export const autoeditAnalyze = (
  dir: string,
  preset: CutPreset,
  opts?: { source?: string; sources?: string[] },
  tag?: string,
) =>
  engineCall<AnalysisResult>(
    "autoedit.analyze",
    {
      dir,
      preset,
      ...(opts?.source ? { source: opts.source } : {}),
      ...(opts?.sources ? { sources: opts.sources } : {}),
    },
    tag,
  );

export const autoeditApply = (
  dir: string,
  preset: CutPreset,
  opts?: {
    source?: string;
    sources?: string[];
    review?: { reject?: string[]; accept?: string[]; takes?: Record<string, number> };
  },
  tag?: string,
) =>
  engineCall<ApplyResult>(
    "autoedit.apply",
    {
      dir,
      preset,
      ...(opts?.source ? { source: opts.source } : {}),
      ...(opts?.sources ? { sources: opts.sources } : {}),
      ...(opts?.review ? { review: opts.review } : {}),
    },
    tag,
  );

export const captionsExport = (dir: string, source: string, format: "srt" | "vtt") =>
  engineCall<{ status: string; path?: string; cues?: number }>("captions.export", {
    dir,
    source,
    format,
  });

export const shortsPropose = (dir: string, source: string, count = 5) =>
  engineCall<ShortsResult>("shorts.propose", { dir, source, count });

export const shortsAccept = (
  dir: string,
  source: string,
  proposal: string,
  opts?: { start_ms?: number; end_ms?: number },
  tag?: string,
) =>
  engineCall<{ status: string; variant: string; output_frames: number }>(
    "shorts.accept",
    { dir, source, proposal, ...(opts ?? {}) },
    tag,
  );

/** `clip.export` — render one source range through the full finish path (R7). */
export const clipExport = (
  dir: string,
  params: { source: string; start_ms: number; end_ms: number; preset?: string; name?: string },
  tag?: string,
) =>
  engineCall<{ status: string; output: string; name: string; bytes: number; job: string }>(
    "clip.export",
    { dir, ...params },
    tag,
  );

export const rendersList = (dir: string) =>
  engineCall<{ renders: RenderFile[] }>("renders.list", { dir });

export const qaRun = (dir: string, render?: string, preset?: string, tag?: string) =>
  engineCall<QaResult>("qa.run", { dir, render, preset }, tag);

export const exportFcpxml = (dir: string, name?: string) =>
  engineCall<{ fcpxml: string; unsupported: string[] }>("export.fcpxml", { dir, name });

export const exportOtio = (dir: string, name?: string) =>
  engineCall<{ otio: string | null; unsupported: string[] }>("export.otio", { dir, name });

export const variantsList = (dir: string) =>
  engineCall<{ variants: { name: string; path?: string; error?: string }[] }>("variants.list", {
    dir,
  });

export const variantsCreate = (dir: string, source: string) =>
  engineCall<{ status: string; variants: { name: string; dir?: string; error?: string }[] }>(
    "variants.create",
    { dir, source },
  );

export const variantsPromote = (dir: string, variant: string) =>
  engineCall<{ status: string }>("variants.promote", { dir, variant });

export type DropOff = {
  id: string;
  out_start_ms: number;
  out_end_ms: number;
  drop_points: number;
  suggested: string;
  source_mappings: {
    source: string;
    source_in_ms: number;
    source_out_ms: number;
    out_start_ms: number;
    out_end_ms: number;
  }[];
};

export const retentionImport = (dir: string, csv: string, render: string, name?: string) =>
  engineCall<{
    status: string;
    name: string;
    points: number;
    drop_offs: number;
    render_hash?: string;
    revision?: number;
  }>("retention.import", { dir, name, csv, render });

export const retentionList = (dir: string) =>
  engineCall<{
    retention: {
      name: string;
      render?: string;
      points: number;
      drop_offs?: number;
      revision?: number;
    }[];
  }>("retention.list", { dir });

export const retentionProposals = (dir: string, name: string) =>
  engineCall<{ name: string; render?: string; drop_offs: DropOff[] }>(
    "retention.proposals",
    { dir, name },
  );

export const retentionApply = (dir: string, name: string, drops?: string[], tag?: string) =>
  engineCall<{
    status: string;
    cuts: number;
    revision?: { cursor: number; total: number };
    render_request?: string;
  }>("retention.apply", { dir, name, ...(drops ? { drops } : {}) }, tag);

export type FramingApproval = "review" | "auto";
export const framingApproval = (settings: { app: Record<string, unknown>; project: Record<string, unknown> }): FramingApproval =>
  (settings.project.framing_approval ?? settings.app.framing_approval ?? "review") === "auto" ? "auto" : "review";

export const framingLabel = (plan: ReframeSummary | undefined, auto: boolean): string =>
  plan?.approved
    ? plan.approval?.approver === "policy:auto" ? "Approved automatically" : "Framing approved"
    : auto ? "Framing: automatic" : "Needs framing approval";

export const settingsGet = (dir?: string) =>
  engineCall<{ app: Record<string, unknown>; project: Record<string, unknown>; revision: string }>(
    "settings.get",
    dir ? { dir } : {},
  );

/** Fired on `window` after a finishing setting is saved. `detail.dir` is the project it was saved
 *  to, or absent for an app-wide save, so a listener for one project can tell them apart. */
export const SETTINGS_CHANGED = "cutright:settings-changed";

const notifySettingsChanged = (dir?: string) =>
  window.dispatchEvent(new CustomEvent<{ dir?: string }>(SETTINGS_CHANGED, { detail: { dir } }));

/** `expectedRevision` (the `revision` `settings.get` reported) makes the write a compare-and-set:
 *  a file that changed since is refused with code `revision_conflict` and nothing is written. */
export const settingsSet = (settings: Record<string, unknown>, dir?: string, expectedRevision?: string) =>
  engineCall<{ status: string; settings: Record<string, unknown>; revision: string }>("settings.set", {
    ...(dir ? { dir } : {}),
    ...(expectedRevision !== undefined ? { expected_revision: expectedRevision } : {}),
    settings,
  }).then((r) => {
    notifySettingsChanged(dir);
    return r;
  });

// ---- Colour grade (colour.get / colour.set / colour.scopes) ----

export type ColourPt = { x: number; y: number };
export type ColourRgb = [number, number, number];
export type ColourWheel = "lift" | "gamma" | "gain";
export type ColourCurveChannel = "master" | "r" | "g" | "b";
export type ColourRange = "reds" | "yellows" | "greens" | "cyans" | "blues" | "magentas";
export type ColourHsl = {
  range: ColourRange;
  hue_shift: number;
  saturation: number;
  lightness: number;
  strength: number;
};
/** Partial grade: an absent field is neutral. Ranges are enforced by the engine. */
export type ColourGrade = {
  curves?: Partial<Record<ColourCurveChannel, ColourPt[]>>;
  wheels?: Record<ColourWheel, ColourRgb>;
  hsl?: ColourHsl[];
  saturation?: number;
};

export type ColourGetResult = {
  grade: ColourGrade | null;
  by_source: Record<string, ColourGrade>;
  /** Absent on older engines; `hdr_to_sdr: false` means HDR footage cannot be finished here. */
  capabilities?: { hdr_to_sdr?: boolean };
};

export type ColourSetResult = {
  status: "saved";
  scope: "project" | "source";
  source?: string;
  grade: ColourGrade | null;
};

export type ColourScopes = {
  status: "ok";
  at_ms: number;
  graded: boolean;
  /** Local file paths; show through `assetUrl`. */
  waveform: string;
  vectorscope: string;
  stats: { y_min: number; y_max: number; y_avg: number; sat_avg: number; sat_max: number };
  clipped: { low: boolean; high: boolean };
};

/** Project grade plus per-source grades for the project at `dir`. */
export const colourGet = (args: { dir: string }) =>
  engineCall<ColourGetResult>("colour.get", { ...args });

/** Omit `source` to write the project grade; `grade: null` clears a grade. */
export const colourSet = (args: { dir: string; source?: string; grade: ColourGrade | null }) =>
  engineCall<ColourSetResult>("colour.set", {
    dir: args.dir,
    ...(args.source !== undefined ? { source: args.source } : {}),
    grade: args.grade,
  }).then((r) => {
    notifySettingsChanged(args.dir);
    return r;
  });

export const colourScopes = (args: {
  dir: string;
  source: string;
  at_ms?: number;
  graded?: boolean;
  width?: number;
}) => engineCall<ColourScopes>("colour.scopes", { ...args });

export const storageStatus = (dir?: string) =>
  engineCall<{
    cache_dirs: { path: string; bytes: number }[];
    cache_bytes: number;
    /** Where dropped-footage projects go when Settings › Storage › Project folder is unset. */
    default_project_root?: string;
  }>("storage.status", dir ? { dir } : {});

export const storageClear = (dir?: string) =>
  engineCall<{ status: string; freed_bytes: number }>("storage.clear", dir ? { dir } : {});

export type ModelKind = "speech" | "face";

export type ModelRow = {
  id?: string;
  label?: string;
  version?: number;
  ready?: boolean;
  state?: "ready" | "missing" | "partial" | "installing";
  files_ok?: number;
  files_total?: number;
  needed?: string[];
  bytes_on_disk?: number;
  size_hint_bytes?: number;
  can_download?: boolean;
  in_progress?: boolean;
  dir?: string;
  error?: string;
};

export const modelsStatus = () =>
  engineCall<{
    speech: ModelRow;
    face: ModelRow;
    models_root?: string;
    signed_packs_only?: boolean;
  }>("models.status");

export type ModelProgress = {
  event: "model.progress";
  model: ModelKind;
  phase: "verify" | "download" | "install" | "done";
  file?: string;
  received?: number;
  total?: number;
};

/** `models.install` — install or repair one signed-pack model (progress: `model.progress` events on `tag`). */
export const modelsInstall = (model: ModelKind, tag?: string) =>
  engineCall<ModelRow>("models.install", { model }, tag);

export interface ReframeSummary {
  status?: string;
  source?: string;
  plan_path: string;
  plan_hash: string;
  mode: string;
  target_aspect?: string;
  output: [number, number];
  crop: [number, number];
  duration_ms: number;
  keyframes: number;
  anchors: number;
  anchors_approved: number;
  approved: boolean;
  approval?: { approver: string; approved_at: string; plan_hash: string } | null;
  detector?: { model?: string } & Record<string, unknown>;
  stats?: Record<string, unknown>;
  plan?: {
    anchors?: { start_ms?: number; end_ms?: number; approved?: boolean; gap?: boolean }[];
  } & Record<string, unknown>;
}

/** R2: plans are per (source × aspect); `aspect` is "9:16" or "1:1". */
export const reframeAnalyze = (dir: string, source: string, aspect?: string, tag?: string) =>
  engineCall<ReframeSummary>(
    "reframe.analyze",
    { dir, source, ...(aspect ? { aspect } : {}) },
    tag,
  );

export const reframeShow = (dir: string, source: string, aspect?: string) =>
  engineCall<ReframeSummary>("reframe.show", {
    dir,
    source,
    ...(aspect ? { aspect } : {}),
  });

export const reframeApprove = (
  dir: string,
  source: string,
  opts: { anchor?: number; all?: boolean; approve?: boolean; approver?: string; aspect?: string },
) =>
  engineCall<ReframeSummary>("reframe.approve", { dir, source, ...opts });

export const reframeRender = (dir: string, source: string, aspect?: string, tag?: string) =>
  engineCall<{ status: string; output: string; receipt: unknown }>(
    "reframe.render",
    { dir, source, ...(aspect ? { aspect } : {}) },
    tag,
  );

/** Caption looks the engine renders, with the labels the screens show. */
export const CAPTION_STYLES = [
  ["classic-box", "Classic box"],
  ["clean-outline", "Clean outline"],
  ["shorts-pop", "Shorts pop"],
  ["karaoke-pop", "Karaoke pop"],
  ["karaoke-fill", "Karaoke fill"],
  ["card-classic", "Card classic"],
  ["card-emphasis", "Card emphasis"],
  ["minimal-lower", "Minimal lower"],
] as const;
/** The caption look used when none is chosen; matches the engine default (`captions_cmd.rs`). */
export const DEFAULT_CAPTION_STYLE = "classic-box";

/** R3: PNG swatch of a caption style on a source frame (`captions.preview`). */
export type CaptionPreview = {
  status: string;
  path: string;
  style: string;
  preset: string;
  width: number;
  height: number;
  mtime_ms?: number;
};

export const captionsPreview = (
  dir: string,
  opts: { style?: string; preset?: string; source?: string } = {},
) => engineCall<CaptionPreview>("captions.preview", { dir, ...opts });

// ---- Native playback (Rust decode + wgpu composite; frames over IPC) ----

export type PlaybackMeta = {
  kind: "timeline" | "file";
  playing: boolean;
  position_ms: number;
  frame: number;
  total_frames: number;
  duration_ms: number;
  fps: { num: number; den: number };
  width: number;
  height: number;
  audio: string;
  speed: number;
  loop?: { start_ms: number; end_ms: number } | null;
  revision: number;
};

export const playbackOpen = (params: {
  dir?: string;
  file?: string;
  width?: number;
  height?: number;
}) => invoke<PlaybackMeta>("playback_open", { params, toolsDir: null });

/** Rebuilds the open timeline preview with freshly fetched colour chains and restores the
 *  playhead. A grade change does not change the revision, so the revision-keyed reopen misses it.
 *  Resolves null when no timeline preview is open. */
/** Fired on `window` after the preview session was rebuilt; a paused player repaints on it. */
export const PLAYBACK_REFRESHED = "cutright:playback-refreshed";

export const playbackRefreshFinishing = (dir: string) =>
  invoke<PlaybackMeta | null>("playback_refresh_finishing", { dir, toolsDir: null }).then((meta) => {
    window.dispatchEvent(new Event(PLAYBACK_REFRESHED));
    return meta;
  });

export const playbackClose = () => invoke<void>("playback_close");

export const playbackStatus = () => invoke<PlaybackMeta>("playback_status");

export const playbackControl = (cmd: string, value?: number, value2?: number) =>
  invoke<PlaybackMeta>("playback_control", { cmd, value: value ?? 0, value2: value2 ?? 0 });

/** Binary frame reply: `[u32 w][u32 h][u32 frame][u32 total][f64 pts]` + RGBA8. */
export const playbackFrame = () => invoke<ArrayBuffer | Uint8Array>("playback_frame");

/** Runtime asset-protocol scope grant (project dirs, referenced media). */
export const mediaScopeAllow = (paths: string[], recursive?: boolean) =>
  invoke<void>("media_scope_allow", { paths, recursive: recursive ?? null });

/** One cached JPEG frame (`media.thumbnail`); `path` is local — show via `assetUrl`. */
export type ThumbFrame = { at_ms: number; path: string };

/** Frames of a source (id or file name) or a render (file name in renders/).
 *  Omit times for one representative frame (20% in). */
export const thumbnails = (
  dir: string,
  target: { source: string } | { render: string },
  opts: { times_ms?: number[]; at_ms?: number; width?: number } = {},
) => engineCall<{ status: "ok"; width: number; frames: ThumbFrame[] }>("media.thumbnail", { dir, ...target, ...opts });

// ---- outputs_cmd.rs (ui-outputs lane) --------------------------------------

/** One stored retention point (`{out_ms, retention}` percent). */
export type RetentionPoint = { out_ms: number; retention: number };

export type RetentionShow = {
  name: string;
  render: string;
  revision?: number;
  drop_min?: number;
  points: RetentionPoint[];
  drop_offs: DropOff[];
  render_exists: boolean;
  stale: boolean;
};

export const retentionShow = (dir: string, name: string) =>
  engineCall<RetentionShow>("retention.show", { dir, name });

export const rendersDelete = (dir: string, name: string) =>
  engineCall<{ status: string; removed: string[] }>("renders.delete", { dir, name });

export type QaReportRow = {
  render_hash: string;
  status: string;
  issues: number;
  checks?: { id?: string; status?: string; detail?: string }[];
};

export const qaList = (dir: string) => engineCall<{ reports: QaReportRow[] }>("qa.list", { dir });

// ---- Platform packages (package.build; crates/cutright-qa/src/package.rs) ----

export type PackagePlatform = "youtube" | "shorts" | "reels" | "tiktok";

/** `package.build` reply. `manifest` is `package-manifest.json` inside the package folder. */
export type PackageBuildResult = {
  status: "packaged";
  platform: PackagePlatform;
  manifest: string;
  files: { path: string; role: string; blake3: string; size_bytes: number }[];
  render: string;
};

/** `render` is a bare file name in renders/. A QA refusal arrives as an error starting `qa_failed:`. */
export const packageBuild = (
  dir: string,
  params: { platform: PackagePlatform; title: string; render?: string; description?: string; tags?: string[] },
) => engineCall<PackageBuildResult>("package.build", { dir, ...params });

// ---- Recipe run + per-source probe (recipe_cmd.rs) ----

export type SourceProbe = {
  status: string;
  source: string;
  duration_ms: number;
  width: number;
  height: number;
  fps?: { num: number; den: number } | null;
  video_codec?: string;
  video_profile?: string | null;
  color_space?: string | null;
  hdr?: string | null;
  has_audio: boolean;
  audio_codec?: string | null;
  audio_channels?: number | null;
  audio_sample_rate?: number | null;
  container?: string;
  rotation?: number;
};

export const sourceProbe = (dir: string, source: string) =>
  engineCall<SourceProbe>("source.probe", { dir, source });

export type RecipeStageEvent = {
  event: "recipe.stage";
  stage: string;
  state: "running" | "done" | "failed" | "skipped";
  fraction?: number;
  error?: string;
};

export type RecipeRunResult = {
  status: string;
  job: string;
  renders: { format: string; preset: string; status: string; output?: string }[];
};

export const recipeRun = (
  dir: string,
  opts: {
    style?: string;
    formats?: string[];
    sources?: string[];
    captions?: boolean;
    music?: string;
    music_level?: number;
    job?: string;
  },
  tag?: string,
) => engineCall<RecipeRunResult>("recipe.run", { dir, ...opts }, tag);

export const recipeCancel = (job: string) =>
  engineCall<{ job: string; cancelled: boolean }>("recipe.cancel", { job });

export type RecipeStatus = {
  ran: boolean;
  signature: Record<string, unknown>;
  done: Record<string, unknown>;
  finished_at?: string;
};

export const recipeStatus = (dir: string) =>
  engineCall<RecipeStatus>("recipe.status", { dir });

/** A caption cue mapped onto the current timeline (`captions.show`). */
export type CaptionCue = {
  start_ms: number;
  end_ms: number;
  lines: string[];
  text: string;
};

export const captionsShow = (dir: string, preset?: string) =>
  engineCall<{ status: string; preset: string; cues: CaptionCue[] }>("captions.show", {
    dir,
    ...(preset ? { preset } : {}),
  });

// Compatibility alias: editor lane imports `captionCues(dir)`.
export const captionCues = captionsShow;

/** One editorial plan item/take mapped through the live timeline. */
export type PlanItem = {
  id: string;
  kind: string;
  tier?: string;
  confidence?: number;
  start_ms: number;
  end_ms: number;
  reason: string;
  text?: string;
  action?: { op?: string; first?: number; end?: number; start_ms?: number; end_ms?: number };
  take?: { group?: string; index?: number };
  covered_by?: string | null;
  /** Output ranges this source window still occupies ([] = already cut). */
  out?: { start_ms: number; end_ms: number }[];
  /** Output position to seek to for this item. */
  at_ms?: number;
  /** Plan this item belongs to (editor lane). */
  plan_id?: string;
  /** Currently chosen take index when alternates exist. */
  candidate_take?: number;
  /** Alternate takes for retake-style items. */
  alternates?: PlanTake[];
};

export type PlanTake = {
  index: number;
  item_id?: string;
  start_ms: number;
  end_ms: number;
  text: string;
  rank?: number;
  chosen?: boolean;
  out?: { start_ms: number; end_ms: number }[];
  at_ms?: number;
};

export type StoredPlan = {
  id?: string;
  source?: string;
  source_blake3?: string;
  source_duration_ms?: number;
  items?: PlanItem[];
  take_groups?: { id: string; chosen: number; takes: PlanTake[] }[];
};

export const editplanList = (dir: string) =>
  engineCall<{ status: string; plans: StoredPlan[] }>("editplan.list", { dir });

// Compatibility alias for consumers that imported the camelCase name.
export const editPlanList = editplanList;
export type EditPlan = StoredPlan;
export type EditPlanItem = PlanItem;

// ---- Channel recipes (recipes.*: app-wide library, per-project ref) ----

/** Engine id rule for library recipes: lowercase letters, digits and '-', 1..64 chars. */
export const RECIPE_ID_RULE = /^[a-z0-9-]{1,64}$/;

export type RecipeSummary = {
  id: string;
  name: string;
  version: number;
  updated_at: string;
  body_blake3: string;
};

export type RecipeListResult = { recipes: RecipeSummary[]; invalid?: unknown[] };

export type RecipeDocument = RecipeSummary & {
  schema: string;
  body: Record<string, unknown>;
};

/** The library recipe this project last applied. */
export type RecipeRef = { id: string; version: number; body_blake3: string; applied_at: string };

export type RecipeStatusResult = {
  ref: RecipeRef | null;
  /** The project's recipe settings were edited after the apply. */
  overridden: boolean;
  update_available: boolean;
  library_version: number | null;
};

export const recipesList = () => engineCall<RecipeListResult>("recipes.list", {});

export const recipesGet = (id: string) => engineCall<RecipeDocument>("recipes.get", { id });

export const recipesSave = (args: {
  id: string;
  name: string;
  body: Record<string, unknown>;
  expect_version?: number;
}) => engineCall<RecipeDocument>("recipes.save", { ...args });

export const recipesDelete = (id: string) => engineCall<unknown>("recipes.delete", { id });

/** Saves this project's recipe settings to the library under `id`; a new version when `id` exists. */
export const recipesSaveFromProject = (args: { dir: string; id: string; name: string }) =>
  engineCall<RecipeDocument>("recipes.save_from_project", { ...args });

/** Replaces this project's recipe settings with the library recipe `id`. */
export const recipesApply = (dir: string, id: string) => engineCall<unknown>("recipes.apply", { dir, id });

export const recipesStatus = (dir: string) => engineCall<RecipeStatusResult>("recipes.status", { dir });

// ---- Batch delivery (batch.*: one project's timelines x presets) ----

export type BatchPreset = "youtube" | "reels" | "tiktok" | "square";
export type BatchState = "pending" | "rendered" | "qualified" | "failed";
export type BatchCover = { at_ms: number; path: string; blake3: string };

/** One requested output. Omit `variant` for the main timeline. */
export type BatchOutputSpec = { variant?: string; preset: BatchPreset };

export type BatchOutputRow = {
  key: string;
  variant: string | null;
  preset: string;
  qa_preset: string;
  state: BatchState;
  output: string | null;
  output_blake3: string | null;
  qa: { status: string; receipt: string } | null;
  cover: BatchCover | null;
  error: string | null;
  attempts: number;
};

export type BatchDocument = {
  schema: string;
  id: string;
  created_at: string;
  timeline_cursor: number;
  outputs: BatchOutputRow[];
};

export type BatchRunOutput = Pick<BatchOutputRow, "key" | "state" | "output" | "cover" | "error">;

/** `batch.status`: the batch document plus its finishing check against the project's settings now. */
export type BatchStatus = BatchDocument & {
  /** Lowercase hex BLAKE3 of the finishing settings the batch was created under; null before pinning existed. */
  finishing_blake3: string | null;
  /** null when the batch is unpinned or the settings could not be read (see `finishing_error`). */
  finishing_matches: boolean | null;
  /** Setting names that differ from the pinned settings, in the engine's order; empty when none. */
  finishing_differs: string[];
  current_finishing_blake3: string | null;
  finishing_error?: string;
};

export type BatchRunResult = {
  status: "done" | "partial";
  id: string;
  outputs: BatchRunOutput[];
  /** Output keys rendered by this run. */
  ran: string[];
  /** Output keys already delivered and kept as they were. */
  skipped: string[];
  /** How this run treated the batch's pinned finishing settings. */
  finishing?: {
    action: "match" | "pinned" | "repinned";
    differs: string[];
    note?: string;
    finishing_blake3: string;
    previous_finishing_blake3: string | null;
  };
};

export type BatchListResult = { batches: { id: string; counts?: Record<string, number> }[] };

/** `batch.output` progress event emitted by `batch.run` on the run's tag; only `key` and `state` are guaranteed. */
export type BatchOutputEvent = {
  event: "batch.output";
  key: string;
  state: BatchState;
  output?: string | null;
  cover?: BatchCover | null;
  error?: string | null;
  attempts?: number;
};

export const batchCreate = (dir: string, outputs: BatchOutputSpec[], id?: string) =>
  // The engine replies {status, created, id, batch}; callers want the batch document.
  engineCall<{ batch: BatchDocument }>("batch.create", { dir, outputs, ...(id ? { id } : {}) }).then((r) => r.batch);

/**
 * Long-running: pass `job` to cancel with `recipeCancel(job)`; progress arrives as `batch.output` on `tag`.
 * `finishing: "current"` pins the batch to the project's current finishing settings and renders every output again.
 */
export const batchRun = (dir: string, id: string, job?: string, tag?: string, finishing?: "current") =>
  engineCall<BatchRunResult>(
    "batch.run",
    { dir, id, ...(job ? { job } : {}), ...(finishing ? { finishing } : {}) },
    tag,
  );

export const batchStatus = (dir: string, id: string) => engineCall<BatchStatus>("batch.status", { dir, id });

export const batchList = (dir: string) => engineCall<BatchListResult>("batch.list", { dir });

/** Sets the cover frame of one output; the reply carries the updated fields of that output row. */
export const batchCover = (dir: string, id: string, key: string, at_ms: number) =>
  engineCall<Partial<BatchOutputRow> & { key: string }>("batch.cover", { dir, id, key, at_ms });

// ---- Portable project bundle (bundle_cmd.rs) ----

/** `collected` copies each source into the bundle; `referenced` lists it by original path. */
export type BundleMedia = "collected" | "referenced";

/** One `external` entry of an export reply. `source` = referenced media; `asset` = a font/LUT that was missing. */
export type BundleExternal = {
  kind: "source" | "asset";
  path: string;
  id?: string;
  name?: string;
  size?: number;
  blake3?: string;
};

/** `bundle.export` reply (`status: "exported"`). `members` counts the project files, not `bundle.json`. */
export type BundleExportResult = {
  status: "exported";
  out: string;
  manifest: string;
  manifest_blake3: string;
  media: BundleMedia;
  project_id: string;
  members: number;
  bytes: number;
  external: BundleExternal[];
  settings_assets: { member: string; original: string }[];
};

/** A referenced source the bundle names that is absent here, or no longer hashes to its BLAKE3. */
export type BundleMissingMedia = {
  id: string | null;
  name: string | null;
  path: string;
  reason: string;
};

/** A generated source the bundle names: `missing` when its bound file is absent here,
 *  `unverified` when present here with a SHA-256 other than the one it was bound to. */
export type BundleGeneratedSourceNote = {
  job_id: string;
  path: string;
  reason: string;
};

/** `bundle.import` reply (`status: "imported"`). `dir` is the new project folder. */
export type BundleImportResult = {
  status: "imported";
  dir: string;
  project_id: string;
  media: BundleMedia;
  members: number;
  bytes: number;
  imported: string[];
  missing_media: BundleMissingMedia[];
  missing_assets: string[];
  /** Present on this computer, but the bundle recorded no hash to check them against. */
  unverified_assets?: string[];
  /** Generated sources whose bound file is absent on this computer. */
  missing_generated_sources?: BundleGeneratedSourceNote[];
  /** Generated sources whose bound file is here but is not the file they were bound to. */
  unverified_generated_sources?: BundleGeneratedSourceNote[];
};

/** `tag` routes the `bundle.progress` frames (`{completed, total}`, write stage) to this call. */
export const bundleExport = (
  dir: string,
  params: { out: string; media: BundleMedia },
  tag?: string,
) => engineCall<BundleExportResult>("bundle.export", { dir, ...params }, tag);

export const bundleImport = (params: { bundle: string; dir: string }) =>
  engineCall<BundleImportResult>("bundle.import", params);
