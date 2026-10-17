import { useState } from "react";
import { Kbd, usePlatform } from "@rightkit/platform-ui/react";
import type { Platform } from "@rightkit/platform-ui";
import { chordFromEvent } from "../shortcuts.js";
import { useShellOptional } from "./provider.js";
import { cx } from "./ui.js";

export interface ShortcutRecorderProps {
  /** The bound chord in the registry vocabulary (`mod+shift+p`), or null or undefined when unbound. */
  value: string | null | undefined;
  /** Receives the new chord, or null when cleared (Backspace or Delete with no modifier while recording). */
  onChange: (chord: string | null) => void;
  /** Accessible name, for example "Shortcut for Save". */
  label: string;
  /** Overrides the shell's platform for the chord glyphs. */
  platform?: Platform;
  /** Text shown when unbound. Default "Not set". */
  emptyLabel?: string;
  disabled?: boolean;
  className?: string;
}

/**
 * Button that records a key chord. Activate it, press the chord, and it reports the chord and stops. Escape cancels,
 * Backspace or Delete clears the binding, and a press of modifiers alone keeps waiting. While recording, the keys are
 * taken by the button (no shell chord fires). A dialog that hosts the recorder should pass `escape={false}` so its
 * window-level Escape handling does not close the dialog before the recorder sees Escape.
 */
export function ShortcutRecorder({ value, onChange, label, platform, emptyLabel = "Not set", disabled, className }: ShortcutRecorderProps) {
  const [recording, setRecording] = useState(false);
  const shell = useShellOptional();
  const detected = usePlatform();
  const effective = platform ?? shell?.platform ?? detected;
  const finish = () => setRecording(false);
  return (
    <button
      type="button"
      className={cx("rk-recorder", recording && "is-recording", className)}
      aria-label={label}
      aria-pressed={recording}
      disabled={disabled}
      title={recording ? "Press the new shortcut. Escape cancels, Backspace clears." : undefined}
      onClick={() => setRecording((on) => !on)}
      onBlur={finish}
      onKeyDown={(event) => {
        if (!recording) return;
        event.preventDefault();
        event.stopPropagation();
        if (event.nativeEvent.isComposing) return;
        if (event.key === "Escape") {
          finish();
          return;
        }
        const bare = !event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey;
        if (bare && (event.key === "Backspace" || event.key === "Delete")) {
          onChange(null);
          finish();
          return;
        }
        const chord = chordFromEvent(event.nativeEvent, effective);
        if (chord) {
          onChange(chord);
          finish();
        }
      }}
    >
      {recording ? (
        <span className="rk-recorder__hint">Press keys…</span>
      ) : value ? (
        <Kbd chord={value} platform={effective} gap={1} />
      ) : (
        <span className="rk-recorder__empty">{emptyLabel}</span>
      )}
    </button>
  );
}
