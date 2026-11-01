import { useEffect, useMemo, useState } from "react";
import { createKeymapStore } from "@rightkit/react-shell";
import {
  getCustomizableShortcutActions,
  getEffectiveShortcutCombo,
  getEffectiveShortcutPrefix,
  type CustomizableShortcutAction,
  type ShortcutActionId,
  type ShortcutCategory,
} from "@rightkit/agent-ui-core/lib/shortcuts/schema";
import {
  formatShortcutForDisplay,
  UNASSIGNED_SHORTCUT,
  type ShortcutCombo,
} from "@rightkit/agent-ui-core/lib/shortcuts/bindings";
import ShortcutRecordingDialog from "./ShortcutRecordingDialog";

export const SHORTCUT_OVERRIDES_STORAGE_KEY = "coderight.shortcuts.v1";

/** Fired on `window` after this window writes overrides, so the live registry re-syncs (other windows get `storage`). */
export const SHORTCUT_OVERRIDES_CHANGED_EVENT = "coderight:shortcut-overrides-changed";

export type ShortcutOverrides = Record<string, ShortcutCombo>;

const keymapStore = createKeymapStore({ storageKey: SHORTCUT_OVERRIDES_STORAGE_KEY });

export function readShortcutOverrides(fallback: ShortcutOverrides = {}): ShortcutOverrides {
  const loaded = Object.fromEntries(
    Object.entries(keymapStore.load()).filter((entry): entry is [string, string] => typeof entry[1] === "string"),
  );
  return Object.keys(loaded).length > 0 ? loaded : { ...fallback };
}

export function writeShortcutOverrides(overrides: ShortcutOverrides): void {
  keymapStore.save(overrides);
  if (typeof window !== "undefined") window.dispatchEvent(new Event(SHORTCUT_OVERRIDES_CHANGED_EVENT));
}

export interface KeyboardShortcutsSettingsProps {
  initialOverrides?: ShortcutOverrides;
  onOverridesChange?: (overrides: ShortcutOverrides) => void;
}

const CATEGORIES: ShortcutCategory[] = ["session", "models", "panels", "navigation", "application"];

function labelForAction(action: CustomizableShortcutAction): string {
  return action.id.split("_").map((part) => part[0].toUpperCase() + part.slice(1)).join(" ");
}

function categoryLabel(category: ShortcutCategory): string {
  return category[0].toUpperCase() + category.slice(1);
}

export default function KeyboardShortcutsSettings({
  initialOverrides = {},
  onOverridesChange,
}: KeyboardShortcutsSettingsProps) {
  const [shortcutOverrides, setShortcutOverrides] = useState<ShortcutOverrides>(() =>
    readShortcutOverrides(initialOverrides),
  );
  const [editingAction, setEditingAction] = useState<CustomizableShortcutAction | null>(null);

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key !== SHORTCUT_OVERRIDES_STORAGE_KEY) return;
      setShortcutOverrides(readShortcutOverrides(initialOverrides));
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [initialOverrides]);

  const actions = useMemo(() => getCustomizableShortcutActions(), []);
  const updateOverrides = (next: ShortcutOverrides) => {
    setShortcutOverrides(next);
    writeShortcutOverrides(next);
    onOverridesChange?.(next);
  };
  const save = (actionId: ShortcutActionId, combo: ShortcutCombo, replaceActionId?: ShortcutActionId) => {
    const next = { ...shortcutOverrides, [actionId]: combo };
    if (replaceActionId) next[replaceActionId] = UNASSIGNED_SHORTCUT;
    updateOverrides(next);
  };
  const resetOne = (actionId: ShortcutActionId) => {
    const next = { ...shortcutOverrides };
    delete next[actionId];
    updateOverrides(next);
  };
  const displayFor = (action: CustomizableShortcutAction) => {
    const prefixStyle = "prefixStyle" in action && action.prefixStyle;
    const combo = prefixStyle
      ? getEffectiveShortcutPrefix(action.id, shortcutOverrides)
      : getEffectiveShortcutCombo(action.id, shortcutOverrides);
    const formatted = formatShortcutForDisplay(combo);
    if (!prefixStyle || !combo || combo === UNASSIGNED_SHORTCUT) return formatted;
    return `${formatted} + digit`;
  };

  return (
    <section className="flex max-w-[720px] flex-col gap-5 p-6" aria-label="Keyboard shortcuts settings">
      <div>
        <h3 className="text-[16px] font-bold text-text">Keyboard shortcuts</h3>
        <p className="mt-1 text-[13px] text-text-2">Remap application commands and review conflicts before saving.</p>
      </div>
      {CATEGORIES.map((category, index) => {
        const categoryActions = actions.filter((action) => action.category === category);
        if (categoryActions.length === 0) return null;
        return (
          <section key={category} className={`grid gap-2 ${index === 0 ? "" : "border-t border-line pt-5"}`}>
            <div className="flex items-center justify-between gap-3">
              <h4 className="text-[13px] font-semibold text-text">{categoryLabel(category)}</h4>
              {index === 0 ? (
                <button
                  type="button"
                  className="inline-flex h-8 items-center rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 cursor-pointer hover:bg-panel-2 hover:text-text"
                  onClick={() => updateOverrides({})}
                >
                  Reset all
                </button>
              ) : null}
            </div>
            <div className="grid gap-2">
              {categoryActions.map((action) => (
                <div key={action.id} className="flex min-h-9 items-center justify-between gap-3 rounded-lg border border-line bg-panel px-3 py-2">
                  <span className="min-w-0 text-[12px] text-text-2">{labelForAction(action)}</span>
                  <div className="flex shrink-0 items-center gap-2">
                    <kbd className="min-w-24 rounded-md border border-line bg-bg px-2 py-1 text-center text-[11px] font-mono text-text">
                      {displayFor(action)}
                    </kbd>
                    <button
                      type="button"
                      className="inline-flex h-8 items-center rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 cursor-pointer hover:bg-panel-2 hover:text-text"
                      onClick={() => setEditingAction(action)}
                    >
                      Edit
                    </button>
                    {action.id in shortcutOverrides ? (
                      <button
                        type="button"
                        className="inline-flex h-8 items-center rounded-lg border border-transparent bg-transparent px-2 text-[12px] text-text-3 cursor-pointer hover:text-text"
                        onClick={() => resetOne(action.id)}
                      >
                        Reset
                      </button>
                    ) : null}
                  </div>
                </div>
              ))}
            </div>
          </section>
        );
      })}
      <ShortcutRecordingDialog
        action={editingAction}
        overrides={shortcutOverrides}
        onSave={save}
        onOpenChange={(open) => {
          if (!open) setEditingAction(null);
        }}
      />
    </section>
  );
}
