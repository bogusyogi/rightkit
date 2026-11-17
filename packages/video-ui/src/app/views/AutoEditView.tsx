import { Badge, Button, Card, Checkbox, EmptyState, Field, PageShell, SegmentedControl, Select, Slider, Toggle, useToast } from "@rightkit/react-shell/react";
import type { ReactNode } from "react";
import { assetUrl } from "../../lib/asset.js";
import { baseName } from "../../lib/project.js";
import { CAPTION_STYLES, framingLabel, type ReframeSummary, type SourceRow } from "../../lib/media.js";
import { Drawer, ErrorPanel, ProgressRow, Thumbnail, timecode, Icon } from "../ui/index.js";
import { RecipeLibrary } from "./RecipeLibrary.js";
import { useVideoFramingReview, type FramingPlanView } from "../hooks/useVideoFramingReview.js";
import { useVideoRecipe, type VideoRecipe } from "../hooks/useVideoRecipe.js";
import { HDR_COLOR_PRESETS, HDR_UNAVAILABLE_NOTICE, useHdrToSdr } from "../../hooks/useHdrToSdr.js";

type Style = VideoRecipe["style"];

const STYLE_DESC: Record<Style, string> = {
  natural: "Keeps the room to breathe — removes only mistakes and dead air.",
  tight: "Cuts pauses, fillers and retakes for a brisk, conversational pace.",
  punchy: "Aggressive trims and fast beat changes for short-form energy.",
};

const FORMATS = ["16:9", "9:16", "1:1"];
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

/** §3.3 Edit recipe: persisted form + recipe.run stage panel + framing review drawer. */
const SWATCH_ASPECT = { youtube: "16 / 9", reels: "9 / 16", square: "1 / 1" } as const;

export function AutoEditView({
  dir,
  onChanged,
  refreshChangedProject,
  onNavigate,
  renderFramingReview,
}: {
  dir: string;
  onChanged: () => Promise<void>;
  refreshChangedProject?: (dir: string) => Promise<void>;
  onNavigate?: (to: "recipe" | "finished") => void;
  renderFramingReview?: (props: FramingDrawerProps) => ReactNode;
}) {
  const toast = useToast();
  const hdr = useHdrToSdr();
  const {
    sources, recipe, thumbs, plans, autoFraming, music, speechReady, swatch, swatchPreset,
    stages, running, runError, cancelled, lastRun, error, refreshError, reframeAspects, framingPairs, unapproved,
    planKey, retryLoad, run, cancel, pickLut, setColor, setAudio, applyLibraryRecipe, saveLibraryRecipe,
    canRun, canCancel, cancelling, setStyle, setCaptionsEnabled, setCaptionStyle, setMusic,
    setMusicLevel, setLutIntensity, setMatchShots, toggleFormat, setSourceIncluded,
    framingFor, openFraming, closeFraming, framingChanged,
  } = useVideoRecipe({
    dir,
    refreshChangedProject: refreshChangedProject ?? onChanged,
    onFinished: (workspaceRefreshed) => {
      toast("Finished edit ready", { kind: "success" });
      if (workspaceRefreshed) onNavigate?.("finished");
    },
  });

  if (!recipe) {
    return (
      <PageShell title="Edit recipe" maxWidth={0}>
        {error ? (
          <ErrorPanel
            title="Could not load recipe"
            message={error}
            actions={<Button variant="secondary" onClick={() => void retryLoad()}>Reload recipe</Button>}
          />
        ) : (
          <ProgressRow label="Loading recipe" status="working" />
        )}
      </PageShell>
    );
  }

  const stageList: [string, string][] = [...STAGE_LABELS];
  for (const format of recipe.formats) stageList.push([`render:${format}`, `Render ${format}`]);

  return (
    <PageShell title="Edit recipe" maxWidth={0} subtitle={"One pass: transcribe, cut, frame and render."}>
      <div className="recipe">
        <div className="recipe__form">
          <RecipeLibrary dir={dir} applyRecipe={applyLibraryRecipe} saveRecipe={saveLibraryRecipe} />

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
              onChange={setStyle}
            />
            <p className="recipe__desc">{STYLE_DESC[recipe.style]}</p>
          </Card>

          <Card>
            <h2>Format</h2>
            <div className="recipe__chips" role="group" aria-label="Output formats">
              {FORMATS.map((f) => {
                const on = recipe.formats.includes(f);
                return (
                  <Button variant="ghost" size="sm" className="cr-chip-btn" key={f} onClick={() => toggleFormat(f)}><Badge
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
              onChange={setCaptionsEnabled}
            />
            {recipe.captions.on && (
              <>
                <Field label="Caption style">{() => (<Select
                    label="Caption style"
                    value={recipe.captions.style}
                    onChange={setCaptionStyle}
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
                  const approved = typeof p === "object" && p.approved;
                  return (
                    <div className="recipe-frame-row" key={planKey(s.id, aspect)}>
                      <Thumbnail src={thumbs[s.id] || undefined} />
                      <span className="recipe-frame-row__name">{s.name || s.id.slice(0, 8)}</span>
                      <Badge tone="neutral">{aspect}</Badge>
                      <Badge tone={approved || autoFraming ? "ok" : "warn"}>
                        {p === "error" ? "Framing could not be loaded" : framingLabel(typeof p === "object" ? p : undefined, autoFraming)}
                      </Badge>
                      <Button
                        size="sm"
                        variant="secondary"
                        onClick={() => openFraming(s, aspect)}
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
                onChange={setColor}
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
                onChange={setAudio}
                options={AUDIO_OPTS.map(([value, label]) => ({ value, label }))}
              />)}</Field>
            {/* The engine places the chosen library music under the edit at the level below. */}
            <Field label="Music" note={music.length === 0 ? "No music in the library yet" : undefined}>{(ids) => (<select {...ids}
                className="rk-input cr-select"
                value={recipe.finish.music ?? ""}
                onChange={(e) => setMusic(e.target.value)}
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
                  onChange={setMusicLevel}
                />)}</Field>
            )}
            {recipe.finish.color === "custom" && (
              <>
                <Field label="Custom LUT" note={recipe.finish.lut_path ? undefined : "Pick a .cube file"}>{(ids) => (<div {...ids} className="recipe-lut-row">
                    <Button
                      size="sm"
                      variant="secondary"
                      onClick={() => void pickLut("Choose a .cube LUT")}
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
                    onChange={setLutIntensity}
                  />)}</Field>
              </>
            )}
            <Toggle
              label="Match shots — even out colour differences between sources"
              checked={recipe.finish.match_shots}
              onChange={setMatchShots}
            />
          </Card>

          <Card>
            <h2>Sources included</h2>
            {sources.map((s) => (
              <Checkbox
                className="recipe-source-row"
                key={s.id}
                checked={recipe.sources.includes(s.id)}
                onChange={(checked) => setSourceIncluded(s.id, checked)}
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
              disabled={!canRun}
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
          {running || runError || refreshError || cancelled || Object.keys(stages).length > 0 ? (
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
              {(canCancel || cancelling) && (
                <Button variant="secondary" disabled={cancelling} onClick={() => void cancel()}>
                  {cancelling ? "Cancelling…" : "Cancel"}
                </Button>
              )}
              {cancelled && <p role="status">Cancelled — run again to resume where it stopped.</p>}
              {refreshError && <ErrorPanel title="Finished edit ready · workspace refresh failed" message={refreshError} />}
              {runError && (
                <ErrorPanel
                  title={running ? "Recipe warning" : "The recipe stopped"}
                  message={runError}
                  actions={canRun ? (
                    <Button size="sm" variant="secondary" onClick={() => void run()}><Icon name="rotate-ccw" />
                      Retry
                    </Button>
                  ) : undefined}
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

      {framingFor?.dir === dir && (() => {
        const selected = framingFor;
        const props: FramingDrawerProps = {
          dir, source: selected.s, aspect: selected.aspect,
          plan: plans[planKey(selected.s.id, selected.aspect)], autoFraming,
          onClose: closeFraming,
          onChanged: (plan) => framingChanged(selected.s.id, selected.aspect, plan),
        };
        return renderFramingReview ? renderFramingReview(props) : <FramingDrawer {...props} />;
      })()}
    </PageShell>
  );
}

/** Framing review for one (source, aspect) plan; shared by Edit recipe and Shorts. */
export type FramingDrawerProps = {
  dir: string;
  source: SourceRow;
  aspect: string;
  plan?: FramingPlanView;
  autoFraming?: boolean;
  onClose: () => void;
  onChanged: (p: ReframeSummary) => void;
};

export function FramingDrawer({
  dir,
  source,
  aspect,
  plan,
  autoFraming = false,
  onClose,
  onChanged,
}: FramingDrawerProps) {
  const { busy, error: err, shots, analyze, approve, reload, approvalInvalidReason } = useVideoFramingReview({ dir, source, aspect, plan, onChanged });

  const ready = typeof plan === "object";
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
              disabled={busy !== null || !!approvalInvalidReason}
              aria-busy={(busy === "approve") || undefined}
            >{!(busy === "approve") && <Icon name="check" />}
              {busy === "approve" ? "Approving…" : "Approve framing"}
            </Button>
          )
        ) : plan === "none" ? (
          <Button
            variant="primary"
            onClick={() => void analyze()}
            disabled={busy !== null}
            aria-busy={(busy === "analyse") || undefined}
          >
            {busy === "analyse" ? "Analysing…" : <>Analyse framing for {aspect}</>}
          </Button>
        ) : (
          <Button variant="secondary" onClick={() => void reload()} disabled={busy !== null} aria-busy={busy === "read" || undefined}>
            {busy === "read" ? "Loading…" : "Reload framing"}
          </Button>
        )
      }
    >
      <p className="recipe__desc" style={{ margin: 0 }}>
        The {aspect} crop over sampled moments of this source. {autoFraming ? "Saved policy approves tracked framing during delivery; review is optional." : "Approve it to unblock vertical output."}
      </p>
      {plan === "error" && !err && (
        <p role="alert">Framing could not be loaded. Reload to check the saved plan.</p>
      )}
      {err && (
        <p role="alert" style={{ margin: 0, color: "var(--rk-bad)", fontSize: 13 }}>
          {err}
        </p>
      )}
      {ready && !plan.approved && approvalInvalidReason === "missing_plan_hash" && (
        <p role="alert" className="cr-muted">Reload framing before approving.</p>
      )}
      {ready && !plan.approved && !err && approvalInvalidReason === "preview_not_ready" && (
        <p role="status" className="cr-muted">Loading framing preview…</p>
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
