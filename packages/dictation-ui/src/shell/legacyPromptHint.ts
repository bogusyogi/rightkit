import { dictationStorageKey } from "../host.js";

// The "Say prompt at the end" tip used to keep its state in the pill webview's localStorage. The
// native Windows pill keeps it in prompt-hint.json instead, so the hub (same app origin and
// WebView2 profile as the retired pill window) hands the old values over once and clears them.


type LegacyStorage = Pick<Storage, "getItem" | "removeItem">;
type LegacyInvoke = (command: string, args: Record<string, unknown>) => Promise<unknown>;

/** Runs the one-time hand-over. Keys stay in place if the hand-over fails, so the next start retries. */
export async function migrateLegacyPromptHint(storage: LegacyStorage, invoke: LegacyInvoke): Promise<void> {
  const COUNT_KEY = dictationStorageKey("promptHintCount");
  const SEEN_KEY = dictationStorageKey("promptHintSeen");
  const DISABLED_KEY = dictationStorageKey("promptHintDisabled");
  const rawCount = storage.getItem(COUNT_KEY);
  const rawSeen = storage.getItem(SEEN_KEY);
  const rawDisabled = storage.getItem(DISABLED_KEY);
  if (rawCount === null && rawSeen === null && rawDisabled === null) return;
  const count = Number(rawCount ?? "0");
  await invoke("import_legacy_prompt_hint", {
    count: Number.isInteger(count) && count > 0 ? count : 0,
    seen: rawSeen === "1",
    disabled: rawDisabled === "1",
  });
  for (const key of [COUNT_KEY, SEEN_KEY, DISABLED_KEY]) storage.removeItem(key);
}
