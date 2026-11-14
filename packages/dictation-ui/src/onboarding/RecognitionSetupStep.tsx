import { Check, Microphone, WarningCircle } from "@phosphor-icons/react";
import { hostAppName } from "../host.js";
import { SectionIntro } from "./Chrome.js";
import { BUTTON, BUTTON_PRIMARY, BUTTON_SECONDARY, PRIVACY_LINE, STEP_SURFACE } from "./constants.js";
import type { OnboardingRecognitionSetupController } from "./useRecognitionSetup.js";

const SCRIPT =
  "Today I am testing this microphone with a short and simple passage. I will speak at a calm and steady pace. The room feels quiet, and my voice sounds clear. A small bird rests beside the garden path. Soft light falls across the green leaves. I can describe each detail without rushing. This test helps the app select a stable speech system for this computer. I will finish the passage now and wait while the app checks the result.";

export function RecognitionSetupStep({
  setup,
  context = "onboarding",
}: {
  setup: OnboardingRecognitionSetupController;
  context?: "onboarding" | "developer";
}) {
  const developer = context === "developer";
  const recording = setup.phase === "recording";
  const processing = setup.phase === "processing";
  const failed = setup.phase === "failed";
  const passed = setup.phase === "passed";

  return (
    <section className={STEP_SURFACE} aria-busy={processing}>
      <SectionIntro
        eyebrow={developer ? "Recognition route test" : ""}
        title={
          passed
            ? developer
              ? "Recognition route saved"
              : "Recognition is ready"
            : failed
              ? "Recognition needs attention"
              : "Read this short script"
        }
        copy={
          passed
            ? developer
              ? "Test found a working route and saved it for future dictation."
              : "Setup found a working recognition route. You are ready to dictate."
            : failed
              ? developer
                ? "Diagnostic evidence was saved. Try again or send diagnostics from support."
                : "Finish onboarding, then contact support from the Hub so we can help."
              : "Read every line at a normal pace. It takes about 35 seconds."
        }
      />

      {passed || failed ? (
        <div
          className={`mx-auto grid w-full max-w-[610px] gap-3 rounded-[var(--radius-xl)] border px-5 py-5 text-left ${
            passed ? "border-[var(--ok-ink)] bg-[var(--tint-ok)]" : "border-[var(--danger)] bg-[var(--tint-danger)]"
          }`}
          role="status"
          aria-live="polite"
        >
          <div className="flex items-start gap-3">
            {passed ? (
              <Check className="mt-0.5 h-5 w-5 shrink-0 text-[var(--ok-ink)]" weight="bold" aria-hidden="true" />
            ) : (
              <WarningCircle className="mt-0.5 h-5 w-5 shrink-0 text-[var(--danger)]" weight="fill" aria-hidden="true" />
            )}
            <div>
              <p className="m-0 text-[15px] font-bold text-[var(--paper)]">
                {passed ? "Setup complete" : "We could not confirm speech recognition"}
              </p>
              <p className="m-0 mt-1 text-[13px] leading-[1.45] text-[var(--muted)]">
                {passed
                  ? developer
                    ? `You can close this test and continue using ${hostAppName()}.`
                    : `Continue to open ${hostAppName()}.`
                  : developer
                    ? "Use Help → Contact support. Test details have been saved for support."
                    : "Use Hub → Help → Contact support after onboarding. Setup details have been saved for support."}
              </p>
            </div>
          </div>
          {failed || (developer && passed) ? (
            <button
              type="button"
              onClick={() => void setup.start()}
              className={`${BUTTON} ${BUTTON_SECONDARY} justify-self-start`}
            >
              {passed ? "Run test again" : "Try setup again"}
            </button>
          ) : null}
        </div>
      ) : (
        <div className="mx-auto grid w-full max-w-[680px] gap-3 text-left">
          <blockquote className="m-0 rounded-[var(--radius-xl)] border border-[var(--line)] bg-[var(--surface-2)] px-5 py-4 text-[15px] leading-[1.62] text-[var(--paper)] shadow-[var(--surface-shadow)] max-[760px]:text-[13px]">
            {SCRIPT}
          </blockquote>
          <div className="flex flex-wrap items-center justify-center gap-3">
            <p className="m-0 text-center text-[12px] leading-[1.45] text-[var(--muted)]">
              {PRIVACY_LINE}
            </p>
            {recording ? (
              <span className="inline-flex items-center gap-2 rounded-full bg-[var(--tint-accent)] px-3 py-1.5 text-[12px] font-bold text-[var(--accent-hover)]" aria-live="polite">
                <span className="h-2 w-2 animate-pulse rounded-full bg-[var(--accent)]" />
                Recording {setup.elapsed}s
              </span>
            ) : null}
          </div>
          {processing ? (
            <div className="flex items-center justify-center gap-3 py-3" role="status" aria-live="polite">
              <span className="h-5 w-5 animate-spin rounded-full border-2 border-[var(--line)] border-t-[var(--accent)]" />
              <span className="text-[13px] font-bold text-[var(--muted)]">{setup.progress || "Processing…"}</span>
            </div>
          ) : (
            <div className="flex flex-wrap justify-center gap-3">
              {recording ? (
                <button
                  type="button"
                  disabled={!setup.readyToFinish}
                  onClick={() => void setup.finish()}
                  className={`${BUTTON} ${BUTTON_PRIMARY}`}
                >
                  {setup.readyToFinish ? "Finish recording" : `Keep reading · ${Math.max(0, setup.minSeconds - setup.elapsed)}s`}
                </button>
              ) : (
                <button type="button" onClick={() => void setup.start()} className={`${BUTTON} ${BUTTON_PRIMARY} gap-2`}>
                  <Microphone className="h-4 w-4" weight="fill" aria-hidden="true" />
                  {developer ? "Start route test" : "Start setup recording"}
                </button>
              )}
            </div>
          )}
          <p className="m-0 text-center text-[11px] text-[var(--muted-2)]">
            Read whole script, then finish recording. Target: {setup.targetSeconds} seconds.
          </p>
        </div>
      )}
    </section>
  );
}
