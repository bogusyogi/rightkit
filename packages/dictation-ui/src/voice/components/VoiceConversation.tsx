import { useState } from "react";
import { ProgressBar } from "@rightkit/react-shell/react";
import Icon from "./Icon.js";
import {
  useVoiceConversation,
  type PendingVoiceResponse,
  type VoiceConversationOptions,
} from "../hooks/useVoiceConversation.js";

export interface VoiceConversationProps
  extends Omit<VoiceConversationOptions, "enabled"> {
  /** Initial state; subsequent changes are controlled by the conversation button. */
  initialEnabled?: boolean;
  disabled?: boolean;
  onEnabledChange?: (enabled: boolean) => void;
  className?: string;
}

function statusLabel(status: ReturnType<typeof useVoiceConversation>["status"]): string {
  switch (status) {
    case "listening":
      return "Listening";
    case "transcribing":
      return "Transcribing";
    case "thinking":
      return "Thinking";
    case "speaking":
      return "Speaking";
    default:
      return "Ready for voice";
  }
}

function actionLabel(status: ReturnType<typeof useVoiceConversation>["status"]): string {
  switch (status) {
    case "listening":
      return "Finish voice turn";
    case "transcribing":
      return "Transcribing voice turn";
    case "thinking":
    case "speaking":
      return "Stop voice turn";
    default:
      return "Start voice turn";
  }
}

/**
 * Compact composer control for Hermes-style dictation and hands-free voice.
 * The component intentionally does not know daemon routes: transcription and
 * submission are supplied by the orchestrator through the hook boundary.
 */
export default function VoiceConversation({
  initialEnabled = false,
  disabled = false,
  onEnabledChange,
  className,
  ...options
}: VoiceConversationProps) {
  const [enabled, setEnabled] = useState(initialEnabled);
  const conversation = useVoiceConversation({ ...options, enabled });
  const active = enabled;
  const currentStatus = statusLabel(conversation.status);
  const actionText = actionLabel(conversation.status);
  const canAct = !disabled && conversation.status !== "transcribing";

  const setConversationEnabled = (next: boolean) => {
    setEnabled(next);
    onEnabledChange?.(next);
    if (!next) void conversation.end();
  };

  const handlePrimaryAction = () => {
    if (!active) {
      setConversationEnabled(true);
      return;
    }
    if (conversation.status === "thinking" || conversation.status === "speaking") {
      void conversation.end();
      setConversationEnabled(false);
      return;
    }
    if (conversation.status === "idle" || conversation.status === "listening") {
      if (conversation.status === "idle") void conversation.start();
      else conversation.stopTurn();
    }
  };

  return (
    <div
      className={`flex min-w-0 items-center gap-1.5 rounded-lg border border-line bg-panel px-1.5 py-1 ${className ?? ""}`}
      data-testid="voice-conversation"
      data-status={conversation.status}
    >
      <button
        type="button"
        className={`grid h-7 w-7 shrink-0 place-items-center rounded-md ${active ? "bg-accent text-on-accent" : "text-text-3 hover:bg-panel-2 hover:text-text"}`}
        aria-label={active ? "End voice conversation" : "Start voice conversation"}
        aria-pressed={active}
        disabled={disabled}
        onClick={() => setConversationEnabled(!active)}
      >
        <Icon name="activity" size={14} />
      </button>
      <button
        type="button"
        className="min-w-0 flex-1 truncate rounded-md px-1.5 py-1 text-left text-[11px] text-text-2 hover:bg-panel-2 disabled:cursor-not-allowed disabled:opacity-50"
        aria-label={actionText}
        disabled={!canAct || !active}
        onClick={handlePrimaryAction}
      >
        <span className="block truncate">{active ? currentStatus : "Voice"}</span>
        {conversation.error ? (
          <span className="block truncate text-danger" role="alert">
            {conversation.error}
          </span>
        ) : null}
      </button>
      {active ? (
        <ProgressBar
          className="h-1.5 w-12"
          label="Microphone level"
          value={Math.round(conversation.level * 100)}
          max={100}
        />
      ) : null}
      {active ? (
        <button
          type="button"
          className="grid h-7 w-7 shrink-0 place-items-center rounded-md text-text-3 hover:bg-panel-2 hover:text-text"
          aria-label={conversation.muted ? "Unmute microphone" : "Mute microphone"}
          aria-pressed={conversation.muted}
          disabled={disabled}
          onClick={conversation.toggleMute}
        >
          <Icon name={conversation.muted ? "close" : "play"} size={12} />
        </button>
      ) : null}
    </div>
  );
}

export type { PendingVoiceResponse };
