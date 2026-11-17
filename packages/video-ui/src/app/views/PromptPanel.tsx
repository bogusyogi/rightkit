import { Badge, Button, Card, Field, Input, PageShell, SegmentedControl, Select, Toggle } from "@rightkit/react-shell/react";
import type { ProgressState } from "../hooks/useProgress.js";
import { useVideoPrompt, videoPromptOptions, videoPromptTitleHasSubtitle, type VideoPromptDuration, type VideoPromptShot } from "../hooks/useVideoPrompt.js";
import type { AssembleForm, TitleDraft } from "../lib/assemble.js";
import { formatUsd, type ModelRow } from "../lib/models.js";
import { ErrorPanel, ProgressRow, Icon } from "../ui/index.js";
import { hostAppName } from "../appName.js";

const TITLE_LABELS: Record<TitleDraft["template"], string> = {
  lower_third: "Lower third",
  centered: "Centered",
  caption: "Caption",
};
const SHOT_LABELS: Record<VideoPromptShot["kind"], string> = { video: "Video", image: "Image" };
const PRESET_LABELS = { youtube: "16:9 (YouTube)", reels: "9:16 (vertical)" };
type ShotCount = (typeof videoPromptOptions.shotCounts)[number];

function modelOptionLabel(m: ModelRow): string {
  return `${m.label}${m.estUsd !== null ? ` (${formatUsd(m.estUsd)})` : ""}`;
}

function ModelPicker({
  choices,
  choose,
  provider,
  model,
  onChange,
  providerLabel = "Provider",
  modelLabel = "Model",
}: {
  choices: { providers: string[]; models: ModelRow[] };
  choose: (provider: string, model?: string) => ModelRow | undefined;
  provider: string;
  model: string;
  onChange: (m: ModelRow) => void;
  providerLabel?: string;
  modelLabel?: string;
}) {
  const { providers, models } = choices;
  const pick = (provider: string, model?: string) => {
    const selection = choose(provider, model);
    if (selection) onChange(selection);
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
  const {
    host, ready, check, hasCatalog, catalogError, prompt, setPrompt, shots, setCount, patchShot,
    setKind, setShotModel, shotDuration, modelChoices, chooseModel, narrate, setNarrate,
    narration, setNarration, voice, patchVoice, titles, addTitle, removeTitle, patchTitle,
    preset, setPreset, render, setRender, narrates, spend, disabled, confirming,
    requestConfirmation, cancelConfirmation, confirmSubmit,
  } = useVideoPrompt({ busy, onSubmit });
  const gr = host.malformedStatus ? { ...host, detail: hostAppName() + " did not report ready." } : host;

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
              value={String(shots.length) as ShotCount}
              options={videoPromptOptions.shotCounts.map((value) => ({ value, label: value }))}
              onChange={(v) => setCount(Number(v))}
            />
          </div>
          {shots.map((s, i) => (
            <div key={i} className="Prompt-shotRow">
              <div className="Prompt-shotGrid">
                <Field label={`Shot ${i + 1} · kind`}>{(ids) => (<select {...ids} className="rk-input cr-select" aria-label={`Shot ${i + 1} kind`} value={s.kind} onChange={(e) => setKind(i, e.target.value as VideoPromptShot["kind"])}>
                    {videoPromptOptions.shotKinds.map((kind) => <option key={kind} value={kind}>{SHOT_LABELS[kind]}</option>)}
                  </select>)}</Field>
                {hasCatalog ? (
                  <ModelPicker
                    choices={modelChoices(s.kind, s.provider)}
                    choose={(provider, model) => chooseModel(s.kind, provider, model)}
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
                    control={shotDuration(s)}
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
                {hasCatalog ? (
                  <ModelPicker
                    choices={modelChoices("voice", voice.provider)}
                    choose={(provider, model) => chooseModel("voice", provider, model)}
                    provider={voice.provider}
                    model={voice.model}
                    providerLabel="Voice provider"
                    modelLabel="Voice model"
                    onChange={(m) => patchVoice({ provider: m.provider, model: m.model })}
                  />
                ) : (
                  <>
                    <Field label="Voice provider">{(ids) => (<Input {...ids} value={voice.provider} onChange={(e) => patchVoice({ provider: e.target.value })} />)}</Field>
                    <Field label="Voice model">{(ids) => (<Input {...ids} value={voice.model} onChange={(e) => patchVoice({ model: e.target.value })} />)}</Field>
                  </>
                )}
                <Field label="Voice" note={"Optional"}>{(ids) => (<Input {...ids} value={voice.voice} onChange={(e) => patchVoice({ voice: e.target.value })} />)}</Field>
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
              onClick={addTitle}
            ><Icon name="plus" />
              Add title
            </Button>
          </div>
          {titles.map((t, i) => (
            <div key={i} className="Prompt-titleRow">
              <Field label="Template">{(ids) => (<select {...ids} className="rk-input cr-select" value={t.template} onChange={(e) => patchTitle(i, { template: e.target.value as TitleDraft["template"] })}>
                  {videoPromptOptions.titleTemplates.map((template) => (
                    <option key={template} value={template}>
                      {TITLE_LABELS[template]}
                    </option>
                  ))}
                </select>)}</Field>
              <Field label={videoPromptTitleHasSubtitle(t.template) ? "Title" : "Text"}>{(ids) => (<Input {...ids} maxLength={videoPromptOptions.titleTextLimit} value={t.text} onChange={(e) => patchTitle(i, { text: e.target.value })} />)}</Field>
              {videoPromptTitleHasSubtitle(t.template) && (
                <Field label="Subtitle" note={"Optional"}>{(ids) => (<Input {...ids} maxLength={videoPromptOptions.titleTextLimit} value={t.subtitle} onChange={(e) => patchTitle(i, { subtitle: e.target.value })} />)}</Field>
              )}
              <Field label="Start (s)">{(ids) => (<Input {...ids} type="number" {...videoPromptOptions.titleStart} value={t.start_s} onChange={(e) => patchTitle(i, { start_s: Number(e.target.value) })} />)}</Field>
              <Field label="Length (s)">{(ids) => (<Input {...ids} type="number" {...videoPromptOptions.titleDuration} value={t.duration_s} onChange={(e) => patchTitle(i, { duration_s: Number(e.target.value) })} />)}</Field>
              <Button size="sm" variant="ghost" onClick={() => removeTitle(i)}>
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
                {videoPromptOptions.presets.map((preset) => <option key={preset} value={preset}>{PRESET_LABELS[preset]}</option>)}
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
            <Button variant="primary" disabled={disabled} onClick={requestConfirmation}>
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
                <strong>Estimate: {hasCatalog ? `about ${formatUsd(spend.total)}` : "unavailable"}</strong>
                {hasCatalog && spend.unpriced > 0
                  ? ` plus ${spend.unpriced} job${spend.unpriced === 1 ? "" : "s"} without a listed price`
                  : ""}
                . This is {hostAppName()}&apos;s own estimate; the final bill may differ.
              </p>
              <div className="Prompt-actions">
                <Button
                  variant="primary"
                  disabled={disabled}
                  onClick={confirmSubmit}
                >
                  Confirm and generate
                </Button>
                <Button variant="secondary" onClick={cancelConfirmation}>
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

function DurationInput({ control, value, onChange }: { control: VideoPromptDuration; value: number; onChange: (d: number) => void }) {
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
  return (
    <Input
      type="number"
      min={control.min}
      max={control.max}
      step={control.step}
      value={value}
      onChange={(e) => onChange(Number(e.target.value))}
    />
  );
}
