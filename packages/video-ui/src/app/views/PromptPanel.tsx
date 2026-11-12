import { Badge, Button, Card, Field, Input, PageShell, SegmentedControl, Select, Toggle } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useMemo, useState } from "react";
import type { ProgressState } from "../hooks/useProgress.js";
import type { AssembleForm, ShotInput, TitleDraft } from "../lib/assemble.js";
import { engineCall, errorMessage } from "../../lib/engine.js";
import { type HostInfo, genRightCallFailed, interpretHost } from "../lib/genright.js";
import {
  type DurationControl,
  type ModelCatalog,
  type ModelKind,
  type ModelRow,
  durationControl,
  findModel,
  formatUsd,
  ofKind,
  parseCatalog,
  pickModel,
  providersOf,
  spendTotal,
} from "../lib/models.js";
import { ErrorPanel, ProgressRow, Icon } from "../ui/index.js";
import { hostAppName } from "../appName.js";

// Preferred starting picks; used only when the installed the host app lists them (else the cheapest of the kind).
const DEFAULTS = {
  video: { provider: "fal", model: "fal-ai/veo3.1/fast" },
  image: { provider: "fal", model: "fal-ai/flux-2/klein/9b" },
  voice: { provider: "elevenlabs", model: "eleven_v4_turbo" },
};
const TEMPLATES: { id: TitleDraft["template"]; label: string }[] = [
  { id: "lower_third", label: "Lower third" },
  { id: "centered", label: "Centered" },
  { id: "caption", label: "Caption" },
];

type Shot = ShotInput & { prompt: string };
type Title = TitleDraft;

/** A shot of `kind` on the preferred (or cheapest) listed model, its default duration preselected. */
function newShot(kind: Shot["kind"], catalog: ModelCatalog | null): Shot {
  const m = catalog ? pickModel(catalog, kind, DEFAULTS[kind]) : undefined;
  return {
    prompt: "",
    kind,
    duration_s: kind === "video" ? durationControl(m).initial : 4,
    provider: m?.provider ?? DEFAULTS[kind].provider,
    model: m?.model ?? DEFAULTS[kind].model,
  };
}

/** Keeps a shot's model valid for its kind and its duration within the model's allowed values. */
function fitShot(s: Shot, catalog: ModelCatalog): Shot {
  const m = findModel(catalog, s.provider, s.model);
  if (!m || m.kind !== s.kind) return { ...newShot(s.kind, catalog), prompt: s.prompt };
  return s.kind === "video" ? { ...s, duration_s: fitDuration(durationControl(m), s.duration_s) } : s;
}

function fitDuration(c: DurationControl, d: number): number {
  if (c.type === "set") return c.options.includes(d) ? d : c.initial;
  if (c.type === "range") return d >= c.min && d <= c.max ? d : c.initial;
  return d;
}

function modelOptionLabel(m: ModelRow): string {
  return `${m.label}${m.estUsd !== null ? ` (${formatUsd(m.estUsd)})` : ""}`;
}

function ModelPicker({
  catalog,
  kind,
  provider,
  model,
  onChange,
  providerLabel = "Provider",
  modelLabel = "Model",
}: {
  catalog: ModelCatalog;
  kind: ModelKind;
  provider: string;
  model: string;
  onChange: (m: ModelRow) => void;
  providerLabel?: string;
  modelLabel?: string;
}) {
  const providers = providersOf(catalog, kind);
  const models = ofKind(catalog, kind).filter((m) => m.provider === provider);
  const pick = (p: string, mdl?: string) => {
    const row = ofKind(catalog, kind).find((m) => m.provider === p && (mdl === undefined || m.model === mdl));
    if (row) onChange(row);
  };
  return (
    <>
      <Field label={providerLabel}>{(ids) => (<select {...ids} className="rk-input cr-select" value={provider} onChange={(e) => pick(e.target.value)}>
          {!providers.includes(provider) && <option value={provider}>{provider}</option>}
          {providers.map((p) => (
            <option key={p} value={p}>
              {p}
            </option>
          ))}
        </select>)}</Field>
      <Field label={modelLabel}>{(ids) => (<select {...ids} className="rk-input cr-select" value={model} onChange={(e) => pick(provider, e.target.value)}>
          {!models.some((m) => m.model === model) && <option value={model}>{model}</option>}
          {models.map((m) => (
            <option key={m.model} value={m.model}>
              {modelOptionLabel(m)}
            </option>
          ))}
        </select>)}</Field>
    </>
  );
}

export function PromptPanel({
  busy,
  progress,
  onSubmit,
  onOpenFinished,
}: {
  busy: boolean;
  progress?: ProgressState;
  onSubmit: (form: AssembleForm) => void;
  onOpenFinished?: () => void;
}) {
  const [gr, setGr] = useState<HostInfo>({ state: "checking" });
  const [prompt, setPrompt] = useState("");
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [shots, setShots] = useState<Shot[]>([newShot("video", null), newShot("video", null)]);
  const [narrate, setNarrate] = useState(true);
  const [narration, setNarration] = useState("");
  const [voice, setVoice] = useState({ ...DEFAULTS.voice, voice: "" });
  const [titles, setTitles] = useState<Title[]>([]);
  const [preset, setPreset] = useState<"youtube" | "reels">("youtube");
  const [render, setRender] = useState(true);
  const [confirming, setConfirming] = useState(false);

  const check = useCallback(async () => {
    setGr({ state: "checking" });
    try {
      setGr(interpretHost(await engineCall("genright.status", {})));
    } catch (e) {
      setGr(genRightCallFailed(errorMessage(e)));
    }
  }, []);
  useEffect(() => {
    void check();
  }, [check]);

  // The model catalog (pickers, durations, estimates) comes from the installed the host app.
  const ready0 = gr.state === "ready";
  useEffect(() => {
    if (!ready0) return;
    let live = true;
    setCatalogError(null);
    engineCall("genright.models", {})
      .then((r) => {
        if (!live) return;
        const c = parseCatalog(r);
        setCatalog(c);
        setShots((s) => s.map((x) => fitShot(x, c)));
        setVoice((v) => {
          const m = findModel(c, v.provider, v.model) ?? pickModel(c, "voice", DEFAULTS.voice);
          return m ? { ...v, provider: m.provider, model: m.model } : v;
        });
      })
      .catch((e) => {
        if (live) {
          setCatalog(null);
          setCatalogError(errorMessage(e));
        }
      });
    return () => {
      live = false;
    };
  }, [ready0]);

  const setCount = (n: number) => {
    const count = Math.max(1, Math.min(6, n || 1));
    setShots((s) => (count <= s.length ? s.slice(0, count) : [...s, ...Array.from({ length: count - s.length }, () => newShot("video", catalog))]));
  };
  const patchShot = (i: number, p: Partial<Shot>) => setShots((s) => s.map((x, j) => (j === i ? { ...x, ...p } : x)));
  const setKind = (i: number, kind: Shot["kind"]) => {
    const { provider, model, duration_s } = newShot(kind, catalog);
    patchShot(i, { kind, provider, model, duration_s });
  };
  const setShotModel = (i: number, m: ModelRow) =>
    setShots((all) =>
      all.map((x, j) =>
        j === i ? { ...x, provider: m.provider, model: m.model, duration_s: x.kind === "video" ? fitDuration(durationControl(m), x.duration_s) : x.duration_s } : x,
      ),
    );
  const patchTitle = (i: number, p: Partial<Title>) => setTitles((t) => t.map((x, j) => (j === i ? { ...x, ...p } : x)));

  const form = useMemo<AssembleForm>(
    () => ({
      prompt,
      shots,
      narration: narrate ? { text: narration, ...voice } : null,
      titles,
      preset,
      render,
    }),
    [prompt, shots, narrate, narration, voice, titles, preset, render],
  );

  const narrates = narrate && narration.trim().length > 0;
  const spend = useMemo(() => {
    const lines = shots.map((s, i) => ({ label: `Shot ${i + 1}`, estUsd: findModel(catalog, s.provider, s.model)?.estUsd ?? null }));
    if (narrates) lines.push({ label: "Narration", estUsd: findModel(catalog, voice.provider, voice.model)?.estUsd ?? null });
    return spendTotal(lines);
  }, [catalog, shots, voice, narrates]);

  const titlesOk = titles.every((t) => t.text.trim().length > 0 && t.text.length <= 160 && t.duration_s > 0 && t.start_s >= 0);
  const valid = prompt.trim().length > 0 && shots.every((s) => s.provider && s.model && s.duration_s > 0) && titlesOk;
  const ready = gr.state === "ready";
  const disabled = !ready || busy || !valid;

  return (
    <PageShell title="From prompt" maxWidth={0} subtitle={`Generate a video from a text prompt with ${hostAppName()}.`}>
      <div className="Prompt">
        <Card>
          <div className="Prompt-grRow" aria-live="polite">
            <span className="Prompt-label">{hostAppName()}</span>
            <span title={ready
                  ? `${hostAppName()} ${gr.appVersion ?? ""} · contract ${gr.found ?? "?"}`
                  : (gr.detail ?? undefined)}><Badge
              tone={ready ? "ok" : gr.state === "checking" ? "neutral" : "warn"}
            >
              {ready ? `${hostAppName()} ready` : gr.state === "checking" ? "Checking…" : gr.state}
            </Badge></span>
            <Button size="sm" variant="ghost" onClick={() => void check()}>
              Re-check
            </Button>
          </div>
          {!ready && gr.state !== "checking" && (
            <ErrorPanel
              title={
                gr.state === "outdated"
                  ? `Update ${hostAppName()}`
                  : gr.state === "missing"
                    ? `Install ${hostAppName()}`
                    : `${hostAppName()} could not be started`
              }
              detail={
                (gr.state === "outdated" && gr.found !== undefined && gr.required !== undefined
                  ? `The installed ${hostAppName()} speaks engine contract ${gr.found}; ${hostAppName()} needs ${gr.required}. Update ${hostAppName()}, then re-check.`
                  : gr.state === "outdated"
                    ? `The installed ${hostAppName()} is too old for ${hostAppName()}. Update ${hostAppName()}, then re-check.`
                    : gr.state === "missing"
                      ? `${hostAppName()} generates media through the ${hostAppName()} app. Install ${hostAppName()}, then re-check.`
                      : `${hostAppName()} is installed but its engine did not run. Reinstall or update ${hostAppName()}, then re-check.`) +
                (gr.detail ? ` (${gr.detail})` : "") +
                (gr.enginePath ? ` Engine: ${gr.enginePath}` : "")
              }
            />
          )}
          {catalogError && (
            <p role="status" className="Prompt-error">
              Could not load {hostAppName()}'s model list ({catalogError}); enter provider and model by hand.
            </p>
          )}

          <Field label="Prompt">{(ids) => (<textarea {...ids}
              className="rk-input cr-textarea"
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="What is the video about?"
            />)}</Field>
        </Card>

        <Card>
          <div className="Prompt-cardHead">
            <h2>Shots</h2>
            <SegmentedControl
              label="Number of shots"
              value={String(Math.min(shots.length, 6)) as "1"}
              options={["1", "2", "3", "4", "5", "6"].map((v) => ({ value: v as "1", label: v }))}
              onChange={(v) => setCount(Number(v))}
            />
          </div>
          {shots.map((s, i) => (
            <div key={i} className="Prompt-shotRow">
              <div className="Prompt-shotGrid">
                <Field label={`Shot ${i + 1} · kind`}>{(ids) => (<select {...ids} className="rk-input cr-select" aria-label={`Shot ${i + 1} kind`} value={s.kind} onChange={(e) => setKind(i, e.target.value as Shot["kind"])}>
                    <option value="video">Video</option>
                    <option value="image">Image</option>
                  </select>)}</Field>
                {catalog ? (
                  <ModelPicker
                    catalog={catalog}
                    kind={s.kind}
                    provider={s.provider}
                    model={s.model}
                    providerLabel="Provider"
                    modelLabel="Model"
                    onChange={(m) => setShotModel(i, m)}
                  />
                ) : (
                  <>
                    <Field label="Provider">{(ids) => (<Input {...ids} value={s.provider} onChange={(e) => patchShot(i, { provider: e.target.value })} />)}</Field>
                    <Field label="Model">{(ids) => (<Input {...ids} value={s.model} onChange={(e) => patchShot(i, { model: e.target.value })} />)}</Field>
                  </>
                )}
                <Field label={s.kind === "image" ? "Still (s)" : "Duration (s)"}>{() => (<DurationInput
                    control={s.kind === "video" ? durationControl(findModel(catalog, s.provider, s.model)) : { type: "free", initial: 4 }}
                    value={s.duration_s}
                    onChange={(d) => patchShot(i, { duration_s: d })}
                  />)}</Field>
              </div>
              <Field label="Shot prompt" note={"Optional — defaults to the main prompt."}>{(ids) => (<Input {...ids} value={s.prompt} onChange={(e) => patchShot(i, { prompt: e.target.value })} />)}</Field>
            </div>
          ))}
        </Card>

        <Card>
          <div className="Prompt-cardHead">
            <h2>Narration</h2>
            <Toggle label="Generate a voiceover" checked={narrate} onChange={setNarrate} />
          </div>
          {narrate && (
            <>
              <Field label="Narration script">{(ids) => (<textarea {...ids} className="rk-input cr-textarea" value={narration} onChange={(e) => setNarration(e.target.value)} />)}</Field>
              <div className="Prompt-shotGrid Prompt-shotGrid--auto">
                {catalog ? (
                  <ModelPicker
                    catalog={catalog}
                    kind="voice"
                    provider={voice.provider}
                    model={voice.model}
                    providerLabel="Voice provider"
                    modelLabel="Voice model"
                    onChange={(m) => setVoice({ ...voice, provider: m.provider, model: m.model })}
                  />
                ) : (
                  <>
                    <Field label="Voice provider">{(ids) => (<Input {...ids} value={voice.provider} onChange={(e) => setVoice({ ...voice, provider: e.target.value })} />)}</Field>
                    <Field label="Voice model">{(ids) => (<Input {...ids} value={voice.model} onChange={(e) => setVoice({ ...voice, model: e.target.value })} />)}</Field>
                  </>
                )}
                <Field label="Voice" note={"Optional"}>{(ids) => (<Input {...ids} value={voice.voice} onChange={(e) => setVoice({ ...voice, voice: e.target.value })} />)}</Field>
              </div>
            </>
          )}
        </Card>

        <Card>
          <div className="Prompt-cardHead">
            <h2>Titles</h2>
            <Button
              size="sm"
              variant="secondary"
              onClick={() => setTitles((x) => [...x, { template: "lower_third", text: "", subtitle: "", start_s: 0, duration_s: 3 }])}
            ><Icon name="plus" />
              Add title
            </Button>
          </div>
          {titles.map((t, i) => (
            <div key={i} className="Prompt-titleRow">
              <Field label="Template">{(ids) => (<select {...ids} className="rk-input cr-select" value={t.template} onChange={(e) => patchTitle(i, { template: e.target.value as Title["template"] })}>
                  {TEMPLATES.map((x) => (
                    <option key={x.id} value={x.id}>
                      {x.label}
                    </option>
                  ))}
                </select>)}</Field>
              <Field label={t.template === "lower_third" ? "Title" : "Text"}>{(ids) => (<Input {...ids} maxLength={160} value={t.text} onChange={(e) => patchTitle(i, { text: e.target.value })} />)}</Field>
              {t.template === "lower_third" && (
                <Field label="Subtitle" note={"Optional"}>{(ids) => (<Input {...ids} maxLength={160} value={t.subtitle} onChange={(e) => patchTitle(i, { subtitle: e.target.value })} />)}</Field>
              )}
              <Field label="Start (s)">{(ids) => (<Input {...ids} type="number" min={0} step={0.5} value={t.start_s} onChange={(e) => patchTitle(i, { start_s: Number(e.target.value) })} />)}</Field>
              <Field label="Length (s)">{(ids) => (<Input {...ids} type="number" min={0.5} step={0.5} value={t.duration_s} onChange={(e) => patchTitle(i, { duration_s: Number(e.target.value) })} />)}</Field>
              <Button size="sm" variant="ghost" onClick={() => setTitles((x) => x.filter((_, j) => j !== i))}>
                Remove
              </Button>
            </div>
          ))}
        </Card>

        <Card>
          <div className="Prompt-cardHead">
            <h2>Output</h2>
          </div>
          <div className="Prompt-shotGrid Prompt-shotGrid--auto">
            <Field label="Aspect">{(ids) => (<select {...ids} className="rk-input cr-select" value={preset} onChange={(e) => setPreset(e.target.value as "youtube" | "reels")}>
                <option value="youtube">16:9 (YouTube)</option>
                <option value="reels">9:16 (vertical)</option>
              </select>)}</Field>
            <Field label="Render" note={"Render the finished edit at the end."}>{() => (<Toggle label="Render final" checked={render} onChange={setRender} />)}</Field>
          </div>
        </Card>

        {busy && progress && progress.stages.length > 0 && (
          <Card>
            {progress.stages.map((s, i) => (
              <ProgressRow key={s} label={s} status={i === progress.stages.length - 1 ? "Running" : "Done"} tone={i === progress.stages.length - 1 ? "info" : "ok"} />
            ))}
          </Card>
        )}
        {progress?.done && onOpenFinished && (
          <Card>
            <div className="Prompt-grRow">
              <Badge tone="ok">Finished edit ready</Badge>
              <Button variant="secondary" onClick={onOpenFinished}>
                Open finished edit
              </Button>
            </div>
          </Card>
        )}

        {!confirming ? (
          <div className="Prompt-actions">
            <Button variant="primary" disabled={disabled} onClick={() => setConfirming(true)}>
              Assemble
            </Button>
          </div>
        ) : (
          <Card className="Prompt-confirm">
            <div role="group" aria-label="Confirm paid generation" className="Prompt-confirmInner">
              <p className="m-0">
                This submits paid {hostAppName()} jobs ({shots.length} shot{shots.length === 1 ? "" : "s"}
                {narrates ? " plus narration" : ""}).
              </p>
              <p className="m-0" data-testid="spend-estimate">
                <strong>Estimate: {catalog ? `about ${formatUsd(spend.total)}` : "unavailable"}</strong>
                {catalog && spend.unpriced > 0
                  ? ` plus ${spend.unpriced} job${spend.unpriced === 1 ? "" : "s"} without a listed price`
                  : ""}
                . This is {hostAppName()}&apos;s own estimate; the final bill may differ.
              </p>
              <div className="Prompt-actions">
                <Button
                  variant="primary"
                  disabled={disabled}
                  onClick={() => {
                    setConfirming(false);
                    onSubmit(form);
                  }}
                >
                  Confirm and generate
                </Button>
                <Button variant="secondary" onClick={() => setConfirming(false)}>
                  Back
                </Button>
              </div>
            </div>
          </Card>
        )}
      </div>
    </PageShell>
  );
}

function DurationInput({ control, value, onChange }: { control: DurationControl; value: number; onChange: (d: number) => void }) {
  if (control.type === "set") {
    return (
      <Select
        label="Duration (s)"
        value={String(value)}
        options={control.options.map((d) => ({ value: String(d), label: String(d) }))}
        onChange={(next) => onChange(Number(next))}
      />
    );
  }
  const [min, max] = control.type === "range" ? [control.min, control.max] : [1, 30];
  return (
    <Input
      type="number"
      min={min}
      max={max}
      step={control.type === "range" ? 1 : 0.5}
      value={value}
      onChange={(e) => onChange(Number(e.target.value))}
    />
  );
}
