import { Badge, Button, Card, Checkbox, EmptyState, Field, PageShell, SegmentedControl, Select, Slider, Toggle, useToast } from "@rightkit/react-shell/react";
import "./Recipe.css";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { engineCall, onEngineRaw, errorCode, errorMessage } from "../lib/engine";
import { assetUrl } from "../lib/asset";
import { interpretAssetLibrary, type GeneratedAsset } from "../lib/genright";
import { pickFile } from "../lib/dialog";
import { baseName, projectShow, type ProjectShow } from "../lib/project";
import {
  type CaptionPreview,
  type RecipeStageEvent,
  type RecipeStatus,
  type SourceRow,
  CAPTION_STYLES,
  DEFAULT_CAPTION_STYLE,
  captionsPreview,
  modelsStatus,
  recipeCancel,
  recipeRun,
  recipeStatus,
  reframeAnalyze,
  reframeApprove,
  reframeShow,
  framingApproval,
  framingLabel,
  settingsGet,
  settingsSet,
  sourceList,
  thumbnails,
  type ReframeSummary,
} from "../lib/media";
import { Drawer, ErrorPanel, ProgressRow, Thumbnail, timecode, Icon } from "../ui";
import { RecipeLibrary } from "./RecipeLibrary";
import { HDR_COLOR_PRESETS, HDR_UNAVAILABLE_NOTICE, useHdrToSdr } from "../hooks/useHdrToSdr";

type Style = "natural" | "tight" | "punchy";
type Stage = { state: "running" | "done" | "failed" | "skipped"; fraction?: number; error?: string };

type Recipe = {
  style: Style;
  formats: string[];
  captions: { on: boolean; style: string };
  finish: {
    color: string;
    audio: string;
    music: string | null;
    music_level: number;
    lut_path: string | null;
    lut_intensity: number;
    match_shots: boolean;
  };
  sources: string[];
};

const STYLE_DESC: Record<Style, string> = {
  natural: "Keeps the room to breathe — removes only mistakes and dead air.",
  tight: "Cuts pauses, fillers and retakes for a brisk, conversational pace.",
  punchy: "Aggressive trims and fast beat changes for short-form energy.",
};

const FORMATS = ["16:9", "9:16", "1:1"];
/**
 * The app's default music bed level (dB) for a recipe that has none saved. The engine's
 * fallback for a recipe with no `music_level` is also -24 dB (recipe_cmd.rs, `unwrap_or(-24.0)`),
 * so the app and engine defaults match (#75).
 */
const DEFAULT_MUSIC_LEVEL_DB = -24;
const COLOR_OPTS: [string, string][] = [
  ["", "None"],
  ["neutral", "Neutral Rec.709"],
  ["hdr-natural", "HDR natural"],
  ["hdr-soft", "HDR soft"],
  ["apple-log-natural", "Apple Log natural"],
  ["talking-head-auto", "Talking head (auto)"],
  ["warm", "Warm"],
  ["cool", "Cool"],
  ["lift", "Lift"],
  ["custom", "Custom LUT (.cube)"],
];
/** Colour options; with HDR to SDR unavailable the HDR presets stay selectable but say so. */
const colourOpts = (hdr: boolean | undefined): [string, string][] =>
  COLOR_OPTS.map(([v, l]): [string, string] =>
    hdr === false && (HDR_COLOR_PRESETS as readonly string[]).includes(v) ? [v, `${l} — not available on this computer`] : [v, l],
  );
const AUDIO_OPTS: [string, string][] = [
  ["", "None"],
  ["talking-head", "Talking head"],
  ["podcast", "Podcast"],
  ["noisy-room", "Noisy room"],
];

const STAGE_LABELS: [string, string][] = [
  ["transcribe", "Transcribe speech"],
  ["analyse", "Analyse visuals"],
  ["takes", "Choose best takes"],
  ["cut", "Cut"],
  ["captions", "Captions"],
  ["framing", "Framing"],
  ["colour", "Colour"],
  ["audio", "Audio"],
  ["music", "Music"],
];

type AnchorView = { mid_ms: number; approved: boolean; crop: { x: number; y: number; w: number; h: number } };

function anchorViews(plan: ReframeSummary): AnchorView[] {
  const p = (plan.plan ?? {}) as {
    anchors?: { start_ms?: number; end_ms?: number; approved?: boolean }[];
    keyframes?: { t_ms?: number; x?: number; y?: number }[];
    source_width?: number;
    source_height?: number;
  };
  const anchors = p.anchors ?? [];
  const keyframes = p.keyframes ?? [];
  const [sw, sh] = [p.source_width ?? 0, p.source_height ?? 0];
  const [cw, ch] = plan.crop ?? [0, 0];
  const out: AnchorView[] = [];
  const step = Math.max(1, Math.floor(anchors.length / 6) || 1);
  for (let i = 0; i < anchors.length && out.length < 6; i += step) {
    const a = anchors[i];
    const mid = Math.round(((a.start_ms ?? 0) + (a.end_ms ?? 0)) / 2);
    let k = keyframes[0];
    for (const cand of keyframes) {
      if (Math.abs((cand.t_ms ?? 0) - mid) < Math.abs((k?.t_ms ?? 0) - mid)) k = cand;
    }
    out.push({
      mid_ms: mid,
      approved: a.approved === true,
      crop: {
        x: sw ? (k?.x ?? 0) / sw : 0,
        y: sh ? (k?.y ?? 0) / sh : 0,
        w: sw ? cw / sw : 0,
        h: sh ? ch / sh : 0,
      },
    });
  }
  return out;
}

/** §3.3 Edit recipe: persisted form + recipe.run stage panel + framing review drawer. */
const SWATCH_ASPECT = { youtube: "16 / 9", reels: "9 / 16", square: "1 / 1" } as const;

export function AutoEditView({
  dir,
  onChanged,
  onNavigate,
}: {
  dir: string;
  onChanged: () => Promise<void>;
  onNavigate?: (to: "recipe" | "finished") => void;
}) {
  const toast = useToast();
  const hdr = useHdrToSdr();
  const [sources, setSources] = useState<SourceRow[]>([]);
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [recipe, setRecipe] = useState<Recipe | null>(null);
  const [thumbs, setThumbs] = useState<Record<string, string>>({});
  const [plans, setPlans] = useState<Record<string, ReframeSummary | "none" | "error">>({});
  const [autoFraming, setAutoFraming] = useState(false);
  const [framingFor, setFramingFor] = useState<{ s: SourceRow; aspect: string } | null>(null);
  const [music, setMusic] = useState<GeneratedAsset[]>([]);
  const [speechReady, setSpeechReady] = useState<boolean | null>(null);
  const [swatch, setSwatch] = useState<CaptionPreview | "loading" | "error" | null>(null);
  const [stages, setStages] = useState<Record<string, Stage>>({});
  const [running, setRunning] = useState<string | null>(null); // job id
  const [runError, setRunError] = useState<string | null>(null);
  const [lastRun, setLastRun] = useState<RecipeStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const runTag = useRef<string | null>(null);
  const persistTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  // The recipe object produced by the user's last edit on this screen (see `patch`). Only that object is saved.
  const userRecipe = useRef<Recipe | null>(null);
  // The project the last user edit was made in: an edit is never written into another project.
  const userRecipeDir = useRef<string | null>(null);

  // `reloadRecipe` re-reads the recipe from project settings (after a library apply) instead of keeping local edits.
  const refresh = useCallback(async (reloadRecipe = false) => {
    const [list, proj, settings, models, status] = await Promise.all([
      sourceList(dir),
      projectShow(dir).catch(() => null),
      settingsGet(dir).catch(() => null),
      modelsStatus().catch(() => null),
      recipeStatus(dir).catch(() => null),
    ]);
    setAutoFraming(settings ? framingApproval(settings) === "auto" : false);
    setPlans({});
    setSources(list.sources as SourceRow[]);
    setShow(proj);
    setSpeechReady(models ? (models.speech.ready ?? null) : null);
    setLastRun(status);
    const saved = (settings?.project?.recipe ?? {}) as Partial<Recipe>;
    setRecipe((cur) => {
      if (cur && !reloadRecipe) return cur;
      const all = (list.sources as SourceRow[]).map((s) => s.id);
      return {
        style: (saved.style as Style) ?? (settings?.project?.cut_preset as Style) ?? "natural",
        formats: saved.formats?.length ? saved.formats : ["16:9"],
        captions: {
          on: saved.captions?.on ?? (settings?.project?.burn_captions !== false),
          style: saved.captions?.style ?? DEFAULT_CAPTION_STYLE,
        },
        finish: {
          color: saved.finish?.color ?? (settings?.project?.color_preset as string) ?? "neutral",
          audio: saved.finish?.audio ?? (settings?.project?.audio_preset as string) ?? "talking-head",
          music: saved.finish?.music ?? null,
          music_level: saved.finish?.music_level ?? DEFAULT_MUSIC_LEVEL_DB,
          lut_path: saved.finish?.lut_path ?? null,
          lut_intensity: saved.finish?.lut_intensity ?? 100,
          match_shots: saved.finish?.match_shots ?? true,
        },
        sources: saved.sources?.length ? saved.sources : all,
      };
    });
  }, [dir]);

  useEffect(() => {
    refresh().catch((e) => setError(errorMessage(e)));
  }, [refresh]);

  // Called after a library recipe is applied: the form adopts the project's new recipe settings.
  const reloadSettings = useCallback(() => {
    refresh(true).catch((e) => setError(errorMessage(e)));
  }, [refresh]);

  // Persist the recipe into project settings.json under "recipe". Loads (first load, refresh, library
  // reload) replace `recipe` with an object that is not `userRecipe`, so they are not written back.
  useEffect(() => {
    if (!recipe || recipe !== userRecipe.current || userRecipeDir.current !== dir) return;
    if (persistTimer.current) clearTimeout(persistTimer.current);
    persistTimer.current = setTimeout(() => {
      settingsSet({ recipe }, dir).catch((e) => setError(errorMessage(e)));
    }, 400);
    return () => {
      if (persistTimer.current) clearTimeout(persistTimer.current);
    };
  }, [recipe, dir]);

  // Card thumbnails for framing/source rows.
  useEffect(() => {
    for (const s of sources) {
      if (thumbs[s.id] !== undefined) continue;
      void thumbnails(dir, { source: s.id }, { width: 240 })
        .then((t) => {
          const f = t.frames[0];
          if (f) setThumbs((m) => ({ ...m, [s.id]: assetUrl(f.path) }));
        })
        .catch(() => setThumbs((m) => ({ ...m, [s.id]: "" })));
    }
  }, [sources, dir, thumbs]);

  // Music library assets for the Finish card.
  useEffect(() => {
    engineCall("assets.list", { dir })
      .then((r) =>
        setMusic(interpretAssetLibrary(r).assets.filter((a) => a.kind === "music" && a.exists)),
      )
      .catch(() => setMusic([]));
  }, [dir]);

  // R3: real caption-style swatch — the engine renders a styled PNG on a real frame.
  const swatchPreset =
    recipe?.formats.includes("9:16") || recipe?.formats.includes("1:1")
      ? recipe.formats.includes("1:1") && !recipe.formats.includes("9:16")
        ? "square"
        : "reels"
      : "youtube";
  useEffect(() => {
    if (!recipe?.captions.on || sources.length === 0) {
      setSwatch(null);
      return;
    }
    let stale = false;
    setSwatch("loading");
    captionsPreview(dir, {
      style: recipe.captions.style,
      preset: swatchPreset,
      source: sources.find((s) => recipe.sources.includes(s.id))?.id,
    })
      .then((r) => {
        if (!stale) setSwatch(r);
      })
      .catch(() => {
        if (!stale) setSwatch("error");
      });
    return () => {
      stale = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [dir, recipe?.captions.on, recipe?.captions.style, swatchPreset, sources.length]);

  // recipe.stage events for this run's tag.
  useEffect(() => {
    let off: (() => void) | undefined;
    let cancelled = false;
    void onEngineRaw((payload) => {
      const frame = payload as unknown as { tag?: string | null; event?: RecipeStageEvent };
      if (frame.event?.event !== "recipe.stage") return;
      if (runTag.current && frame.tag !== runTag.current) return;
      const ev = frame.event;
      setStages((m) => ({
        ...m,
        [ev.stage]: { state: ev.state, fraction: ev.fraction, error: ev.error },
      }));
    }).then((f) => {
      if (cancelled) f();
      else off = f;
    });
    return () => {
      cancelled = true;
      off?.();
    };
  }, []);

  const selectedSources = useMemo(
    () => sources.filter((s) => recipe?.sources.includes(s.id)),
    [sources, recipe],
  );
  // R2: every (source × format) pair gets its own plan — plans keyed "id|aspect".
  const reframeAspects = useMemo(
    () => (recipe?.formats ?? []).filter((f) => f !== "16:9"),
    [recipe],
  );
  const needsReframe = useCallback(
    (s: SourceRow, aspect: string) => {
      const v = show?.timeline.sources.find((t) => t.id === s.id)?.video;
      if (!v) return false;
      const [aw, ah] = aspect.split(":").map(Number);
      const target = aw / ah;
      return Math.abs(v.width / v.height / target - 1) >= 0.05;
    },
    [show],
  );
  const planKey = (id: string, aspect: string) => `${id}|${aspect}`;

  // Reframe approval state for each (source × aspect) that needs it.
  useEffect(() => {
    for (const s of sources) {
      for (const aspect of reframeAspects) {
        const k = planKey(s.id, aspect);
        if (!needsReframe(s, aspect) || plans[k] !== undefined) continue;
        setPlans((m) => ({ ...m, [k]: "none" }));
        reframeShow(dir, s.id, aspect)
          .then((r) => setPlans((m) => ({ ...m, [k]: r })))
          .catch(() => setPlans((m) => ({ ...m, [k]: "none" })));
      }
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sources, plans, needsReframe, reframeAspects, dir]);

  const framingPairs = useMemo(
    () =>
      selectedSources.flatMap((s) =>
        reframeAspects.filter((a) => needsReframe(s, a)).map((a) => ({ s, aspect: a })),
      ),
    [selectedSources, reframeAspects, needsReframe],
  );
  const unapproved = framingPairs.filter(({ s, aspect }) => {
    const p = plans[planKey(s.id, aspect)];
    return !(p && p !== "none" && p !== "error" && p.approved);
  });

  const run = useCallback(async () => {
    if (!recipe) return;
    setStages({});
    setRunError(null);
    const job = `recipe-ui-${Date.now().toString(36)}`;
    const tag = `recipe-${job}`;
    runTag.current = tag;
    setRunning(job);
    try {
      // Flush the recipe to settings.json so engine reads current values (R3/R4).
      await settingsSet({ recipe }, dir).catch(() => undefined);
      await recipeRun(
        dir,
        {
          style: recipe.style,
          formats: recipe.formats,
          sources: recipe.sources,
          captions: recipe.captions.on,
          music: recipe.finish.music ?? undefined,
          music_level: recipe.finish.music_level,
          job,
        },
        tag,
      );
      toast("Finished edit ready", { kind: "success" });
      onNavigate?.("finished");
      await onChanged();
    } catch (e) {
      if (errorCode(e) === "cancelled" || errorCode(e) === "render_cancelled") {
        setRunError("Cancelled — run again to resume where it stopped.");
      } else {
        setRunError(errorMessage(e));
      }
    } finally {
      setRunning(null);
      runTag.current = null;
      void recipeStatus(dir).then(setLastRun).catch(() => undefined);
      void refresh();
    }
  }, [recipe, dir, toast, onNavigate, onChanged, refresh]);

  const stageList = useMemo(() => {
    const rows: [string, string][] = [...STAGE_LABELS];
    for (const f of recipe?.formats ?? []) rows.push([`render:${f}`, `Render ${f}`]);
    return rows;
  }, [recipe]);

  if (!recipe) {
    return (
      <PageShell title="Edit recipe" maxWidth={0}>
        <ProgressRow label="Loading recipe" status="working" fraction={undefined} />
      </PageShell>
    );
  }

  // Every user edit on this screen goes through here, so it is the only path that marks a recipe for saving.
  const patch = (p: Partial<Recipe>) => {
    const next: Recipe = { ...recipe, ...p };
    userRecipe.current = next;
    userRecipeDir.current = dir;
    setRecipe(next);
  };

  return (
    <PageShell title="Edit recipe" maxWidth={0} subtitle={"One pass: transcribe, cut, frame and render."}>
      <div className="recipe">
        <div className="recipe__form">
          <RecipeLibrary
            dir={dir}
            onApplied={reloadSettings}
            onBeforeSave={async () => {
              if (recipe) await settingsSet({ recipe }, dir);
            }}
          />

          <Card>
            <h2>Style</h2>
            <SegmentedControl<Style>
              label="Edit style"
              value={recipe.style}
              options={[
                { value: "natural", label: "Natural" },
                { value: "tight", label: "Tight" },
                { value: "punchy", label: "Punchy" },
              ]}
              onChange={(style) => patch({ style })}
            />
            <p className="recipe__desc">{STYLE_DESC[recipe.style]}</p>
          </Card>

          <Card>
            <h2>Format</h2>
            <div className="recipe__chips" role="group" aria-label="Output formats">
              {FORMATS.map((f) => {
                const on = recipe.formats.includes(f);
                return (
                  <Button variant="ghost" size="sm" className="cr-chip-btn" key={f} onClick={() => {
                      const next = on
                        ? recipe.formats.filter((x) => x !== f)
                        : [...recipe.formats, f];
                      if (next.length) patch({ formats: next });
                    }}><Badge
                    aria-pressed={on}
                    tone={on ? "ok" : "neutral"}
                  >
                    {f}
                  </Badge></Button>
                );
              })}
            </div>
          </Card>

          <Card>
            <h2>Captions</h2>
            <Toggle
              label="Burn captions into the finished edit"
              checked={recipe.captions.on}
              onChange={(on) => patch({ captions: { ...recipe.captions, on } })}
            />
            {recipe.captions.on && (
              <>
                <Field label="Caption style">{() => (<Select
                    label="Caption style"
                    value={recipe.captions.style}
                    onChange={(style) =>
                      patch({ captions: { ...recipe.captions, style } })
                    }
                    options={CAPTION_STYLES.map(([value, label]) => ({ value, label }))}
                  />)}</Field>
                {/* R3: real engine-rendered style swatch (`captions.preview` PNG). */}
                {/* The box is there from the first paint with the preview's shape, so nothing
                    below it moves when the preview loads or is rendered again. */}
                <div className="recipe-swatch" style={{ aspectRatio: SWATCH_ASPECT[swatchPreset] }}>
                  {swatch === null ? null : swatch === "loading" ? (
                    <ProgressRow label="Rendering style preview" status="working" />
                  ) : swatch === "error" ? (
                    <p className="recipe__desc">Preview unavailable for this project.</p>
                  ) : (
                    <img
                      className="recipe-swatch-img"
                      src={assetUrl(swatch.path) + `?m=${swatch.mtime_ms ?? 0}`}
                      alt={`${recipe.captions.style} caption style preview`}
                    />
                  )}
                </div>
              </>
            )}
          </Card>

          {reframeAspects.length > 0 && (
            <Card>
              <h2>Framing</h2>
              {framingPairs.length === 0 ? (
                <p className="recipe__desc">
                  Every included source already fits {reframeAspects.join(" and ")}.
                </p>
              ) : (
                framingPairs.map(({ s, aspect }) => {
                  const p = plans[planKey(s.id, aspect)];
                  const approved = p !== undefined && p !== "none" && p !== "error" && p.approved;
                  return (
                    <div className="recipe-frame-row" key={planKey(s.id, aspect)}>
                      <Thumbnail src={thumbs[s.id] || undefined} />
                      <span className="recipe-frame-row__name">{s.name || s.id.slice(0, 8)}</span>
                      <Badge tone="neutral">{aspect}</Badge>
                      <Badge tone={approved || autoFraming ? "ok" : "warn"}>
                        {framingLabel(p && p !== "none" && p !== "error" ? p : undefined, autoFraming)}
                      </Badge>
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={() => setFramingFor({ s, aspect })}
                      >
                        Review framing
                      </Button>
                    </div>
                  );
                })
              )}
            </Card>
          )}

          <Card>
            <h2>Finish</h2>
            <Field label="Colour">{() => (<Select
                label="Colour"
                value={recipe.finish.color}
                onChange={(color) => {
                  // The LUT applies only under the Custom preset (engine, commit 9866899): leaving Custom clears it.
                  patch({ finish: { ...recipe.finish, color, ...(color === "custom" ? {} : { lut_path: null }) } });
                  settingsSet({ color_preset: color }, dir).catch(() => undefined);
                }}
                options={colourOpts(hdr).map(([value, label]) => ({ value, label }))}
              />)}</Field>
            {hdr === false && (
              // A saved HDR preset that cannot run is kept, and the notice is an alert because it is in use.
              <p className="recipe__desc" role={(HDR_COLOR_PRESETS as readonly string[]).includes(recipe.finish.color) ? "alert" : undefined}>
                {HDR_UNAVAILABLE_NOTICE}
              </p>
            )}
            <Field label="Audio">{() => (<Select
                label="Audio"
                value={recipe.finish.audio}
                onChange={(audio) => {
                  patch({ finish: { ...recipe.finish, audio } });
                  settingsSet({ audio_preset: audio }, dir).catch(() => undefined);
                }}
                options={AUDIO_OPTS.map(([value, label]) => ({ value, label }))}
              />)}</Field>
            {/* The engine places the chosen library music under the edit at the level below. */}
            <Field label="Music" note={music.length === 0 ? "No music in the library yet" : undefined}>{(ids) => (<select {...ids}
                className="rk-input cr-select"
                value={recipe.finish.music ?? ""}
                onChange={(e) =>
                  patch({ finish: { ...recipe.finish, music: e.target.value || null } })
                }
              >
                <option value="">None</option>
                {music.map((a) => (
                  <option key={a.assetId} value={a.sourceId ?? a.assetId}>
                    {a.prompt ?? baseName(a.path)}
                  </option>
                ))}
              </select>)}</Field>
            {recipe.finish.music && (
              <Field label={`Music level (${recipe.finish.music_level} dB)`}>{() => (<Slider
                  label={`Music level (${recipe.finish.music_level} dB)`}
                  min={-40}
                  max={-6}
                  step={1}
                  value={recipe.finish.music_level}
                  onChange={(music_level) =>
                    patch({ finish: { ...recipe.finish, music_level } })
                  }
                />)}</Field>
            )}
            {recipe.finish.color === "custom" && (
              <>
                <Field label="Custom LUT" note={recipe.finish.lut_path ? undefined : "Pick a .cube file"}>{(ids) => (<div {...ids} className="recipe-lut-row">
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() =>
                        void pickFile("Choose a .cube LUT", ["cube"]).then((p) => {
                          if (p) patch({ finish: { ...recipe.finish, lut_path: p } });
                        })
                      }
                    ><Icon name="folder-open" />
                      {recipe.finish.lut_path ? "Change" : "Choose file"}
                    </Button>
                    {recipe.finish.lut_path && (
                      <span className="recipe__desc">{baseName(recipe.finish.lut_path)}</span>
                    )}
                  </div>)}</Field>
                <Field label={`LUT intensity (${recipe.finish.lut_intensity}%)`}>{() => (<Slider
                    label={`LUT intensity (${recipe.finish.lut_intensity}%)`}
                    min={0}
                    max={100}
                    step={5}
                    value={recipe.finish.lut_intensity}
                    onChange={(lut_intensity) =>
                      patch({ finish: { ...recipe.finish, lut_intensity } })
                    }
                  />)}</Field>
              </>
            )}
            <Toggle
              label="Match shots — even out colour differences between sources"
              checked={recipe.finish.match_shots}
              onChange={(match_shots) => patch({ finish: { ...recipe.finish, match_shots } })}
            />
          </Card>

          <Card>
            <h2>Sources included</h2>
            {sources.map((s) => (
              <Checkbox
                className="recipe-source-row"
                key={s.id}
                checked={recipe.sources.includes(s.id)}
                onChange={(checked) =>
                  patch({
                    sources: checked
                      ? [...recipe.sources, s.id]
                      : recipe.sources.filter((x) => x !== s.id),
                  })
                }
                label={<span className="recipe-source-row">
                  <Thumbnail src={thumbs[s.id] || undefined} />
                  <span className="recipe-source-row__name">{s.name || s.id.slice(0, 8)}</span>
                </span>}
              />
            ))}
            {sources.length === 0 && (
              <p className="recipe__desc">Import footage on the Media screen first.</p>
            )}
          </Card>

          <div className="recipe__cta">
            <Button
              variant="primary"
              onClick={() => void run()}
              disabled={(recipe.sources.length === 0 || recipe.formats.length === 0) || (running !== null)}
              aria-busy={(running !== null) || undefined}
              className="cr-btn--block"
            >{!(running !== null) && <Icon name="wand-sparkles" />}
              {running !== null ? "Making…" : "Make finished edit"}
            </Button>
            {!autoFraming && reframeAspects.length > 0 && unapproved.length > 0 && (
              <p className="recipe__blocked">
                Framing for{" "}
                {unapproved
                  .map(({ s, aspect }) => `${s.name || s.id.slice(0, 8)} (${aspect})`)
                  .join(", ")}{" "}
                needs approval before the run.
              </p>
            )}
            {speechReady === false && (
              <p className="recipe__blocked">
                The speech model is unavailable — untranscribed sources get a visual edit.
              </p>
            )}
            {error && (
              <p role="alert" className="recipe__blocked" style={{ color: "var(--rk-bad)" }}>
                {error}
              </p>
            )}
          </div>
        </div>

        <div className="recipe__run">
          {/* A run refused before its first stage still has to show why. */}
          {running || runError || Object.keys(stages).length > 0 ? (
            <Card>
              <h2>This run</h2>
              {stageList.map(([id, label]) => {
                const st = stages[id];
                return (
                  <ProgressRow
                    key={id}
                    label={label}
                    status={
                      st?.state === "done"
                        ? "done"
                        : st?.state === "skipped"
                          ? "skipped"
                          : st?.state === "failed"
                            ? "failed"
                            : st?.state === "running"
                              ? "running"
                              : "waiting"
                    }
                    tone={
                      st?.state === "done"
                        ? "ok"
                        : st?.state === "failed"
                          ? "danger"
                          : st?.state === "skipped"
                            ? "neutral"
                            : "info"
                    }
                    fraction={st?.state === "running" ? st.fraction : null}
                    error={st?.state === "failed" ? st.error : undefined}
                  />
                );
              })}
              {running && (
                <Button variant="secondary" onClick={() => void recipeCancel(running)}>
                  Cancel
                </Button>
              )}
              {runError && !running && (
                <ErrorPanel
                  title="The recipe stopped"
                  message={runError}
                  actions={
                    <Button size="sm" variant="secondary" onClick={() => void run()}><Icon name="rotate-ccw" />
                      Retry
                    </Button>
                  }
                />
              )}
            </Card>
          ) : lastRun?.ran ? (
            <Card>
              <h2>Last run</h2>
              <p className="recipe__desc">
                Finished {lastRun.finished_at ? new Date(lastRun.finished_at).toLocaleString() : "recently"}
                {" · "}
                {(lastRun.signature?.formats as string[] | undefined)?.join(", ") ?? "—"}
                {" · style "}
                {String(lastRun.signature?.style ?? "—")}
              </p>
              <div className="recipe__chips">
                {Object.entries(lastRun.done).map(([k]) => (
                  <Badge key={k} tone="ok"><Icon name="check" width={12} height={12} />
                    {k.replace("render:", "Render ")}
                  </Badge>
                ))}
              </div>
            </Card>
          ) : (
            <EmptyState title="Your recipe runs here" icon={<Icon name="wand-sparkles" width={28} height={28} />}>
              Set the recipe on the left, then run it — each stage reports here.
            </EmptyState>
          )}
        </div>
      </div>

      {framingFor && (
        <FramingDrawer
          dir={dir}
          source={framingFor.s}
          aspect={framingFor.aspect}
          plan={plans[planKey(framingFor.s.id, framingFor.aspect)]}
          autoFraming={autoFraming}
          onClose={() => setFramingFor(null)}
          onChanged={(p) =>
            setPlans((m) => ({ ...m, [planKey(framingFor.s.id, framingFor.aspect)]: p }))
          }
        />
      )}
    </PageShell>
  );
}

/** Framing review for one (source, aspect) plan; shared by Edit recipe and Shorts. */
export function FramingDrawer({
  dir,
  source,
  aspect,
  plan,
  autoFraming = false,
  onClose,
  onChanged,
}: {
  dir: string;
  source: SourceRow;
  aspect: string;
  plan?: ReframeSummary | "none" | "error";
  autoFraming?: boolean;
  onClose: () => void;
  onChanged: (p: ReframeSummary) => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [shots, setShots] = useState<{ mid_ms: number; src?: string; crop: AnchorView["crop"]; approved: boolean }[]>([]);

  const loadShots = useCallback(
    async (p: ReframeSummary) => {
      const views = anchorViews({ ...p, plan: p.plan ?? (p as ReframeSummary).plan });
      const times = views.map((v) => v.mid_ms);
      const t = await thumbnails(dir, { source: source.id }, { times_ms: times, width: 640 });
      setShots(
        views.map((v, i) => ({
          mid_ms: v.mid_ms,
          src: t.frames[i] ? assetUrl(t.frames[i].path) : undefined,
          crop: v.crop,
          approved: v.approved,
        })),
      );
    },
    [dir, source.id],
  );

  useEffect(() => {
    if (plan && plan !== "none" && plan !== "error") {
      loadShots(plan).catch((e) => setErr(errorMessage(e)));
    } else {
      setShots([]);
    }
  }, [plan, loadShots]);

  const analyze = async () => {
    setBusy("analyse");
    setErr(null);
    try {
      const r = await reframeAnalyze(dir, source.id, aspect, `frame-${source.id.slice(0, 8)}`);
      onChanged(r);
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const approve = async () => {
    setBusy("approve");
    setErr(null);
    try {
      const r = await reframeApprove(dir, source.id, { all: true, approver: "ui", aspect });
      onChanged(r);
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  const ready = plan && plan !== "none" && plan !== "error";
  return (
    <Drawer
      title={`Framing — ${source.name || source.id.slice(0, 8)}`}
      open
      onClose={onClose}
      footer={
        ready ? (
          plan.approved ? (
            <Badge tone="ok"><Icon name="check" width={12} height={12} />
              {framingLabel(plan, false)}
            </Badge>
          ) : (
            <Button
              variant="primary"
              onClick={() => void approve()}
              disabled={(busy === "approve")}
              aria-busy={(busy === "approve") || undefined}
            >{!(busy === "approve") && <Icon name="check" />}
              {busy === "approve" ? "Approving…" : "Approve framing"}
            </Button>
          )
        ) : (
          <Button
            variant="primary"
            onClick={() => void analyze()}
            disabled={(busy === "analyse")}
            aria-busy={(busy === "analyse") || undefined}
          >
            {busy === "analyse" ? "Analysing…" : <>Analyse framing for {aspect}</>}
          </Button>
        )
      }
    >
      <p className="recipe__desc" style={{ margin: 0 }}>
        The {aspect} crop over sampled moments of this source. {autoFraming ? "Saved policy approves tracked framing during delivery; review is optional." : "Approve it to unblock vertical output."}
      </p>
      {err && (
        <p role="alert" style={{ margin: 0, color: "var(--rk-bad)", fontSize: 13 }}>
          {err}
        </p>
      )}
      {ready && (
        <div className="recipe-anchors">
          {shots.map((s, i) => (
            <div className="recipe-anchor" key={i}>
              {s.src ? <img src={s.src} alt="" /> : <Thumbnail />}
              {s.src && (
                <span
                  className="recipe-anchor__crop"
                  style={{
                    left: `${s.crop.x * 100}%`,
                    top: `${s.crop.y * 100}%`,
                    width: `${s.crop.w * 100}%`,
                    height: `${s.crop.h * 100}%`,
                  }}
                />
              )}
              <span className="recipe-anchor__t">{timecode(s.mid_ms)}</span>
            </div>
          ))}
        </div>
      )}
      {busy === "analyse" && <ProgressRow label="Tracking the face" status="working" fraction={undefined} />}
    </Drawer>
  );
}
