// The Tauri `DictationClient`: the hub window's pages over the app's native commands and events.
import { invoke } from "@tauri-apps/api/core";
import { ask, pickFile, pickSavePath } from "@rightkit/tauri/dialog";
import type { LibraryOrigin, LibrarySource } from "@rightkit/library-ui";
import { deliveryQueryArgs, deliveryRow } from "../history/deliveryRows.js";
import type { DictationClient, DictationCommand, DictationEventMap, Unsubscribe } from "../host.js";
import type { HistoryItem } from "../lib/homeMetrics.js";
import type {
  AccountClient,
  LicenseSettingsClient,
  AppSettingsClient,
  CloudSyncStatus,
  DeveloperClient,
  LocalAsrModelStatus,
  ProBridgeStatus,
  SpeechClient,
  SyncClient,
  SyncProviderChoice,
  WakeLabStatus,
  WhisperModelStatus,
} from "../settings/client.js";
import { eventSubscribe, notMapped, tauriListenFn, type ListenFn, type TauriClientOptions } from "./events.js";
import { tauriDictationSettingsBackend, type TauriSettingsOptions } from "./settingsBackend.js";

export interface TauriDictationOptions extends TauriClientOptions, Omit<TauriSettingsOptions, "invoke"> {
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
   * The account session and its devices (`DictationClient.account`), which an app answers from its licence
   * layer: the rejections carry the wording to show. Without it the group is absent and the Devices section
   * fails with "no `account` group".
   */
  account?: AccountClient;
  /** The licence as the License section follows it (`DictationClient.license`); see `LicenseSettingsClient`. */
  license?: LicenseSettingsClient;
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
  localAsrModel: "local-asr-model",
  whisperModel: "whisper-model",
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

interface HistoryPageResult {
  items: HistoryItem[];
  total: number;
  has_more: boolean;
}

function dateStamp(): string {
  return new Date().toISOString().slice(0, 10);
}

function extensionOf(name: string): string {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1) : "";
}

/** The save dialog's filter name for an extension; others show the extension in capitals. */
const FILTER_NAMES: Record<string, string> = { txt: "Plain text" };

/** Shows a save dialog for `suggestedName` (filtered to its extension) and writes `contents` through `write_text_file`. */
async function saveTextFile(suggestedName: string, contents: string): Promise<boolean> {
  const ext = extensionOf(suggestedName);
  const path = await pickSavePath({
    defaultPath: suggestedName,
    ...(ext ? { filters: [{ name: FILTER_NAMES[ext.toLowerCase()] ?? ext.toUpperCase(), extensions: [ext] }] } : {}),
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
      const key = origin.iconKey?.trim() ?? "";
      // A row that names its raw process (`processName`, possibly empty) is looked up by exactly that
      // and its exe path; other rows fall back to the filter key.
      const raw = origin.processName?.trim();
      const processName = raw ?? (origin.filterKey?.trim() || key);
      const url = await invoke<string | null>("app_icon_png", {
        processName,
        exePath: key && key !== processName ? key : null,
      }).catch(() => null);
      return typeof url === "string" && url.startsWith("data:image/") ? url : null;
    },
  };
}

const appSettings: AppSettingsClient = {
  launchAtLoginEnabled: () => invoke<boolean>("launch_at_login_enabled"),
  setLaunchAtLogin: (enabled) => invoke<void>("set_launch_at_login", { enabled }),
  rotateHistoryKey: () => invoke<void>("rotate_history_key"),
  confirm: (message, options) => ask(message, options),
};

const speech: SpeechClient = {
  localAsrModelStatus: (backend) => invoke<LocalAsrModelStatus>("local_asr_model_status_for", { backend }),
  downloadLocalAsrModel: (backend) => invoke<LocalAsrModelStatus>("download_local_asr_model", { backend }),
  whisperModelStatus: () => invoke<WhisperModelStatus>("whisper_model_status"),
  downloadWhisperModel: () => invoke<void>("download_whisper_model"),
  prewarmEngine: () => invoke<void>("prewarm_engine"),
  dismissModelUpdate: () => invoke<void>("dismiss_model_update"),
};

const developer: DeveloperClient = {
  wakeLabStatus: () => invoke<WakeLabStatus>("get_wake_lab_status"),
  setWakeLabEnabled: (enabled) => invoke<void>("set_wake_lab_enabled", { enabled }),
  diagnosticCaptureAudioPresent: () => invoke<boolean>("diagnostic_capture_audio_present"),
  openDiagnosticCapturesFolder: () => invoke<string>("open_diagnostic_captures_folder"),
  recompileCoreMlCache: () => invoke<void>("recompile_coreml_cache"),
  sendDiagnostics: () => invoke<string>("send_diagnostics"),
  exportDiagnostics: (includeNativeDumps) => invoke<string>("export_diagnostics", { includeNativeDumps }),
};

// The provider choice reads and writes the store's `sync_provider` through `get_settings` and
// `apply_settings_patch`, the commands HeardRight's own settings use for it.
const sync: SyncClient = {
  async syncProvider() {
    const settings = await invoke<{ sync_provider?: SyncProviderChoice | null }>("get_settings");
    return settings.sync_provider;
  },
  setSyncProvider: (provider) => invoke<void>("apply_settings_patch", { patch: { sync_provider: provider } }),
  cloudSyncStatus: () => invoke<CloudSyncStatus>("cloud_sync_status"),
  setCloudSyncEnabled: (enabled) => invoke<void>("cloud_sync_set_enabled", { enabled }),
  setKeychainSyncEnabled: (enabled) => invoke<void>("pro_bridge_set_keychain_sync_enabled", { enabled }),
  setProResolutionEnabled: (enabled) => invoke<void>("pro_bridge_set_pro_resolution_enabled", { enabled }),
  proBridgeStatus: () => invoke<ProBridgeStatus>("pro_bridge_status"),
};

/** The Tauri `DictationClient`. Building it touches no Tauri API; the first call does. */
export function tauriDictationClient(options: TauriDictationOptions = {}): DictationClient {
  const listen = options.listen ?? tauriListenFn;
  const base = eventSubscribe<Omit<DictationEventMap, "entitlement">>(EVENTS, listen);
  return {
    settings: tauriDictationSettingsBackend(options),
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

    app: appSettings,
    speech,
    developer,
    sync,
    account: options.account,
    license: options.license,
  };
}
