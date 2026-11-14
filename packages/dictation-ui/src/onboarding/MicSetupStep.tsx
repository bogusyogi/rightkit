import { useState } from "react";
import { cx } from "../lib/ui.js";
import { MicDevicePicker } from "../settings/MicDevicePicker.js";
import { PermissionBadge } from "./Controls.js";
import { BUTTON, BUTTON_SECONDARY, PLAIN_SECTION, SIMPLE_ROW, STEP_SURFACE } from "./constants.js";
import { MicMeter } from "./PillPreview.js";
import type { MicCheckState, PermissionState } from "./types.js";

type Props = {
  mac: boolean;
  keychainTitle: string;
  keychainCopy: string;
  keychainAction: string | undefined;
  permissionBusy: "accessibility" | "keychain" | "mic" | null;
  keychainState: PermissionState;
  micAction: string | undefined;
  micState: PermissionState;
  micCheckState: MicCheckState;
  micSignalSeen: boolean;
  inputDevice: string | undefined;
  micRefreshToken: number;
  permissionStatus: string;
  onRunKeychainFlow: () => void;
  onRunMicFlow: () => void;
  onMicDeviceChange: (value: string) => void;
  onRetryMicCheck: () => void;
};

export function MicSetupStep({
  mac,
  keychainTitle,
  keychainCopy,
  keychainAction,
  permissionBusy,
  keychainState,
  micAction,
  micState,
  micCheckState,
  micSignalSeen,
  inputDevice,
  micRefreshToken,
  permissionStatus,
  onRunKeychainFlow,
  onRunMicFlow,
  onMicDeviceChange,
  onRetryMicCheck,
}: Props) {
  const [micDeviceLabel, setMicDeviceLabel] = useState("System default microphone");

  return (
    <section className={STEP_SURFACE}>
      <div className="mx-auto max-w-[840px]">
        <h2 className="m-0 [font-family:var(--font-display)] text-[28px] font-semibold leading-none tracking-[0] text-[var(--paper)] [text-wrap:balance]">
          First, make sure the mic can hear you
          <span className="text-[var(--accent-hover)]">.</span>
        </h2>
        <p className="m-0 mx-auto mt-3 max-w-[760px] text-[15px] leading-[1.45] text-[var(--muted-2)] [text-wrap:pretty]">
          {mac
            ? "Approve Keychain and microphone access. The meter only confirms your microphone is sending signal. It does not transcribe."
            : "The meter only confirms your microphone is sending signal. It does not transcribe."}
        </p>
      </div>

      {mac ? (
        <section className={PLAIN_SECTION}>
          <div className="flex items-center justify-center gap-2">
            <span className="grid h-7 w-7 place-items-center rounded-full bg-[var(--accent)] text-[13px] font-bold text-[var(--accent-contrast)]">
              1
            </span>
            <h3 className="m-0 text-[18px] font-bold text-[var(--paper)]">Keychain access</h3>
          </div>
          <div className={SIMPLE_ROW}>
            <div className="min-w-0">
              <strong className="text-[15px]">{keychainTitle}</strong>
              <p className="m-0 mt-1 text-[13px] leading-[1.4] text-[var(--muted-2)] [text-wrap:pretty]">
                {keychainCopy}
              </p>
            </div>
            {keychainAction ? (
              <button
                type="button"
                className={cx(
                  BUTTON,
                  BUTTON_SECONDARY,
                  "min-h-[36px] whitespace-nowrap px-3 text-[12px]",
                )}
                onClick={onRunKeychainFlow}
                disabled={permissionBusy === "keychain"}
              >
                {permissionBusy === "keychain" ? "Waiting…" : keychainAction}
              </button>
            ) : (
              <PermissionBadge state={keychainState} />
            )}
          </div>
        </section>
      ) : null}

      <section className={PLAIN_SECTION}>
        {/* Numbered sections only where there are two (macOS: Keychain, then microphone); on Windows the step title says it. */}
        {mac ? (
          <div className="flex items-center justify-center gap-2">
            <span className="grid h-7 w-7 place-items-center rounded-full bg-[var(--accent)] text-[13px] font-bold text-[var(--accent-contrast)]">
              2
            </span>
            <h3 className="m-0 text-[18px] font-bold text-[var(--paper)]">Microphone access</h3>
          </div>
        ) : null}
        <div className="mx-auto w-full max-w-[520px] text-left">
        <div className={SIMPLE_ROW}>
          <div className="min-w-0">
            <div className="min-w-0">
              <strong className="text-[15px]">Microphone</strong>
              <p className="m-0 mt-1 text-[13px] leading-[1.4] text-[var(--muted-2)] [text-wrap:pretty]">
                Pick the microphone you dictate with.
              </p>
            </div>
            <div className="mt-4 grid gap-2">
              <div className="max-w-[360px]">
                <MicDevicePicker
                  value={inputDevice}
                  onChange={onMicDeviceChange}
                  disabled={permissionBusy === "mic"}
                  refreshToken={micRefreshToken}
                  onResolvedLabel={setMicDeviceLabel}
                />
              </div>
              <MicMeter active={micState === "ready"} />
              <p className="m-0 text-[12px] leading-[1.35] text-[var(--muted-2)]">
                {micState === "restricted"
                  ? "Microphone access is managed by policy — see the message above."
                  : micState === "denied"
                    ? "Microphone access is blocked — see the message above to fix it."
                    : micState !== "ready"
                      ? "Approve microphone access first."
                      : micSignalSeen
                    ? "We can hear you."
                    : micCheckState === "no_signal"
                      ? `${micDeviceLabel} is producing no audio. Choose another input or retry.`
                      : micCheckState === "unavailable"
                        ? `${micDeviceLabel} is unavailable. Choose another input or retry.`
                        : "Speak to test your microphone."}
              </p>
              {micState === "ready" &&
              (micCheckState === "no_signal" || micCheckState === "unavailable") ? (
                <button
                  type="button"
                  className={cx(BUTTON, BUTTON_SECONDARY, "w-fit min-h-[34px] px-3 text-[12px]")}
                  onClick={onRetryMicCheck}
                >
                  Retry microphone
                </button>
              ) : null}
            </div>
          </div>
          {micAction ? (
            <button
              type="button"
              className={cx(
                BUTTON,
                BUTTON_SECONDARY,
                "min-h-[36px] whitespace-nowrap px-3 text-[12px]",
              )}
              onClick={onRunMicFlow}
              disabled={permissionBusy === "mic"}
            >
              {permissionBusy === "mic" ? "Waiting…" : micAction}
            </button>
          ) : (
            <PermissionBadge state={micSignalSeen ? "ready" : micState} />
          )}
        </div>
        </div>
      </section>

      {permissionStatus && permissionStatus !== "Microphone is ready." && !(micSignalSeen && permissionStatus === "Audio detected.") ? (
        <p className="m-0 text-[12px] leading-[1.4] text-[var(--muted-2)]">{permissionStatus}</p>
      ) : null}
    </section>
  );
}
