import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { engineCall, errorCode, errorMessage } from "../../lib/engine.js";
import { projectShow, type ProjectShow } from "../../lib/project.js";
import { assetUrl } from "../../lib/asset.js";
import { captionsPreview, DEFAULT_CAPTION_STYLE, framingApproval, modelsStatus, recipeCancel, recipeRun, recipeStatus, recipesApply, recipesSave, settingsGet, settingsSet, sourceList, thumbnails, type CaptionPreview, type RecipeStageEvent, type RecipeStatus, type SourceRow } from "../../lib/media.js";
import { interpretAssetLibrary, type GeneratedAsset } from "../lib/genright.js";
import { onEngineRaw } from "../lib/engineRaw.js";
import { pickFile } from "../lib/dialog.js";
import { framingReadFailure, readCurrentFraming, useFramingPlanChanges, type FramingPlanView } from "./useVideoFramingReview.js";

export type VideoRecipe = {
  style: "natural" | "tight" | "punchy"; formats: string[];
  captions: { on: boolean; style: string; [key: string]: unknown };
  finish: { color: string; audio: string; music: string | null; music_level: number; lut_path: string | null; lut_intensity: number; match_shots: boolean; [key: string]: unknown };
  sources: string[];
  [key: string]: unknown;
};
type Stage = { state: "running" | "done" | "failed" | "skipped"; fraction?: number; error?: string };
type Operation = { kind: "run" | "apply" | "save"; job?: string; tag?: string; phase?: "preparing" | "running" | "refreshing"; cancelling?: boolean; registered?: boolean; cancelRequest?: () => void; stages?: Record<string, Stage>; warning?: string; done?: Promise<void> };
type RunResult = { stages: Record<string, Stage>; error: string | null; cancelled: boolean; refreshError?: string };
type RecipeDraft = { recipe: Record<string, unknown>; overrides: Record<string, unknown> };
const results = new Map<string, RunResult>();
const operations = new Map<string, Operation>();
const operationListeners = new Set<() => void>();
let operationSnapshot: ReadonlyMap<string, Operation> = new Map();
const subscribeOperations = (listener: () => void) => { operationListeners.add(listener); return () => { operationListeners.delete(listener); }; };
function publishOperations() { operationSnapshot = new Map(operations); for (const listener of operationListeners) listener(); }
const writes = new Map<string, Promise<void>>();
export const videoRecipePlanKey = (id: string, aspect: string) => `${id}|${aspect}`;

/** Recipe state and engine interactions; presentation and notification copy stay with the host. */
export function useVideoRecipe({ dir, onFinished, refreshChangedProject }: {
  dir: string;
  onFinished: (workspaceRefreshed: boolean) => void;
  refreshChangedProject: (dir: string) => Promise<void>;
}) {
  const [sources, setSources] = useState<SourceRow[]>([]);
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [recipe, setRecipe] = useState<VideoRecipe | null>(null);
  const [thumbs, setThumbs] = useState<Record<string, string>>({});
  const [plans, setPlans] = useState<Record<string, FramingPlanView>>({});
  const [autoFraming, setAutoFraming] = useState(false);
  const [music, setMusic] = useState<GeneratedAsset[]>([]);
  const [speechReady, setSpeechReady] = useState<boolean | null>(null);
  const [swatch, setSwatch] = useState<CaptionPreview | "loading" | "error" | null>(null);

  const operation = useSyncExternalStore(subscribeOperations, () => operationSnapshot, () => operationSnapshot).get(dir) ?? null;
  const running = operation?.kind === "run" ? operation.job ?? null : null;
  const observedOperation = useRef<{ dir: string; operation: Operation | null }>({ dir, operation });
  const [framingFor, setFramingFor] = useState<{ dir: string; s: SourceRow; aspect: string } | null>(null);
  const result = results.get(dir);
  const stages = operation?.kind === "run" ? operation.stages ?? {} : result?.stages ?? {};
  const runError = operation?.warning ?? result?.error ?? null;
  const cancelled = result?.cancelled ?? false;
  const canCancel = operation?.kind === "run" && operation.phase !== "refreshing" && !operation.cancelling;
  const cancelling = operation?.kind === "run" && operation.phase !== "refreshing" && !!operation.cancelling;
  const [lastRun, setLastRun] = useState<RecipeStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const owner = useRef<{ dir: string; reads: number; framingGeneration: number; draft: RecipeDraft | null; recipe: VideoRecipe | null; timer: ReturnType<typeof setTimeout> | null } | null>(null);
  useLayoutEffect(() => {
    const token = { dir, reads: 0, framingGeneration: 0, draft: null as RecipeDraft | null, recipe: null as VideoRecipe | null, timer: null as ReturnType<typeof setTimeout> | null };
    owner.current = token;
    setSources([]); setShow(null); setRecipe(null); setThumbs({}); setPlans({}); setMusic([]); setSwatch(null); setError(null); setSpeechReady(null); setAutoFraming(false); setLastRun(null); setFramingFor(null);
    return () => {
      if (token.timer) { clearTimeout(token.timer); token.timer = null; void enqueue(token).catch(() => undefined); }
      if (owner.current === token) owner.current = null;
    };
  }, [dir]);
  const enqueue = async (token: NonNullable<typeof owner.current>) => {
    const previous = writes.get(token.dir) ?? Promise.resolve();
    const draft = token.draft;
    if (!draft) return previous;
    const next = previous.catch(() => undefined).then(async () => {
      const { project } = await settingsGet(token.dir);
      const stored = (project.recipe ?? {}) as Record<string, unknown>;
      const recipe = { ...stored, ...draft.recipe };
      if (draft.recipe.finish) recipe.finish = { ...stored.finish as object, ...draft.recipe.finish as object };
      if (draft.recipe.captions) {
        const captions = typeof stored.captions === "boolean" ? { on: stored.captions } : stored.captions;
        recipe.captions = { ...captions as object, ...draft.recipe.captions as object };
      }
      await settingsSet({ recipe, ...draft.overrides }, token.dir);
      if (token.draft === draft) token.draft = null;
      if (owner.current === token) setError(null);
    });
    writes.set(token.dir, next);
    void next.finally(() => { if (writes.get(token.dir) === next) writes.delete(token.dir); }).catch(() => undefined);
    return next;
  };
  const flush = async () => {
    const token = owner.current;
    if (!token || token.dir !== dir || !token.recipe) return;
    if (token.timer) { clearTimeout(token.timer); token.timer = null; }
    await enqueue(token);
  };
  const refresh = useCallback(async (reload = false, applying = false) => {
    const token = owner.current;
    if (!token || token.dir !== dir) return;
    const generation = ++token.reads;
    await writes.get(dir)?.catch(() => undefined);
    const pending = operations.get(dir);
    if (!applying && pending?.kind === "apply" && pending.done) await pending.done.catch(() => undefined);
    if (owner.current !== token || token.reads !== generation) return;
    const [list, project, settings, models, status] = await Promise.all([sourceList(dir), projectShow(dir).catch(() => null), settingsGet(dir), modelsStatus().catch(() => null), recipeStatus(dir).catch(() => null)]);
    if (owner.current !== token) return;
    if (token.reads !== generation) {
      if (reload) throw new Error("Project or recipe state changed during reload");
      return;
    }
    token.framingGeneration += 1;
    setSources(list.sources as SourceRow[]); setShow(project); setAutoFraming(settings ? framingApproval(settings) === "auto" : false); setPlans({});
    setSpeechReady(models?.speech?.ready ?? null); setLastRun(status);
    if (!token.recipe || reload) {
      const { project: projectSettings, app: appSettings } = settings;
      const saved = (projectSettings.recipe ?? {}) as Partial<VideoRecipe> & { music?: string | null; music_level?: number };
      const rawCaptions = saved.captions as VideoRecipe["captions"] | boolean | undefined;
      const captionFields: Partial<VideoRecipe["captions"]> & { enabled?: boolean } = typeof rawCaptions === "object" && rawCaptions !== null ? rawCaptions : {};
      const captionEnabled = typeof rawCaptions === "boolean" ? rawCaptions : captionFields.on ?? captionFields.enabled;
      const finishing = <T,>(key: string, stored: T | undefined | null, fallback: T): T =>
        (projectSettings[key] as T | undefined | null) ?? stored ?? (appSettings[key] as T | undefined | null) ?? fallback;
      const next: VideoRecipe = {
        ...saved,
        style: saved.style ?? (projectSettings.cut_preset as VideoRecipe["style"]) ?? "natural",
        formats: saved.formats?.length ? saved.formats : ["16:9"],
        captions: {
          ...captionFields,
          on: finishing("burn_captions", captionEnabled, true),
          style: finishing("caption_style", captionFields.style || undefined, DEFAULT_CAPTION_STYLE) || DEFAULT_CAPTION_STYLE,
        },
        finish: {
          ...saved.finish,
          color: finishing("color_preset", saved.finish?.color, "neutral"),
          audio: finishing("audio_preset", saved.finish?.audio, "talking-head"),
          music: saved.finish?.music ?? saved.music ?? null,
          music_level: saved.finish?.music_level ?? saved.music_level ?? -24,
          lut_path: finishing("lut_path", saved.finish?.lut_path, null),
          lut_intensity: finishing("lut_intensity", saved.finish?.lut_intensity, 100),
          match_shots: finishing("match_shots", saved.finish?.match_shots, true),
        },
        sources: saved.sources ?? list.sources.map((s) => s.id),
      };
      token.draft = null;
      token.recipe = next; setRecipe(next);
    }
  }, [dir]);
  useEffect(() => { const token = owner.current; void refresh().catch((e) => { if (owner.current === token) setError(errorMessage(e)); }); }, [refresh]);
  useEffect(() => {
    const previous = observedOperation.current;
    observedOperation.current = { dir, operation };
    if (previous.dir === dir && previous.operation?.kind === "run" && !operation) {
      const token = owner.current;
      void refresh().catch((e) => { if (owner.current === token && token?.dir === dir) setError(errorMessage(e)); });
    }
  }, [dir, operation, refresh]);
  const patch = (change: Partial<VideoRecipe>, overrides?: Record<string, unknown>) => {
    const token = owner.current;
    if (!token || token.dir !== dir || !token.recipe || operations.has(dir)) return;
    const current = token.recipe;
    const next = { ...current, ...change };
    const fields: Record<string, unknown> = { ...token.draft?.recipe, ...change };
    for (const key of ["captions", "finish"] as const) {
      const edits = change[key];
      if (edits) fields[key] = {
        ...token.draft?.recipe[key] as object,
        ...Object.fromEntries(Object.entries(edits).filter(([field, value]) => value !== current[key][field])),
      };
    }
    token.draft = { recipe: fields, overrides: { ...token.draft?.overrides, ...overrides } };
    token.recipe = next;
    setRecipe(next);
    if (token.timer) clearTimeout(token.timer);
    token.timer = setTimeout(() => { token.timer = null; void enqueue(token).catch((e) => { if (owner.current === token) setError(errorMessage(e)); }); }, 400);
  };
  useEffect(() => {
    const token = owner.current;
    if (!token || token.dir !== dir) return;
    let active = true;
    for (const source of sources) void thumbnails(dir, { source: source.id }, { width: 240 }).then((r) => {
      if (active && owner.current === token) setThumbs((old) => ({ ...old, [source.id]: r.frames[0] ? assetUrl(r.frames[0].path) : "" }));
    }).catch(() => { if (active && owner.current === token) setThumbs((old) => ({ ...old, [source.id]: "" })); });
    return () => { active = false; };
  }, [dir, sources]);
  useEffect(() => {
    const token = owner.current;
    void engineCall("assets.list", { dir }).then((r) => { if (token && owner.current === token) setMusic(interpretAssetLibrary(r).assets.filter((a) => a.kind === "music" && a.exists)); }).catch(() => { if (token && owner.current === token) setMusic([]); });
  }, [dir]);
  const swatchPreset = recipe?.formats.includes("9:16") ? "reels" : recipe?.formats.includes("1:1") ? "square" : "youtube";
  const captionStyle = recipe?.captions.on ? recipe.captions.style : null;
  const captionSource = recipe && sources.find((source) => recipe.sources.includes(source.id))?.id;
  const hasSources = sources.length > 0;
  useEffect(() => {
    const token = owner.current; let live = true;
    if (captionStyle === null || !hasSources) { setSwatch(null); return; }
    setSwatch("loading");
    captionsPreview(dir, { style: captionStyle, preset: swatchPreset, source: captionSource ?? undefined })
      .then((r) => { if (live && owner.current === token) setSwatch(r); }).catch(() => { if (live && owner.current === token) setSwatch("error"); });
    return () => { live = false; };
  }, [dir, captionStyle, captionSource, hasSources, swatchPreset]);
  const selectedSources = useMemo(() => sources.filter((s) => recipe?.sources.includes(s.id)), [sources, recipe]);
  const reframeAspects = useMemo(() => (recipe?.formats ?? []).filter((f) => f !== "16:9"), [recipe]);
  const needsReframe = useCallback((source: SourceRow, aspect: string) => {
    const video = show?.timeline.sources.find((t) => t.id === source.id)?.video;
    if (!video) return false;
    const [w, h] = aspect.split(":").map(Number);
    return Math.abs(video.width / video.height / (w / h) - 1) >= 0.05;
  }, [show]);
  useFramingPlanChanges(dir, (source, aspect, plan) => setPlans((old) => ({ ...old, [videoRecipePlanKey(source, aspect)]: plan })));
  useEffect(() => {
    const token = owner.current;
    for (const source of sources) for (const aspect of reframeAspects) {
      const key = videoRecipePlanKey(source.id, aspect);
      if (!needsReframe(source, aspect) || plans[key] !== undefined) continue;
      setPlans((old) => ({ ...old, [key]: "loading" }));
      const generation = token?.framingGeneration;
      void readCurrentFraming(dir, source.id, aspect, (value) => { if (token && owner.current === token && token.framingGeneration === generation) setPlans((old) => ({ ...old, [key]: value })); }, (e) => { if (token && owner.current === token && token.framingGeneration === generation) setPlans((old) => ({ ...old, [key]: framingReadFailure(e, source.id, aspect) })); });
    }
  }, [dir, sources, reframeAspects, needsReframe, plans]);
  const framingPairs = selectedSources.flatMap((s) => reframeAspects.filter((aspect) => needsReframe(s, aspect)).map((aspect) => ({ s, aspect })));
  const unapproved = framingPairs.filter(({ s, aspect }) => { const plan = plans[videoRecipePlanKey(s.id, aspect)]; return !(typeof plan === "object" && plan.approved); });
  const run = async () => {
    const token = owner.current;
    if (!token || token.dir !== dir || !token.recipe || !token.recipe.sources.length || !token.recipe.formats.length || operations.has(dir)) return;
    const value = token.recipe;
    const job = `recipe-ui-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
    const tag = `recipe-${job}`;
    const record: Operation = { kind: "run", job, tag, phase: "preparing", stages: {} };
    operations.set(dir, record);
    results.delete(dir);
    publishOperations();
    let off: (() => void) | undefined;
    const attemptCancel = async () => {
      try {
        const response = await recipeCancel(job);
        if (response.cancelled || record.phase !== "running" || operations.get(dir) !== record) return;
        record.warning = "Cancellation was not accepted";
      } catch (e) {
        if (record.phase !== "running" || operations.get(dir) !== record) return;
        record.warning = errorMessage(e);
      }
      record.cancelling = false;
      publishOperations();
    };
    try {
      await flush();
      if (value.captions.on) {
        const { app, project } = await settingsGet(dir);
        const stored = (project.recipe ?? {}) as { captions?: Partial<VideoRecipe["captions"]> | boolean };
        const captions = typeof stored.captions === "object" && stored.captions !== null ? stored.captions : {};
        const configuredStyle = project.caption_style ?? (captions.style || app.caption_style);
        if (!configuredStyle && !record.cancelling) {
          await settingsSet({ caption_style: value.captions.style }, dir);
        }
      }
      record.cancelRequest = () => { void attemptCancel(); };
      off = await onEngineRaw((payload) => {
        const frame = payload as unknown as { tag?: string | null; event?: RecipeStageEvent };
        if (operations.get(dir) !== record || frame.tag !== record.tag || frame.event?.event !== "recipe.stage") return;
        const event = frame.event;
        const firstStage = !record.registered;
        record.registered = true;
        record.stages = { ...record.stages, [event.stage]: { state: event.state, fraction: event.fraction, error: event.error } };
        publishOperations();
        if (firstStage && record.cancelling) void attemptCancel();
      });
      if (record.cancelling) {
        results.set(dir, { stages: {}, error: null, cancelled: true });
        return;
      }
      record.phase = "running";
      publishOperations();
      await recipeRun(dir, { style: value.style, formats: value.formats, sources: value.sources, captions: value.captions.on, music: value.finish.music ?? undefined, music_level: value.finish.music_level, job }, tag);
      record.phase = "refreshing";
      record.warning = undefined;
      publishOperations();
      let refreshError: string | undefined;
      try {
        await refreshChangedProject(dir);
      } catch (e) {
        refreshError = errorMessage(e);
      }
      results.set(dir, { stages: record.stages ?? {}, error: null, cancelled: false, refreshError });
      if (owner.current === token) {
        try { onFinished(refreshError === undefined); } catch (e) {
          results.set(dir, { stages: record.stages ?? {}, error: null, cancelled: false, refreshError: errorMessage(e) });
        }
      }
    } catch (e) {
      const code = errorCode(e);
      const wasCancelled = (record.phase === "preparing" && record.cancelling) || code === "cancelled" || code === "render_cancelled";
      results.set(dir, { stages: record.stages ?? {}, error: wasCancelled ? null : errorMessage(e), cancelled: wasCancelled });
    } finally {
      if (operations.get(dir) === record) operations.delete(dir);
      publishOperations();
      off?.();
    }
  };
  const applyLibraryRecipe = async (id: string) => {
    const token = owner.current;
    if (!token || token.dir !== dir || !token.recipe || operations.has(dir)) throw new Error("Recipe operation unavailable or busy");
    let settle!: () => void;
    const done = new Promise<void>((resolve) => { settle = resolve; });
    operations.set(dir, { kind: "apply", done });
    publishOperations();
    try {
      await flush();
      await recipesApply(dir, id);
      if (owner.current === token) {
        try {
          await refresh(true, true);
        } catch (e) {
          if (owner.current === token) {
            token.recipe = null;
            setRecipe(null);
            setError(errorMessage(e));
          }
        }
      }
    } finally {
      operations.delete(dir);
      settle();
      publishOperations();
    }
  };
  const saveLibraryRecipe = async (id: string, name: string) => {
    const token = owner.current;
    if (!token || token.dir !== dir || !token.recipe || operations.has(dir)) throw new Error("Recipe operation unavailable or busy");
    const { sources: _projectSources, ...recipe } = token.recipe;
    operations.set(dir, { kind: "save" }); publishOperations();
    try {
      await flush();
      const body: Record<string, unknown> = {
        ...recipe,
        burn_captions: recipe.captions.on,
        caption_style: recipe.captions.style,
        color_preset: recipe.finish.color,
        audio_preset: recipe.finish.audio,
        lut_path: recipe.finish.lut_path,
        lut_intensity: recipe.finish.lut_intensity,
        match_shots: recipe.finish.match_shots,
      };
      const { project } = await settingsGet(dir);
      if (project.grade != null) body.grade = project.grade;
      await recipesSave({ id, name, body });
    } finally { operations.delete(dir); publishOperations(); }
  };
  const cancel = async () => {
    const record = operations.get(dir);
    if (record?.kind !== "run" || !record.job || record.phase === "refreshing" || record.cancelling) return;
    record.cancelling = true;
    record.warning = undefined;
    publishOperations();
    if (record.registered) record.cancelRequest?.();
  };
  const setColor = (color: string) => {
    const token = owner.current;
    if (!token?.recipe || token.dir !== dir || operations.has(dir)) return;
    const lutCleared = color === "custom" ? {} : { lut_path: null };
    patch({ finish: { ...token.recipe.finish, color, ...lutCleared } }, { color_preset: color, ...lutCleared });
  };
  const setAudio = (audio: string) => {
    const token = owner.current;
    if (!token?.recipe || token.dir !== dir || operations.has(dir)) return;
    patch({ finish: { ...token.recipe.finish, audio } }, { audio_preset: audio });
  };
  const pickLut = async (title: string) => {
    const token = owner.current;
    if (!token || token.dir !== dir || operations.has(dir)) return;
    try {
      const value = await pickFile(title, ["cube"]);
      if (value && owner.current === token && token.recipe?.finish.color === "custom" && !operations.has(dir)) {
        patch({ finish: { ...token.recipe.finish, lut_path: value } }, { lut_path: value });
      }
    } catch (e) {
      if (owner.current === token) setError(errorMessage(e));
    }
  };
  const toggleFormat = (format: string) => {
    const value = owner.current?.recipe;
    if (!value) return;
    const formats = value.formats.includes(format) ? value.formats.filter((f) => f !== format) : [...value.formats, format];
    if (formats.length) patch({ formats });
  };
  const setSourceIncluded = (id: string, included: boolean) => {
    const value = owner.current?.recipe;
    if (!value) return;
    patch({ sources: included ? [...new Set([...value.sources, id])] : value.sources.filter((source) => source !== id) });
  };
  const openFraming = (s: SourceRow, aspect: string) => {
    if (owner.current?.dir === dir && !operations.has(dir)) setFramingFor({ dir, s, aspect });
  };
  const closeFraming = () => setFramingFor(null);
  const framingChanged = (source: string, aspect: string, plan: FramingPlanView) => {
    if (owner.current?.dir === dir && framingFor?.dir === dir && framingFor.s.id === source && framingFor.aspect === aspect) setPlans((old) => ({ ...old, [videoRecipePlanKey(source, aspect)]: plan }));
  };
  const setStyle = (style: VideoRecipe["style"]) => patch({ style });
  const setCaptionsEnabled = (on: boolean) => {
    const value = owner.current?.recipe;
    if (value) patch({ captions: { ...value.captions, on } }, { burn_captions: on });
  };
  const setCaptionStyle = (style: string) => {
    const value = owner.current?.recipe;
    if (value) patch({ captions: { ...value.captions, style } }, { caption_style: style });
  };
  const setMusic = (music: string) => {
    const value = owner.current?.recipe;
    if (value) patch({ music: undefined, finish: { ...value.finish, music: music || null } });
  };
  const setMusicLevel = (music_level: number) => { const value = owner.current?.recipe; if (value) patch({ finish: { ...value.finish, music_level } }); };
  const setLutIntensity = (lut_intensity: number) => {
    const value = owner.current?.recipe;
    if (value) patch({ finish: { ...value.finish, lut_intensity } }, { lut_intensity });
  };
  const setMatchShots = (match_shots: boolean) => {
    const value = owner.current?.recipe;
    if (value) patch({ finish: { ...value.finish, match_shots } }, { match_shots });
  };
  const retryLoad = async () => {
    const token = owner.current;
    if (!token || token.dir !== dir || operations.has(dir)) return;
    setError(null);
    try { await refresh(); } catch (e) { if (owner.current === token) setError(errorMessage(e)); }
  };
  const canRun = !!recipe && recipe.sources.length > 0 && recipe.formats.length > 0 && !operation;
  return { sources, recipe, thumbs, plans, autoFraming, music, speechReady, swatch, swatchPreset, stages, running, runError, cancelled, lastRun, error, refreshError: result?.refreshError, reframeAspects, framingPairs, unapproved, planKey: videoRecipePlanKey, retryLoad, run, cancel, pickLut, setColor, setAudio, applyLibraryRecipe, saveLibraryRecipe, canRun, canCancel, cancelling, setStyle, setCaptionsEnabled, setCaptionStyle, setMusic, setMusicLevel, setLutIntensity, setMatchShots, toggleFormat, setSourceIncluded, framingFor, openFraming, closeFraming, framingChanged };
}
