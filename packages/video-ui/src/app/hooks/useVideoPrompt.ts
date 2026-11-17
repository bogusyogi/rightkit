import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { engineCall, errorMessage } from "../../lib/engine.js";
import { genRightCallFailed, interpretHost, type HostInfo } from "../lib/genright.js";
import { durationControl, findModel, ofKind, parseCatalog, pickModel, providersOf, spendTotal, type DurationControl, type ModelCatalog, type ModelKind, type ModelRow } from "../lib/models.js";
import type { AssembleForm, ShotInput, TitleDraft } from "../lib/assemble.js";

const defaults = {
  video: { provider: "fal", model: "fal-ai/veo3.1/fast" },
  image: { provider: "fal", model: "fal-ai/flux-2/klein/9b" },
  voice: { provider: "elevenlabs", model: "eleven_v4_turbo" },
};
export const videoPromptOptions = {
  shotCounts: ["1", "2", "3", "4", "5", "6"],
  shotKinds: ["video", "image"],
  titleTemplates: ["lower_third", "centered", "caption"],
  titleTextLimit: 160,
  titleStart: { min: 0, step: 0.5 },
  titleDuration: { min: 0.5, step: 0.5 },
  presets: ["youtube", "reels"],
} as const;
export type VideoPromptShot = ShotInput & { prompt: string };
export type VideoPromptDuration = Extract<DurationControl, { type: "set" }> | { type: "number"; min: number; max: number; step: number };

export const videoPromptTitleHasSubtitle = (template: TitleDraft["template"]) => template === "lower_third";

function fitDuration(control: DurationControl, value: number) {
  if (control.type === "set") return control.options.includes(value) ? value : control.initial;
  if (control.type === "range") return value >= control.min && value <= control.max ? value : control.initial;
  return value;
}
function newShot(kind: VideoPromptShot["kind"], catalog: ModelCatalog | null): VideoPromptShot {
  const model = catalog ? pickModel(catalog, kind, defaults[kind]) : undefined;
  return {
    prompt: "", kind, duration_s: kind === "video" ? durationControl(model).initial : 4,
    provider: model?.provider ?? defaults[kind].provider,
    model: model?.model ?? defaults[kind].model,
  };
}
function fitShot(shot: VideoPromptShot, catalog: ModelCatalog): VideoPromptShot {
  const model = findModel(catalog, shot.provider, shot.model);
  if (!model || model.kind !== shot.kind) return { ...newShot(shot.kind, catalog), prompt: shot.prompt };
  return shot.kind === "video" ? { ...shot, duration_s: fitDuration(durationControl(model), shot.duration_s) } : shot;
}

export function useVideoPrompt({ busy, onSubmit }: { busy: boolean; onSubmit: (form: AssembleForm) => void }) {
  const [{ host, generation }, setHostCheck] = useState<{ host: HostInfo; generation: number }>({ host: { state: "checking" }, generation: 0 });
  const [prompt, setPrompt] = useState("");
  const [catalog, setCatalog] = useState<ModelCatalog | null>(null);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [shots, setShots] = useState<VideoPromptShot[]>([newShot("video", null), newShot("video", null)]);
  const [narrate, setNarrate] = useState(true);
  const [narration, setNarration] = useState("");
  const [voice, setVoice] = useState({ ...defaults.voice, voice: "" });
  const [titles, setTitles] = useState<TitleDraft[]>([]);
  const [preset, setPreset] = useState<"youtube" | "reels">("youtube");
  const [render, setRender] = useState(true);
  const [confirming, setConfirming] = useState(false);
  const lifetime = useRef<object | null>(null);
  const checkSequence = useRef(0);
  const confirmation = useRef(false);
  useLayoutEffect(() => {
    const token = {};
    lifetime.current = token;
    return () => { if (lifetime.current === token) lifetime.current = null; };
  }, []);
  const check = useCallback(async () => {
    const token = lifetime.current;
    if (!token) return;
    const generation = ++checkSequence.current;
    setHostCheck({ host: { state: "checking" }, generation });
    let next: HostInfo;
    try {
      next = interpretHost(await engineCall("genright.status", {}));
    } catch (e) {
      next = genRightCallFailed(errorMessage(e));
    }
    if (lifetime.current === token && checkSequence.current === generation) setHostCheck({ host: next, generation });
  }, []);
  useEffect(() => { void check(); }, [check]);
  const ready = host.state === "ready";
  useEffect(() => {
    if (!ready) return;
    const token = lifetime.current;
    let live = true;
    const current = () => live && lifetime.current === token && checkSequence.current === generation;
    setCatalogError(null);
    void engineCall("genright.models", {}).then((result) => {
      if (!current()) return;
      const next = parseCatalog(result);
      setCatalog(next);
      setShots((old) => old.map((shot) => fitShot(shot, next)));
      setVoice((old) => {
        const model = findModel(next, old.provider, old.model) ?? pickModel(next, "voice", defaults.voice);
        return model ? { ...old, provider: model.provider, model: model.model } : old;
      });
    }).catch((error) => {
      if (!current()) return;
      setCatalog(null);
      setCatalogError(errorMessage(error));
    });
    return () => { live = false; };
  }, [generation, ready]);
  const setCount = (value: number) => {
    const count = Math.max(1, Math.min(videoPromptOptions.shotCounts.length, Math.trunc(value) || 1));
    setShots((old) => count <= old.length
      ? old.slice(0, count)
      : [...old, ...Array.from({ length: count - old.length }, () => newShot("video", catalog))]);
  };
  const patchShot = (index: number, patch: Partial<VideoPromptShot>) => setShots((old) => old.map((shot, i) => i === index ? { ...shot, ...patch } : shot));
  const setKind = (index: number, kind: VideoPromptShot["kind"]) => {
    const { provider, model, duration_s } = newShot(kind, catalog);
    patchShot(index, { kind, provider, model, duration_s });
  };
  const setShotModel = (index: number, model: ModelRow) => setShots((old) => old.map((shot, i) => i === index ? {
    ...shot, provider: model.provider, model: model.model,
    duration_s: shot.kind === "video" ? fitDuration(durationControl(model), shot.duration_s) : shot.duration_s,
  } : shot));
  const patchVoice = (patch: Partial<typeof voice>) => setVoice((old) => ({ ...old, ...patch }));
  const patchTitle = (index: number, patch: Partial<TitleDraft>) => setTitles((old) => old.map((title, i) => i === index ? { ...title, ...patch } : title));
  const addTitle = () => setTitles((old) => [...old, { template: "lower_third", text: "", subtitle: "", start_s: 0, duration_s: 3 }]);
  const removeTitle = (index: number) => setTitles((old) => old.filter((_, i) => i !== index));
  const modelChoices = (kind: ModelKind, provider: string) => ({
    providers: catalog ? providersOf(catalog, kind) : [],
    models: catalog ? ofKind(catalog, kind).filter((model) => model.provider === provider) : [],
  });
  const chooseModel = (kind: ModelKind, provider: string, model?: string) => catalog
    ? ofKind(catalog, kind).find((row) => row.provider === provider && (model === undefined || row.model === model))
    : undefined;
  const shotDuration = (shot: VideoPromptShot): VideoPromptDuration => {
    if (shot.kind === "video") {
      const control = durationControl(findModel(catalog, shot.provider, shot.model));
      if (control.type === "set") return control;
      if (control.type === "range") return { type: "number", min: control.min, max: control.max, step: 1 };
    }
    return { type: "number", min: 1, max: 30, step: 0.5 };
  };
  const form = useMemo<AssembleForm>(() => ({
    prompt, shots, narration: narrate ? { text: narration, ...voice } : null, titles, preset, render,
  }), [prompt, shots, narrate, narration, voice, titles, preset, render]);
  const narrates = narrate && narration.trim().length > 0;
  const spend = useMemo(() => {
    const lines = shots.map((shot, index) => ({ label: String(index), estUsd: findModel(catalog, shot.provider, shot.model)?.estUsd ?? null }));
    if (narrates) lines.push({ label: "voice", estUsd: findModel(catalog, voice.provider, voice.model)?.estUsd ?? null });
    return spendTotal(lines);
  }, [catalog, shots, voice, narrates]);
  const titlesOk = titles.every((title) => title.text.trim().length > 0 && title.text.length <= videoPromptOptions.titleTextLimit
    && title.duration_s > 0 && title.start_s >= videoPromptOptions.titleStart.min);
  const valid = prompt.trim().length > 0 && shots.every((shot) => !!shot.provider && !!shot.model && shot.duration_s > 0) && titlesOk;
  const disabled = !ready || busy || !valid;
  const requestConfirmation = () => {
    if (disabled) return;
    confirmation.current = true;
    setConfirming(true);
  };
  const cancelConfirmation = () => {
    confirmation.current = false;
    setConfirming(false);
  };
  const confirmSubmit = () => {
    if (!confirmation.current || disabled || !lifetime.current) return false;
    cancelConfirmation();
    onSubmit(form);
    return true;
  };
  return {
    host, ready, check, hasCatalog: catalog !== null, catalogError,
    prompt, setPrompt, shots, setCount, patchShot, setKind, setShotModel, shotDuration, modelChoices, chooseModel,
    narrate, setNarrate, narration, setNarration, voice, patchVoice, titles, addTitle, removeTitle, patchTitle,
    preset, setPreset, render, setRender, narrates, spend, disabled, confirming, requestConfirmation, cancelConfirmation, confirmSubmit,
  };
}
