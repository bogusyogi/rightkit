import { useState, type Dispatch, type SetStateAction } from "react";
import { cx } from "../lib/ui.js";
import { sectionClient } from "../settings/client.js";
import { WHISPER_LANGUAGE_COUNT } from "../settings/speech/options.js";
import { BUTTON, BUTTON_PRIMARY } from "./constants.js";
import { onboardingWakeWord } from "./model.js";
import { usePlatform } from "./platform.js";

// Compact, top-aligned surface (`hr-step` sets the top alignment and gap) so the
// email field stays above the fold at 900x560.
const TRIAL_SURFACE =
  "hr-step mx-auto grid h-full w-full max-w-[920px] content-start pb-3 pt-4 text-center";

type TrialPhase = "idle" | "sent" | "active";

type Props = {
  trialPhase: TrialPhase;
  trialEmail: string;
  trialKey: string;
  trialBusy: boolean;
  trialMsg: string;
  setTrialEmail: Dispatch<SetStateAction<string>>;
  setTrialKey: Dispatch<SetStateAction<string>>;
  requestTrial: () => Promise<void>;
  activateTrial: () => Promise<void>;
};

export function TrialStep({
  trialPhase,
  trialEmail,
  trialKey,
  trialBusy,
  trialMsg,
  setTrialEmail,
  setTrialKey,
  requestTrial,
  activateTrial,
}: Props) {
  const [hasKey, setHasKey] = useState(false);
  const keyEntryVisible = trialPhase === "sent" || hasKey;
  const platform = usePlatform();
  const trialDays = sectionClient("license").trialDays;
  const wakeWord = onboardingWakeWord();
  const commandsCopy =
    platform === "macos"
      ? 'Run actions by voice — "open Chrome", "screenshot", or any macOS Shortcut.'
      : 'Run actions by voice — "open Chrome", "screenshot", "switch window".';
  return (
    <section className={TRIAL_SURFACE}>
      <div className="mx-auto max-w-[640px]">
        <h2 className="m-0 [font-family:var(--font-display)] text-[28px] font-semibold leading-[1.05] tracking-[0] max-[760px]:text-[28px]">
          {trialPhase === "active" ? `Pro is on for ${trialDays} days` : `Try Pro free for ${trialDays} days`}
          <span className="text-[var(--accent-hover)]">.</span>
        </h2>
        <p className="m-0 mx-auto mt-2 max-w-[460px] text-[13px] leading-[1.45] text-[var(--muted-2)] [text-wrap:pretty]">
          {trialPhase === "active"
            ? "Every feature is on. No card, nothing to cancel."
            : "Drop your email to get a key. We do not collect card or payment information. No auto-renewal. Skip anytime."}
        </p>
      </div>
      <div className="mx-auto grid w-full max-w-[620px] grid-cols-2 gap-3 text-left max-[760px]:grid-cols-1">
        {[
          [platform === "macos" ? "Commands and Shortcuts" : "Voice commands", commandsCopy],
          ["AI polish", "Polishes dictation with AI. Sends text to your provider, using your own API key."],
          ["More languages", `Adds ${WHISPER_LANGUAGE_COUNT} languages on device, including locked single-language mode.`],
          ["Longer files", "Transcribe files longer than the free 10-minute limit."],
        ].map(([title, copy], i) => (
          <div
            key={title}
            className="hr-stagger rounded-[var(--radius-lg)] border border-[var(--line)] bg-[var(--surface-2)] px-4 py-3"
            style={{ animationDelay: `${i * 40}ms` }}
          >
            <strong className="text-[13px] text-[var(--paper)]">{title}</strong>
            <p className="m-0 mt-1 text-[12px] leading-[1.4] text-[var(--muted-2)]">{copy}</p>
          </div>
        ))}
      </div>
      {trialPhase === "active" ? (
        <p className="mx-auto inline-flex items-center gap-2 rounded-full bg-[var(--tint-ok)] px-4 py-2 text-[13px] font-bold text-[var(--ok-ink)]">
          Pro is on. Enjoy the next {trialDays} days.
        </p>
      ) : (
        <div className="mx-auto grid w-full max-w-[440px] gap-2.5">
          <div className="flex gap-2">
            <input
              type="email"
              name="email"
              aria-label="Email address"
              inputMode="email"
              autoComplete="email"
              placeholder="you@email.com"
              value={trialEmail}
              onChange={(e) => setTrialEmail(e.target.value)}
              className="min-h-[40px] flex-1 rounded-[var(--radius-md)] border border-[var(--line)] bg-[var(--surface-2)] px-3 text-[13px] text-[var(--paper)] outline-none focus:border-[var(--accent)]"
            />
            <button
              type="button"
              className={cx(
                BUTTON,
                BUTTON_PRIMARY,
                "whitespace-nowrap disabled:cursor-not-allowed disabled:opacity-50",
              )}
              onClick={() => void requestTrial()}
              disabled={trialBusy || !trialEmail.trim()}
            >
              {trialPhase === "sent" ? "Resend key" : trialBusy ? "Sending…" : "Email me my key"}
            </button>
          </div>
          {keyEntryVisible ? (
            <div className="hr-stagger flex gap-2">
              <input
                type="text"
                aria-label="Trial key"
                autoComplete="off"
                placeholder="Paste your key from the email"
                value={trialKey}
                onChange={(e) => setTrialKey(e.target.value)}
                className="min-h-[40px] flex-1 rounded-[var(--radius-md)] border border-[var(--line)] bg-[var(--surface-2)] px-3 [font-family:var(--font-mono)] text-[13px] text-[var(--paper)] outline-none focus:border-[var(--accent)]"
              />
              <button
                type="button"
                className={cx(BUTTON, BUTTON_PRIMARY, "whitespace-nowrap")}
                onClick={() => void activateTrial()}
                disabled={trialBusy || !trialKey.trim()}
              >
                {trialBusy ? "Activating…" : "Activate"}
              </button>
            </div>
          ) : (
            <button
              type="button"
              onClick={() => setHasKey(true)}
              className="justify-self-center text-[12px] font-bold text-[var(--accent-hover)] underline underline-offset-2 hover:text-[var(--paper)]"
            >
              I already have a key
            </button>
          )}
          {trialMsg ? (
            <p className="m-0 text-[12px] leading-[1.4] text-[var(--muted-2)]">{trialMsg}</p>
          ) : null}
          {/* Skipping lives ONLY in the wizard's corner Skip button — an inline
              "Skip, stay Free" here duplicated it. */}
        </div>
      )}

      <div className="mx-auto max-w-[560px] border-t border-[var(--line)] pt-2">
        <p className="m-0 text-[12px] leading-[1.5] text-[var(--muted)]">
          Local dictation, inline formatting and {wakeWord} stop/send/cancel are free. Pro adds longer
          files, more languages, app and system commands{platform === "macos" ? " and Shortcuts" : ""},
          and AI polish.
        </p>
      </div>
    </section>
  );
}
