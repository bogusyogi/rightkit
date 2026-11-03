import { useId, useState, type ReactNode } from "react";
import type { VoiceCallController, VoiceCallPhase } from "./voiceCall.js";

export interface VoiceCallDevice {
  id: string;
  label: string;
}

export interface VoiceCallBarLabels {
  title: string;
  mute: string;
  unmute: string;
  hangUp: string;
  inputDevice: string;
  settings: string;
  showTranscript: string;
  hideTranscript: string;
  transcript: string;
  emptyTranscript: string;
  you: string;
  agent: string;
  phase: Record<VoiceCallPhase, string>;
}

export const DEFAULT_VOICE_CALL_BAR_LABELS: VoiceCallBarLabels = {
  title: "Voice call",
  mute: "Mute",
  unmute: "Unmute",
  hangUp: "Hang up",
  inputDevice: "Input device",
  settings: "Settings",
  showTranscript: "Show transcript",
  hideTranscript: "Hide transcript",
  transcript: "Live transcript",
  emptyTranscript: "No speech yet.",
  you: "You",
  agent: "Agent",
  phase: {
    idle: "Ready",
    connecting: "Connecting",
    listening: "Listening",
    "user-speaking": "You are speaking",
    "agent-thinking": "Thinking",
    "agent-speaking": "Speaking",
    muted: "Muted",
    error: "Something went wrong",
    ended: "Call ended",
  },
};

export interface VoiceCallBarProps {
  /** The state and controls from `useVoiceCall`. */
  call: VoiceCallController;
  /** Input devices to offer. The select appears only when at least one is given. */
  inputDevices?: readonly VoiceCallDevice[];
  /** Optional settings slot (voice, speed, language, input), shown in a disclosure under the controls. */
  settings?: ReactNode;
  labels?: Partial<VoiceCallBarLabels>;
  /** Start with the transcript folded out. Default false. */
  defaultTranscriptOpen?: boolean;
  className?: string;
}

const cx = (...parts: (string | false | null | undefined)[]): string => parts.filter(Boolean).join(" ");

/**
 * Themed voice-call bar: mute, hang up, input device, an optional settings slot and a live transcript that folds
 * out under the bar. Focus order follows the DOM: mute, hang up, input, settings, transcript. The transcript is an
 * aria-live region, so screen readers announce new text. Colours come from the `--rk-media-*` tokens; the
 * stylesheet honours reduced motion and text scaling.
 */
export function VoiceCallBar(props: VoiceCallBarProps) {
  const { call, inputDevices, settings, className } = props;
  const labels: VoiceCallBarLabels = {
    ...DEFAULT_VOICE_CALL_BAR_LABELS,
    ...props.labels,
    phase: { ...DEFAULT_VOICE_CALL_BAR_LABELS.phase, ...props.labels?.phase },
  };
  const [transcriptOpen, setTranscriptOpen] = useState(props.defaultTranscriptOpen ?? false);
  const transcriptId = useId();
  const { state, phase } = call;
  const ended = state.activity === "ended";

  return (
    <section className={cx("rk-vc", className)} data-phase={phase} aria-label={labels.title}>
      <div className="rk-vc-bar">
        <span className="rk-vc-indicator" aria-hidden="true" />
        <span className="rk-vc-status" role="status">
          {labels.phase[phase]}
        </span>
        <div className="rk-vc-controls">
          <button
            type="button"
            className="rk-vc-button"
            aria-pressed={state.muted}
            disabled={ended}
            onClick={call.toggleMute}
          >
            {state.muted ? labels.unmute : labels.mute}
          </button>
          <button type="button" className="rk-vc-button rk-vc-hangup" disabled={ended} onClick={call.hangUp}>
            {labels.hangUp}
          </button>
          {inputDevices && inputDevices.length > 0 ? (
            <label className="rk-vc-device">
              <span className="rk-vc-visually-hidden">{labels.inputDevice}</span>
              <select
                value={state.inputDeviceId ?? inputDevices[0]!.id}
                disabled={ended}
                onChange={(event) => call.selectInput(event.currentTarget.value)}
              >
                {inputDevices.map((device) => (
                  <option key={device.id} value={device.id}>
                    {device.label}
                  </option>
                ))}
              </select>
            </label>
          ) : null}
          <button
            type="button"
            className="rk-vc-button"
            aria-expanded={transcriptOpen}
            aria-controls={transcriptId}
            onClick={() => setTranscriptOpen((open) => !open)}
          >
            {transcriptOpen ? labels.hideTranscript : labels.showTranscript}
          </button>
        </div>
      </div>
      {state.error ? (
        <p className="rk-vc-error" role="alert">
          {state.error}
        </p>
      ) : null}
      {settings ? (
        <details className="rk-vc-settings">
          <summary>{labels.settings}</summary>
          <div className="rk-vc-settings-body">{settings}</div>
        </details>
      ) : null}
      <div id={transcriptId} className="rk-vc-transcript" hidden={!transcriptOpen}>
        <h3 className="rk-vc-transcript-title">{labels.transcript}</h3>
        {state.lines.length === 0 ? (
          <p className="rk-vc-empty">{labels.emptyTranscript}</p>
        ) : (
          <ol className="rk-vc-lines" aria-live="polite">
            {state.lines.map((line) => (
              <li key={line.id} className={cx("rk-vc-line", !line.final && "rk-vc-line-partial")} data-speaker={line.speaker}>
                <span className="rk-vc-speaker">{line.speaker === "user" ? labels.you : labels.agent}</span>
                <span className="rk-vc-text">{line.text}</span>
              </li>
            ))}
          </ol>
        )}
      </div>
    </section>
  );
}
