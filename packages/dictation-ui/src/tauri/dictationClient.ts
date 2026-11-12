// The Tauri `DictationClient`: the hub window's pages over the app's native commands and events.
import { invoke } from "@tauri-apps/api/core";
import { pickFile, pickSavePath } from "@rightkit/tauri/dialog";
import type { LibraryOrigin, LibrarySource } from "@rightkit/library-ui";
import type { SettingsBackend, SettingsSnapshot, SettingsValues } from "@rightkit/settings-ui/core";
import { deliveryQueryArgs, deliveryRow } from "../history/deliveryRows.js";
import type { DictationClient, DictationCommand, DictationEventMap, Unsubscribe } from "../host.js";
import type { HistoryItem } from "../lib/homeMetrics.js";
import { eventSubscribe, notMapped, tauriListenFn, type ListenFn, type TauriClientOptions } from "./events.js";

export interface TauriDictationOptions extends TauriClientOptions {
  /** Product name in the history recovery notice. Default `"The app"`. */
  appName?: string;
  /** Start of the exported file names (`<prefix>-history-<date>.json`). Default none. */
  fileNamePrefix?: string;
  /**
   * Pro entitlement, which comes from the app's licence lifecycle, not from a command of this client.
   * Without it `entitlement()` and the `entitlement` event throw "not mapped".
   */
  entitlement?: {
    current(): boolean | null;
    subscribe(handler: (pro: boolean | null) => void): Promise<Unsubscribe>;
  };
  /**
   * Maps the native settings values to the keys the pages use (`load`, `patch` results) and a patch back to
   * native keys (before `apply_settings_patch_at`). Default: identity. HeardRight uses it for the
   * `appearance.theme` choice.
   */
  settingsKeys?: {
    fromNative(values: SettingsValues): SettingsValues;
    toNative(patch: SettingsValues): SettingsValues;
  };
  /** Name of the window event that fans settings changes out inside one window. Default `"rk-dictation-settings-updated"`. */
  settingsEvent?: string;
}

const COMMANDS: Record<DictationCommand, string> = {
  startDictation: "start_dictation",
  stopDictation: "stop_dictation",
  cancelDictation: "cancel_dictation",
};

const EVENTS: { [K in Exclude<keyof DictationEventMap, "entitlement">]: string } = {
  appState: "app-state-changed",
  deliveryRecorded: "delivery-recorded",
  wake: "wake-fired",
  hotkeyStatus: "hotkey-registration-status",
  nativeDragDrop: "native-drag-drop",
};

const RETENTION_OPTIONS = [
  { value: "forever", label: "Forever" },
  { value: "7_days", label: "7 days" },
  { value: "30_days", label: "30 days" },
  { value: "90_days", label: "90 days" },
  { value: "1_year", label: "1 year" },
];

const HISTORY_EXPORT_FILTERS = [
  { name: "JSON (portable)", extensions: ["json"] },
  { name: "Markdown", extensions: ["md"] },
  { name: "Plain text", extensions: ["txt"] },
  { name: "CSV", extensions: ["csv"] },
];

interface NativeSnapshot {
  values: SettingsValues;
  /** A 64-bit revision as a decimal string. */
  revision: string;
}

interface HistoryPageResult {
  items: HistoryItem[];
  total: number;
  has_more: boolean;
}

/**
 * The native revision is a 64-bit value sent as a decimal string: a JS number rounds it above 2^53 and the
 * rounded value would never match the store again. The settings port types the revision as a number and
 * only echoes it, so it is handed a small opaque handle; the exact string stays here.
 */
function settingsBackend(options: TauriDictationOptions): SettingsBackend {
  const maxHandles = 64;
  const eventName = options.settingsEvent ?? "rk-dictation-settings-updated";
  const keys = options.settingsKeys ?? { fromNative: (v: SettingsValues) => v, toNative: (p: SettingsValues) => p };
  let nextHandle = 1;
  const revisionByHandle = new Map<number, string>();
  const handleByRevision = new Map<string, number>();

  function revisionHandle(revision: string): number {
    const known = handleByRevision.get(revision);
    if (known !== undefined) return known;
    const handle = nextHandle++;
    handleByRevision.set(revision, handle);
    revisionByHandle.set(handle, revision);
    if (revisionByHandle.size > maxHandles) {
      const oldest = revisionByHandle.keys().next().value as number;
      const old = revisionByHandle.get(oldest);
      revisionByHandle.delete(oldest);
      if (old !== undefined) handleByRevision.delete(old);
    }
    return handle;
  }

  const snapshot = ({ values, revision }: NativeSnapshot): SettingsSnapshot => ({
    values: keys.fromNative(values),
    revision: revisionHandle(revision),
  });

  const conflictError = (error: unknown): Error => {
    const message = error instanceof Error ? error.message : String(error);
    if (!message.startsWith("revision_conflict")) return error instanceof Error ? error : new Error(message);
    return Object.assign(new Error(message), { code: "revision_conflict" });
  };

  return {
    async load() {
      return snapshot(await invoke<NativeSnapshot>("get_settings_snapshot"));
    },
    async patch(patch, expectedRevision) {
      let native: NativeSnapshot;
      try {
        native = await invoke<NativeSnapshot>("apply_settings_patch_at", {
          patch: keys.toNative({ ...patch }),
          // An unknown handle sends text the native side refuses as stale (null would drop the guard).
          expectedRevision: expectedRevision === undefined ? null : (revisionByHandle.get(expectedRevision) ?? "stale"),
        });
      } catch (error) {
        throw conflictError(error);
      }
      window.dispatchEvent(new CustomEvent(eventName, { detail: native.values }));
      return snapshot(native);
    },
    async subscribe(handler) {
      const onUpdate = ((event: Event) => {
        const detail = (event as CustomEvent<SettingsValues | undefined>).detail;
        if (detail !== undefined) handler(Object.keys(detail));
      }) as EventListener;
      window.addEventListener(eventName, onUpdate);
      return () => window.removeEventListener(eventName, onUpdate);
    },
  };
}

function dateStamp(): string {
  return new Date().toISOString().slice(0, 10);
}

function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1) : "";
}

/** Shows a save dialog for `suggestedName` (filtered to its extension) and writes `contents` through `write_text_file`. */
async function saveTextFile(suggestedName: string, contents: string): Promise<boolean> {
  const ext = extensionOf(suggestedName);
  const path = await pickSavePath({
    defaultPath: suggestedName,
    ...(ext ? { filters: [{ name: ext.toUpperCase(), extensions: [ext] }] } : {}),
  });
  if (!path) return false;
  await invoke("write_text_file", { path, contents });
  return true;
}

function historySource(options: TauriDictationOptions, listen: ListenFn): LibrarySource {
  const prefix = options.fileNamePrefix ? `${options.fileNamePrefix}-` : "";
  const appName = options.appName ?? "The app";
  return {
    async query(query) {
      const page = await invoke<HistoryPageResult>("query_history", { query: deliveryQueryArgs(query) });
      return { rows: page.items.map(deliveryRow), total: page.total, hasMore: page.has_more };
    },
    // `delivery-recorded` for local deliveries; `cloud-sync-updated` for rows pulled from another device.
    async subscribe(onChange) {
      const stops = await Promise.all([
        listen("delivery-recorded", () => onChange()),
        listen("cloud-sync-updated", () => onChange()),
      ]);
      return () => stops.forEach((stop) => stop());
    },
    copyText: (text) => invoke<void>("copy_text_to_clipboard", { text }),
    remove: (id) => invoke<void>("delete_history_item", { deliveryId: id }),
    update: (id, text) => invoke<boolean>("update_history_item", { deliveryId: id, transcript: text }),
    async exportAll() {
      const path = await pickSavePath({
        defaultPath: `${prefix}history-${dateStamp()}.json`,
        filters: HISTORY_EXPORT_FILTERS,
      });
      if (!path) return null;
      return invoke<number>("export_history", { path });
    },
    saveText: saveTextFile,
    retention: {
      options: RETENTION_OPTIONS,
      get: () => invoke<string>("get_history_retention"),
      set: (value) => invoke<number>("set_history_retention", { retention: value }),
    },
    recovery: {
      async status() {
        const status = await invoke<{ created_at_ms: number; backups: unknown[] } | null>("get_history_recovery_status");
        if (!status || status.backups.length === 0) return null;
        return {
          createdAtMs: status.created_at_ms,
          title: "Earlier history was kept as an encrypted backup.",
          message: `${appName} could not unlock the previous history database, so it started a fresh one. The old file was not deleted.`,
        };
      },
      async exportDiagnostics() {
        const path = await pickSavePath({
          defaultPath: `${prefix}history-recovery.txt`,
          filters: [{ name: "Plain text", extensions: ["txt"] }],
        });
        if (!path) return false;
        await invoke("export_history_recovery_diagnostics", { path });
        return true;
      },
    },
    // A row's `iconKey` is its exe path, else its process name; `filterKey` is the raw process name.
    async originIcon(origin: LibraryOrigin) {
      const processName = origin.filterKey?.trim() ?? "";
      const key = origin.iconKey?.trim() ?? "";
      const url = await invoke<string | null>("app_icon_png", {
        processName: processName || key,
        exePath: key && key !== processName ? key : null,
      }).catch(() => null);
      return typeof url === "string" && url.startsWith("data:image/") ? url : null;
    },
  };
}

/** The Tauri `DictationClient`. Building it touches no Tauri API; the first call does. */
export function tauriDictationClient(options: TauriDictationOptions = {}): DictationClient {
  const listen = options.listen ?? tauriListenFn;
  const base = eventSubscribe<Omit<DictationEventMap, "entitlement">>(EVENTS, listen);
  return {
    settings: settingsBackend(options),
    on: (event, handler) => {
      if (event === "entitlement") {
        if (!options.entitlement) return Promise.reject(notMapped("the entitlement event"));
        return options.entitlement.subscribe(handler as (pro: boolean | null) => void);
      }
      return base(event, handler as never);
    },

    appState: () => invoke("get_app_state"),
    run: (command) => invoke<void>(COMMANDS[command]),
    frontendReady: (surface) => void invoke("hub_frontend_ready", { surface }).catch(() => undefined),

    history: historySource(options, listen),
    recentHistory: () => invoke<HistoryItem[]>("get_recent_history"),
    updateHistoryItem: (deliveryId, transcript) => invoke<boolean>("update_history_item", { deliveryId, transcript }),
    copyText: (text) => invoke<void>("copy_text_to_clipboard", { text }),

    listVocabulary: () => invoke("list_vocabulary"),
    listVocabularySuggestions: () => invoke("list_vocab_suggestions"),
    // `sounds_like` keeps the native field's name.
    addVocabulary: (term, soundsLike) => invoke("add_vocabulary", { term, sounds_like: soundsLike }),
    importVocabulary: (terms) => invoke("import_vocabulary", { terms }),
    removeVocabulary: (id) => invoke<boolean>("remove_vocabulary", { id }),
    acceptVocabularySuggestion: (term) => invoke("accept_vocab_suggestion", { term }),
    dismissVocabularySuggestion: (term) => invoke<void>("dismiss_vocab_suggestion", { term }),

    aiKeyStatus: () => invoke("get_l3_key_status"),
    validateAiKey: (provider, key) => invoke("validate_l3_key", { provider, key }),
    setAiKeys: (update) => invoke("set_l3_api_keys", { update }),

    micDevices: () => invoke("list_mic_devices"),

    hotkeyConflict: () => invoke("get_hotkey_conflict"),
    registerHotkeys: () => invoke<void>("register_hotkeys_now"),

    accessibilityGranted: (prompt) => invoke<boolean>("macos_accessibility_status", { prompt }),
    activateShortcuts: () => invoke<void>("activate_macos_shortcuts"),
    openAccessibilitySettings: () => invoke<void>("open_macos_accessibility_settings"),

    entitlement: () => {
      if (!options.entitlement) throw notMapped("entitlement()");
      return options.entitlement.current();
    },
    // The pricing page URL is fixed in the app's native code.
    openPricing: () => invoke<void>("open_pricing_page"),

    pickMediaFile: () => pickFile(),
    transcribeFile: (path) => invoke("transcribe_file", { path }),
    saveTextFile,
    openFilesSettings: (removable) => invoke<void>("open_macos_files_settings", { removable }),
  };
}
