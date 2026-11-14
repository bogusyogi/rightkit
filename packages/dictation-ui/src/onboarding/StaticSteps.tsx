import { UserCircleMinus } from "@phosphor-icons/react";
import { hostAppName, hostComponents } from "../host.js";
import { FeatureIcon, SectionIntro } from "./Chrome.js";
import { PRIVACY_LINE, STEP_SURFACE } from "./constants.js";
import { onboardingWakeWord } from "./model.js";
import { IdlePillPreview } from "./PillPreview.js";
import type { OS } from "./types.js";

export function WelcomeStep() {
  return (
    <section className="hr-step grid min-h-full content-start justify-items-center px-4 pb-6 pt-4 text-center">
      <div className="mx-auto w-full max-w-[820px]">
        <h1 className="m-0 mx-auto max-w-[760px] [font-family:var(--font-display)] text-[28px] font-semibold leading-[1.04] tracking-[0] text-[var(--paper)] [text-wrap:balance] max-[760px]:text-[28px]">
          <span className="block">Free to own and use.</span>
          <span className="block">
            No limits. <span className="text-[var(--accent-hover)]">FOREVER.</span>
          </span>
        </h1>
        <p className="m-0 mx-auto mt-4 max-w-[700px] text-[18px] font-bold leading-[1.35] text-[var(--paper)] [text-wrap:balance]">
          {PRIVACY_LINE}
        </p>
        <p className="m-0 mx-auto mt-2.5 max-w-[620px] text-[15px] leading-[1.5] text-[var(--muted-2)] [text-wrap:balance]">
          We will walk through a quick setup so local dictation works cleanly from day one.
        </p>

        <div className="mx-auto mt-10 grid max-w-[640px] grid-cols-4 gap-4 max-[760px]:grid-cols-2">
          {[
            ["device", "On device"],
            ["offline", "Works offline"],
            ["account", "No account"],
            ["subscription", "No subscriptions"],
          ].map(([kind, label], i) => (
            <div
              key={kind}
              className="hr-stagger grid justify-items-center gap-3"
              style={{ animationDelay: `${120 + i * 80}ms` }}
            >
              {kind === "account" ? (
                <span className="grid h-[58px] w-[58px] place-items-center">
                  <UserCircleMinus className="h-9 w-9 text-[var(--accent-hover)]" weight="bold" aria-hidden="true" />
                </span>
              ) : (
                <FeatureIcon kind={kind as "device" | "offline" | "subscription"} />
              )}
              <strong className="whitespace-nowrap text-[15px] text-[var(--paper)]">{label}</strong>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}

export function CommandsStep({ platform }: { platform: OS }) {
  const wakeWord = onboardingWakeWord();
  const browser = platform === "macos" || platform === "ios" ? "Safari" : "Chrome";
  return (
    <section className="hr-step mx-auto grid h-full w-full max-w-[980px] content-start pb-2 pt-4 text-left">
      <div className="grid grid-cols-[minmax(0,0.82fr)_minmax(0,1.18fr)] items-center gap-8 max-[700px]:mx-auto max-[700px]:max-w-[560px] max-[700px]:grid-cols-1">
        <div className="grid gap-5">
          <div>
            <h2 className="m-0 max-w-[14ch] max-[700px]:max-w-none [font-family:var(--font-display)] text-[28px] font-semibold leading-[0.98] tracking-[0] text-[var(--paper)] max-[760px]:text-[28px]">
              Say controls inside dictation<span className="text-[var(--accent-hover)]">.</span>
            </h2>
            <p className="m-0 mt-4 max-w-[34ch] text-[13px] leading-[1.5] text-[var(--muted-2)]">
              Say a command and it runs. It never shows up in your text.
            </p>
          </div>

          <div className="rounded-[var(--radius-lg)] border border-[var(--line)] bg-[var(--surface-2)] p-4 shadow-[var(--surface-shadow)]">
            <p className="m-0 text-[11px] font-bold uppercase tracking-[0.1em] text-[var(--muted-2)]">
              Example
            </p>
            <p className="m-0 mt-2 text-[15px] font-bold leading-[1.45] text-[var(--paper)]">
              "Send this to Maya{" "}
              <code className="rounded-[var(--radius-sm)] bg-[var(--tint-accent)] px-1.5 py-0.5 [font-family:var(--font-mono)] text-[12px] text-[var(--accent-hover)]">
                {wakeWord} send
              </code>
              "
            </p>
            <p className="m-0 mt-2 text-[12px] leading-[1.4] text-[var(--muted-2)]">
              {hostAppName()} pastes the sentence and sends it. The command never appears.
            </p>
          </div>
        </div>

        <div className="grid gap-1.5">
          <section
            className="hr-stagger rounded-[var(--radius-lg)] border border-[var(--line)] bg-[var(--surface-2)] px-4 py-2.5 shadow-[var(--surface-shadow)]"
            style={{ animationDelay: "40ms" }}
          >
            <h3 className="m-0 text-[13px] font-bold text-[var(--paper)]">End dictation</h3>
            <div className="mt-2 grid grid-cols-3 gap-2">
              {[
                [`${wakeWord} stop`, "paste"],
                [`${wakeWord} send`, "paste + send"],
                [`${wakeWord} cancel`, "discard"],
              ].map(([command, copy]) => (
                <div key={command} className="grid gap-0.5 rounded-[var(--radius-md)] bg-[var(--surface-1)] px-3 py-1.5">
                  <code className="[font-family:var(--font-mono)] text-[12px] font-bold leading-tight text-[var(--accent-hover)]">
                    {command}
                  </code>
                  <span className="text-[11px] font-bold text-[var(--muted-2)]">{copy}</span>
                </div>
              ))}
            </div>
          </section>

          <section
            className="hr-stagger rounded-[var(--radius-lg)] border border-[var(--line)] bg-[var(--surface-2)] px-4 py-2.5 shadow-[var(--surface-shadow)]"
            style={{ animationDelay: "80ms" }}
          >
            <div className="flex items-center gap-2">
              <h3 className="m-0 text-[13px] font-bold text-[var(--paper)]">Inline commands</h3>
            </div>
            <div className="mt-2 flex flex-wrap gap-1.5">
              {[
                "new line",
                "new paragraph",
                "question mark",
                "exclamation mark",
                "open parenthesis",
                "close parenthesis",
              ].map((command) => (
                <span
                  key={command}
                  className="rounded-full bg-[color-mix(in_srgb,var(--line)_35%,transparent)] px-2.5 py-1 text-[12px] font-semibold [font-family:var(--font-mono)] text-[var(--paper)]"
                >
                  {command}
                </span>
              ))}
            </div>
          </section>

          <section
            className="hr-stagger rounded-[var(--radius-lg)] border border-[var(--line)] bg-[var(--surface-2)] px-4 py-2.5 shadow-[var(--surface-shadow)]"
            style={{ animationDelay: "160ms" }}
          >
            <div className="flex items-center gap-2">
              <h3 className="m-0 text-[13px] font-bold text-[var(--paper)]">Standalone commands</h3>
              <span className="rounded-full bg-[var(--tint-accent)] px-2 py-0.5 text-[11px] font-bold text-[var(--accent-hover)]">Pro</span>
            </div>
            <div className="mt-2 flex flex-wrap gap-1.5">
              {[
                `open ${browser}`,
                "open calculator",
                "screenshot",
                "switch window",
                "undo",
                "select all",
                "left click",
                "right click",
              ].map((command) => (
                <span
                  key={command}
                  className="rounded-full bg-[color-mix(in_srgb,var(--line)_35%,transparent)] px-2.5 py-1 text-[12px] font-semibold [font-family:var(--font-mono)] text-[var(--paper)]"
                >
                  {command}
                </span>
              ))}
            </div>
          </section>
        </div>
      </div>
    </section>
  );
}

export function AppLivesStep({ trayName, dockName }: { trayName: string; dockName: string }) {
  const AppMark = hostComponents().AppMark;
  const ios = trayName === "app";
  return (
    <section className={STEP_SURFACE}>
      <SectionIntro
        eyebrow=""
        title={ios ? "Where it runs" : "Where it lives"}
        copy={
          ios
            ? `Dictation runs inside the ${hostAppName()} app.`
            : `The hub is occasional. Daily dictation lives in the ${trayName}, the shortcut, and the pill.`
        }
      />
      <div className="mx-auto grid w-full max-w-[560px] gap-5 text-left">
        {!ios ? (
          <div className="hr-stagger grid grid-cols-[88px_1fr] items-center gap-5">
            <div className="grid place-items-center">
              <IdlePillPreview />
            </div>
            <div>
              <strong className="text-[15px] text-[var(--paper)]">Idle pill</strong>
              <p className="m-0 mt-1 text-[13px] leading-[1.45] text-[var(--muted-2)]">
                A small anchor above the {dockName}. It grows into a live meter while you dictate.
                Starts visible. Hide it until you record in Settings &gt; Application.
              </p>
            </div>
          </div>
        ) : null}

        <div
          className="hr-stagger grid grid-cols-[88px_1fr] items-center gap-5"
          style={{ animationDelay: ios ? "0ms" : "40ms" }}
        >
          <div className="grid place-items-center">
            {AppMark ? <AppMark /> : null}
          </div>
          <div>
            <strong className="text-[15px] text-[var(--paper)]">
              {ios ? `${hostAppName()} app` : `Hub and ${trayName}`}
            </strong>
            <p className="m-0 mt-1 text-[13px] leading-[1.45] text-[var(--muted-2)]">
              {ios
                ? "Open the app for dictation, model download, history, commands, vocabulary, and settings."
                : `The hub stays out of the ${dockName} by default. Open it from the ${trayName} for history, commands, vocabulary, or settings.`}
            </p>
          </div>
        </div>
      </div>
    </section>
  );
}
