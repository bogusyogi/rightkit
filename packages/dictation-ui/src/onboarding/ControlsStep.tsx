import { hostAppName } from "../host.js";
import { DICTATION_CHORD, pushToTalkChord, pushToTalkCopy } from "../lib/chords.js";
import { cx } from "../lib/ui.js";
import { useSettings } from "../hooks/useSettings.js";
import { SectionIntro } from "./Chrome.js";
import { formatChord } from "./chord.js";
import { ControlCard, PermissionBadge, ShortcutCard } from "./Controls.js";
import { BUTTON, BUTTON_SECONDARY, SIMPLE_ROW, STEP_SURFACE } from "./constants.js";
import type { OS, PermissionState, ShortcutKind } from "./types.js";

type Props = {
  mac: boolean;
  accessibilityCopy: string;
  accessibilityAction: string | undefined;
  accessibilityBusyLabel: string | undefined;
  accessibilityState: PermissionState;
  permissionBusy: "accessibility" | "keychain" | "mic" | null;
  platform: OS;
  liveChordKeys: string[];
  activeShortcut: ShortcutKind | null;
  shortcutSeen: Record<ShortcutKind, boolean>;
  ctrlHeld: boolean;
  controlSeen: { ptt: boolean; middle: boolean };
  middleClickPulse: boolean;
  permissionStatus: string;
  onRunAccessibilityFlow: () => void;
};

export function ControlsStep({
  mac,
  accessibilityCopy,
  accessibilityAction,
  accessibilityBusyLabel,
  accessibilityState,
  permissionBusy,
  platform,
  liveChordKeys,
  activeShortcut,
  shortcutSeen,
  ctrlHeld,
  controlSeen,
  middleClickPulse,
  permissionStatus,
  onRunAccessibilityFlow,
}: Props) {
  const ios = platform === "ios";
  const { get: getSetting } = useSettings();
  const pttKey =
    platform === "macos"
      ? (getSetting("mac_push_to_talk_key", "ctrl") as string) || "ctrl"
      : (getSetting("push_to_talk_key", "ralt") as string) || "ralt";
  return (
    <section className={STEP_SURFACE}>
      <SectionIntro
        eyebrow=""
        title="Try the controls"
        copy={
          mac
            ? "Accessibility lets macOS deliver fn shortcuts and paste back into the app you were using."
            : ios
              ? `iOS uses in-app record, send, and cancel controls. The system does not allow ${hostAppName()} to register desktop-style global shortcuts.`
              : "These are the same global shortcuts used after setup. Press each one once here and its card lights up."
        }
      />
      <div className="mx-auto grid w-full max-w-[900px] gap-6">
        {mac ? (
          <section>
            <div className="flex items-center justify-center gap-2">
              <span className="grid h-7 w-7 place-items-center rounded-full bg-[var(--accent)] text-[13px] font-bold text-[var(--accent-contrast)]">
                1
              </span>
              <h3 className="m-0 text-[18px] font-bold text-[var(--paper)]">Accessibility access</h3>
            </div>
            <div className={SIMPLE_ROW}>
              <div className="min-w-0">
                <strong className="text-[15px]">Accessibility</strong>
                <p className="m-0 mt-1 text-[13px] leading-[1.4] text-[var(--muted-2)] [text-wrap:pretty]">
                  {accessibilityCopy}
                </p>
              </div>
              {accessibilityAction ? (
                <button
                  type="button"
                  className={cx(
                    BUTTON,
                    BUTTON_SECONDARY,
                    "min-h-[36px] whitespace-nowrap px-3 text-[12px]",
                  )}
                  onClick={onRunAccessibilityFlow}
                  disabled={permissionBusy === "accessibility"}
                >
                  {accessibilityBusyLabel}
                </button>
              ) : (
                <PermissionBadge state={accessibilityState} />
              )}
            </div>
          </section>
        ) : null}

        <section>
          {mac ? (
            <div className="flex items-center justify-center gap-2">
              <span className="grid h-7 w-7 place-items-center rounded-full bg-[var(--accent)] text-[13px] font-bold text-[var(--accent-contrast)]">
                2
              </span>
              <h3 className="m-0 text-[18px] font-bold text-[var(--paper)]">The controls</h3>
            </div>
          ) : null}
          <div className={`${mac ? "mt-4 " : ""}grid grid-cols-3 gap-5 max-[760px]:grid-cols-1`}>
            <div className="hr-stagger h-full">
              <ShortcutCard
                title="Start / stop"
                chord={DICTATION_CHORD}
                copy={
                  ios
                    ? "Use the in-app record control to start dictation, then stop to keep the transcript."
                    : "Tap to start dictation, tap again to stop and paste."
                }
                os={platform}
                activeKeys={liveChordKeys}
                active={activeShortcut === "toggle"}
                seen={shortcutSeen.toggle}
              />
            </div>
            {!ios ? (
              <div className="hr-stagger h-full" style={{ animationDelay: "40ms" }}>
                <ControlCard
                  title="Push-to-talk"
                  label={formatChord(pushToTalkChord(platform, pttKey), platform).join(" + ")}
                  copy={pushToTalkCopy(platform, pttKey)}
                  active={ctrlHeld}
                  seen={controlSeen.ptt}
                />
              </div>
            ) : null}
            {!ios ? (
              <div className="hr-stagger h-full" style={{ animationDelay: "80ms" }}>
                <ControlCard
                  title="Middle-click"
                  label="Middle button"
                  copy="Click the middle mouse button to start or stop. On links, opens a new tab instead of recording."
                  active={middleClickPulse}
                  seen={controlSeen.middle}
                />
              </div>
            ) : null}
          </div>
          <p className="m-0 mt-3 text-[12px] leading-[1.35] text-[var(--muted-2)]">
            {ios ? (
              `Dictation runs inside the ${hostAppName()} app.`
            ) : (
              <>
                Push-to-talk and middle-click are on by default; switch them off anytime in{" "}
                <span className="font-bold text-[var(--paper)]">Controls</span>.
              </>
            )}
          </p>
        </section>
      </div>
      {/* The mic step's success lines are not news on the controls step; failures still show. */}
      {permissionStatus &&
      permissionStatus !== "Audio detected." &&
      permissionStatus !== "Microphone is ready." ? (
        <p className="m-0 text-[12px] leading-[1.4] text-[var(--muted-2)]">{permissionStatus}</p>
      ) : null}
    </section>
  );
}
