import { useEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Button, Dialog } from "@rightkit/react-shell/react";
import {
  getShortcutBindingConflicts,
  type CustomizableShortcutAction,
  type ShortcutActionId,
  type ShortcutBindingConflict,
} from "@rightkit/agent-ui-core/lib/shortcuts/schema";
import {
  formatShortcutForDisplay,
  isRiskyBrowserShortcut,
  keyToShortcutToken,
  normalizeCombo,
  resolveShortcutEventKey,
  type ShortcutCombo,
} from "@rightkit/agent-ui-core/lib/shortcuts/bindings";
import { rightKitShortcuts } from "@rightkit/agent-ui-core/lib/shortcuts/dispatcher";

const MODIFIER_KEYS = new Set(["shift", "control", "alt", "meta"]);
const MAX_SHORTCUT_KEY_COUNT = 3;
const SECOND_CHORD_TIMEOUT_MS = 3000;

type RecordingKeyboardEvent = {
  altKey: boolean;
  code: string;
  ctrlKey: boolean;
  isComposing: boolean;
  key: string;
  metaKey: boolean;
  repeat: boolean;
  shiftKey: boolean;
};

export type ShortcutRecordingState = {
  chords: ShortcutCombo[];
  livePreview: ShortcutCombo | null;
  settled: boolean;
};

interface ShortcutRecordingDialogProps {
  action: CustomizableShortcutAction | null;
  overrides: Record<string, string>;
  onSave: (actionId: ShortcutActionId, combo: ShortcutCombo, replaceActionId?: ShortcutActionId) => void;
  onOpenChange: (open: boolean) => void;
}

function getPhysicalKeyCount(
  event: Pick<RecordingKeyboardEvent, "altKey" | "ctrlKey" | "key" | "metaKey" | "shiftKey">,
  includeEventKey = false,
): number {
  const keys = new Set<string>();
  if (event.altKey) keys.add("alt");
  if (event.ctrlKey) keys.add("control");
  if (event.metaKey) keys.add("meta");
  if (event.shiftKey) keys.add("shift");
  if (includeEventKey) keys.add(event.key.toLowerCase());
  return keys.size;
}

function isCustomizableConflict(
  conflict: ShortcutBindingConflict,
): conflict is ShortcutBindingConflict & { action: CustomizableShortcutAction } {
  return conflict.action.customizable;
}

function getModifierPreview(event: RecordingKeyboardEvent): ShortcutCombo | null {
  if (getPhysicalKeyCount(event) > MAX_SHORTCUT_KEY_COUNT) return null;
  const parts: string[] = [];
  if (event.metaKey || event.ctrlKey) parts.push("mod");
  if (event.shiftKey) parts.push("shift");
  if (event.altKey) parts.push("alt");
  return parts.length > 0 ? normalizeCombo(parts.join("+")) : null;
}

function keyboardEventToCombo(event: RecordingKeyboardEvent): ShortcutCombo | null {
  if (MODIFIER_KEYS.has(event.key.toLowerCase())) return null;
  if (getPhysicalKeyCount(event, true) > MAX_SHORTCUT_KEY_COUNT) return null;
  const key = keyToShortcutToken(resolveShortcutEventKey(event));
  if (!key) return null;
  const parts: string[] = [];
  if (event.metaKey || event.ctrlKey) parts.push("mod");
  if (event.shiftKey) parts.push("shift");
  if (event.altKey) parts.push("alt");
  parts.push(key);
  return normalizeCombo(parts.join("+"));
}

function modifierKeyUpToCombo(event: ReactKeyboardEvent<HTMLDivElement>): ShortcutCombo | null {
  const key = event.key.toLowerCase();
  if (!MODIFIER_KEYS.has(key)) return null;
  if (getPhysicalKeyCount(event, true) > MAX_SHORTCUT_KEY_COUNT) return null;
  const parts: string[] = [];
  if (event.metaKey || event.ctrlKey || key === "meta" || key === "control") parts.push("mod");
  if (event.shiftKey || key === "shift") parts.push("shift");
  if (event.altKey || key === "alt") parts.push("alt");
  return parts.length > 0 ? normalizeCombo(parts.join("+")) : null;
}

export function settleShortcutRecordingState(state: ShortcutRecordingState): ShortcutRecordingState {
  return state.chords.length > 0 ? { ...state, livePreview: null, settled: true } : state;
}

export function updateShortcutRecordingState(
  state: ShortcutRecordingState,
  event: RecordingKeyboardEvent,
  phase: "keydown" | "keyup",
): ShortcutRecordingState {
  if (event.repeat || event.isComposing) return state;
  if (phase === "keyup") return { ...state, livePreview: getModifierPreview(event) };
  if (event.key === "Backspace") return { chords: state.chords.slice(0, -1), livePreview: null, settled: false };
  const chord = keyboardEventToCombo(event);
  if (chord) {
    if (state.settled) return { chords: [chord], livePreview: null, settled: false };
    const chords = state.chords.length < 2 ? [...state.chords, chord] : state.chords;
    return { chords, livePreview: null, settled: chords.length === 2 };
  }
  return { ...state, livePreview: getModifierPreview(event) };
}

export default function ShortcutRecordingDialog({ action, overrides, onSave, onOpenChange }: ShortcutRecordingDialogProps) {
  const [recording, setRecording] = useState<ShortcutRecordingState>({ chords: [], livePreview: null, settled: false });
  const recordingRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!action) return;
    setRecording({ chords: [], livePreview: null, settled: false });
    recordingRef.current?.focus();
    rightKitShortcuts.suspend();
    return () => rightKitShortcuts.resume();
  }, [action]);

  const waitingForSecondChord = recording.chords.length === 1 && !recording.settled;
  useEffect(() => {
    if (!waitingForSecondChord) return;
    const timeout = window.setTimeout(() => setRecording(settleShortcutRecordingState), SECOND_CHORD_TIMEOUT_MS);
    return () => window.clearTimeout(timeout);
  }, [waitingForSecondChord]);

  const combo = normalizeCombo(recording.chords.join(" "));
  const conflicts = action && combo ? getShortcutBindingConflicts(action.id, combo, overrides) : [];
  const protectedConflict = conflicts.find((conflict) => !conflict.action.customizable && conflict.kind !== "contextual-prefix");
  const customizableConflicts = conflicts.filter(isCustomizableConflict);
  const prefixConflict = customizableConflicts.find((conflict) => conflict.kind === "prefix");
  const exactConflict = customizableConflicts.find((conflict) => conflict.kind === "exact");
  const contextualPrefixConflict = conflicts.find((conflict) => conflict.kind === "contextual-prefix");

  const close = () => onOpenChange(false);
  const confirm = () => {
    if (!recording.settled) setRecording(settleShortcutRecordingState);
    if (!action || !combo || protectedConflict || prefixConflict) return;
    onSave(action.id, combo, exactConflict?.action.id);
    close();
  };
  const handleRecordingEvent = (event: ReactKeyboardEvent<HTMLDivElement>, phase: "keydown" | "keyup") => {
    event.preventDefault();
    event.stopPropagation();
    const isPrefixStyleAction = Boolean(action && "prefixStyle" in action && action.prefixStyle);
    if (phase === "keyup" && isPrefixStyleAction && recording.chords.length === 0) {
      const modifierCombo = modifierKeyUpToCombo(event);
      if (modifierCombo) {
        setRecording({ chords: [modifierCombo], livePreview: null, settled: true });
        return;
      }
    }
    const nextRecording = updateShortcutRecordingState(recording, {
      altKey: event.altKey,
      code: event.nativeEvent.code,
      ctrlKey: event.ctrlKey,
      isComposing: event.nativeEvent.isComposing,
      key: event.key,
      metaKey: event.metaKey,
      repeat: event.repeat,
      shiftKey: event.shiftKey,
    }, phase);
    setRecording(isPrefixStyleAction && nextRecording.chords.length > 1 ? recording : nextRecording);
  };

  // The recorder owns every key including Escape, so the dialog opts out of Escape dismissal.
  if (!action) return null;
  return (
    <Dialog
      title={`Edit shortcut: ${action.id.split("_").join(" ")}`}
      description="Press one key or a two-key sequence. The recording stays local until you confirm."
      onClose={close}
      escape={false}
      autoFocus={false}
      width={400}
    >
      <div
        ref={recordingRef}
        role="application"
        // biome-ignore lint/a11y/noNoninteractiveTabindex: recorder is intentional keyboard capture target.
        tabIndex={0}
        className="mt-4 flex min-h-28 items-center justify-center rounded-lg border border-line bg-panel-2 px-4 py-5 text-center outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
        onKeyDown={(event) => handleRecordingEvent(event, "keydown")}
        onKeyUp={(event) => handleRecordingEvent(event, "keyup")}
        onBlur={() => setRecording((current) => ({ ...current, livePreview: null }))}
        aria-label="Shortcut recorder"
      >
        <div className="flex flex-wrap items-center justify-center gap-2">
          {recording.chords.map((chord, index) => (
            <kbd
              // biome-ignore lint/suspicious/noArrayIndexKey: repeated chords have no stable identity.
              key={`${chord}-${index}`}
              className="rounded-md border border-line bg-bg px-3 py-2 text-[12px] font-mono text-text"
            >
              {formatShortcutForDisplay(chord)}
            </kbd>
          ))}
          {recording.livePreview ? (
            <kbd className="rounded-md border border-dashed border-line bg-bg px-3 py-2 text-[12px] font-mono text-text-3">
              {formatShortcutForDisplay(recording.livePreview)}
            </kbd>
          ) : null}
          {recording.chords.length === 0 && !recording.livePreview ? <span className="text-[12px] text-text-3">Waiting for a key…</span> : null}
        </div>
      </div>

      {recording.settled && protectedConflict ? <p className="mt-3 text-[12px] text-danger">This shortcut is reserved by {protectedConflict.action.id}.</p> : null}
      {recording.settled && prefixConflict ? <p className="mt-3 text-[12px] text-danger">This shortcut conflicts with {prefixConflict.action.id}.</p> : null}
      {recording.settled && exactConflict && !protectedConflict && !prefixConflict ? <p className="mt-3 text-[12px] text-warning">This replaces the shortcut for {exactConflict.action.id}.</p> : null}
      {recording.settled && contextualPrefixConflict && !protectedConflict && !prefixConflict ? <p className="mt-3 text-[12px] text-warning">This may overlap {contextualPrefixConflict.action.id}.</p> : null}
      {recording.settled && combo && isRiskyBrowserShortcut(combo) ? <p className="mt-3 text-[12px] text-warning">This may be intercepted by the browser or operating system.</p> : null}

      <div className="mt-4 flex justify-end gap-2">
        <Button type="button" variant="secondary" size="sm" onClick={close}>Cancel</Button>
        <Button
          type="button"
          variant="primary"
          size="sm"
          disabled={!combo || Boolean(protectedConflict) || Boolean(prefixConflict)}
          onClick={confirm}
        >
          Confirm
        </Button>
      </div>
    </Dialog>
  );
}
