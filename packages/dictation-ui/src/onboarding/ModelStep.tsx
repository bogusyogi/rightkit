import { Suspense, useEffect, useRef, useState } from "react";
import { hostComponents, openPricing } from "../host.js";
import { TDT_LANGUAGES, WHISPER_LANGUAGE_COUNT } from "../settings/speech/options.js";
import { SectionIntro } from "./Chrome.js";
import { PRIVACY_LINE } from "./constants.js";
import { trackModelInstall } from "./flow.js";
import { onboardingModel } from "./model.js";
import { usePlatform } from "./platform.js";

/** The part of the models port the extras read: the row's state, now and as it changes. */
export interface ModelStateSource {
  list(): Promise<readonly { state: string }[]>;
  subscribe?(handler: (entry: { state: string }) => void): Promise<() => void>;
}

// The language chips show the first few; the rest are a count.
const CHIP_LANGUAGES = 7;

// The Model step is RightKit's download step with this as its intro: what is about to be installed and why it
// covers the user, then the download row itself. There is no separate "Download" step.
export function ModelIntro() {
  // The pinned archive is 317 MB on Windows and 391 MB on macOS, rounded to the nearest 10.
  const model = onboardingModel();
  const size = usePlatform() === "macos" ? model.size.macos : model.size.other;
  const moreLanguages = TDT_LANGUAGES.length - CHIP_LANGUAGES;
  return (
    <div className="grid justify-items-center gap-3 text-center">
      <SectionIntro
        eyebrow=""
        title="Your dictation model"
        copy={`${model.label} runs on your device: English plus ${TDT_LANGUAGES.length - 1} more languages, ${size}, downloaded once. ${PRIVACY_LINE}`}
      />
      <div className="flex max-w-[640px] flex-wrap justify-center gap-1.5">
        {TDT_LANGUAGES.slice(0, CHIP_LANGUAGES).map((lang) => (
          <span
            key={lang}
            className="rounded-full border border-[var(--line)] bg-[var(--surface-2)] px-2.5 py-1 text-[12px] font-bold leading-none text-[var(--paper)]"
          >
            {lang}
          </span>
        ))}
        <span className="px-1 py-1 text-[12px] font-bold leading-none text-[var(--muted-2)]">+{moreLanguages} more</span>
      </div>
      <p className="m-0 text-[13px] leading-[1.4] text-[var(--muted)]">
        Need another language? <span className="font-bold text-[var(--paper)]">Pro</span> adds {WHISPER_LANGUAGE_COUNT} on
        device.{" "}
        <button
          type="button"
          onClick={() => void openPricing()}
          className="font-bold text-[var(--accent-hover)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)]"
        >
          See Pro →
        </button>
      </p>
    </div>
  );
}

// The reel and the model credit under RightKit's download row. The reel plays while the model is not installed
// (the 0.1.213 download screen); when a download completes while this step is open, `onInstalled` fires once,
// after a beat, so setup moves on by itself as it did in 0.1.213. A model that was already installed when the
// step opened (relaunch, Back) never fires it. `isActive` tells the caller whether this step is still the one
// on screen (it turns false when the step unmounts, e.g. on Back or a rail click).
export function ModelDownloadExtras({
  models,
  onInstalled,
}: {
  models: ModelStateSource;
  onInstalled: (isActive: () => boolean) => void;
}) {
  const [state, setState] = useState<string>("unknown");
  const onInstalledRef = useRef(onInstalled);
  onInstalledRef.current = onInstalled;
  const mountedRef = useRef(true);
  const sawNotInstalledRef = useRef(false);
  const [advance, setAdvance] = useState(false);
  useEffect(() => {
    mountedRef.current = true;
    return () => { mountedRef.current = false; };
  }, []);
  useEffect(() => {
    let live = true;
    let off: (() => void) | undefined;
    void models.list().then((list) => { if (live) setState(list[0]?.state ?? "missing"); }, () => undefined);
    void models.subscribe?.((entry) => { if (live) setState(entry.state); }).then((unsubscribe) => {
      if (live) off = unsubscribe;
      else unsubscribe();
    }, () => undefined);
    return () => { live = false; off?.(); };
  }, [models]);
  const installed = state === "installed";
  const credit = onboardingModel().credit;
  const ModelReel = hostComponents().ModelReel;
  useEffect(() => {
    const next = trackModelInstall(sawNotInstalledRef.current, state);
    sawNotInstalledRef.current = next.sawNotInstalled;
    setAdvance(next.advance);
  }, [state]);
  useEffect(() => {
    if (!advance) return;
    const timer = window.setTimeout(() => onInstalledRef.current(() => mountedRef.current), 900);
    return () => window.clearTimeout(timer);
  }, [advance]);
  return (
    <div className="grid w-full justify-items-center gap-3">
      {installed || state === "unknown" || !ModelReel ? null : (
        <Suspense fallback={<div className="h-[148px] w-full max-w-[560px]" aria-hidden="true" />}>
          <ModelReel className="mx-auto w-full max-w-[560px] shadow-[var(--surface-shadow)]" />
        </Suspense>
      )}
      <p className="m-0 text-center text-[12px] leading-[1.4] text-[var(--muted-2)]">
        Powered by <span className="font-bold text-[var(--muted)]">{credit.name}</span> ·{" "}
        <a href={credit.licenseUrl} className="font-semibold text-[var(--accent-hover)] underline">
          {credit.licenseLabel}
        </a>
      </p>
    </div>
  );
}
