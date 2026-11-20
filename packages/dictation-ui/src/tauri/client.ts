// Host-injected Tauri client for the `DictationClient` (host.ts) and its `onboarding` group. The package imports no
// Tauri module: the host passes its own `invoke` and `listen`, the command name for each client member, and the
// event name for each subscribable event. A member or event with no mapping rejects (or throws, for `on`) with an
// Error naming it; optional members are included only when the host maps at least one of their commands.
import type { LibrarySource } from "@rightkit/library-ui";
import type { SettingsBackend } from "@rightkit/settings-ui/core";
import type {
  AiKeyCheck,
  AiKeyProvider,
  AiKeyStatus,
  AiKeysUpdate,
  DictationClient,
  DictationCommand,
  DictationEventMap,
  FileTranscript,
  HotkeyConflict,
  MicDevice,
  Subscribe,
  Unsubscribe,
} from "../host.js";
import type { HistoryPageResult } from "../history/historyQueryTypes.js";
import type { AppState, HistoryItem } from "../lib/homeMetrics.js";
import type {
  OnboardingClient,
  OnboardingEventMap,
  RecognitionSetupResult,
  RecognitionSetupStart,
} from "../onboarding/client.js";
import type { LocalAsrModelStatus, LocalBackend, SpeechClient, WhisperModelStatus } from "../settings/client.js";
import type { VocabularyEntry, VocabularySuggestion } from "../types/vocabulary.js";

/** The host's IPC invoke (for example `@tauri-apps/api/core`'s `invoke`). Args are sent as given. */
export type TauriInvoke = <T = unknown>(command: string, args?: Record<string, unknown>) => Promise<T>;

/** The host's IPC listen. The handler receives an object whose `payload` is the event payload. */
export type TauriListen = <T>(event: string, handler: (event: { payload: T }) => void) => Promise<() => void>;

/** Every client member the host can map to a command. The dictation commands use the `DictationCommand` names. */
export type TauriDictationCommand =
  | "appState"
  | "startDictation"
  | "stopDictation"
  | "cancelDictation"
  | "frontendReady"
  | "recentHistory"
  | "queryHistory"
  | "updateHistoryItem"
  | "copyText"
  | "listVocabulary"
  | "listVocabularySuggestions"
  | "addVocabulary"
  | "importVocabulary"
  | "removeVocabulary"
  | "acceptVocabularySuggestion"
  | "dismissVocabularySuggestion"
  | "aiKeyStatus"
  | "validateAiKey"
  | "setAiKeys"
  | "micDevices"
  | "hotkeyConflict"
  | "registerHotkeys"
  | "accessibilityGranted"
  | "activateShortcuts"
  | "openAccessibilitySettings"
  | "transcribeFile"
  | "saveTextFile"
  | "openFilesSettings"
  | "localAsrModelStatus"
  | "downloadLocalAsrModel"
  | "whisperModelStatus"
  | "downloadWhisperModel"
  | "prewarmEngine"
  | "dismissModelUpdate"
  | "microphoneAccessStatus"
  | "requestMicrophoneAccess"
  | "openMicrophoneSettings"
  | "startMicMeter"
  | "stopMicMeter"
  | "setShortcutsActive"
  | "startRecognitionSetup"
  | "stopRecognitionSetup"
  | "cancelRecognitionSetup";

/** Every event the dictation and onboarding subscriptions can use. */
export type TauriClientEvent = keyof DictationEventMap | keyof OnboardingEventMap;

const ONBOARDING_COMMANDS: readonly TauriDictationCommand[] = [
  "microphoneAccessStatus",
  "requestMicrophoneAccess",
  "openMicrophoneSettings",
  "startMicMeter",
  "stopMicMeter",
  "setShortcutsActive",
  "startRecognitionSetup",
  "stopRecognitionSetup",
  "cancelRecognitionSetup",
];

const SPEECH_COMMANDS: readonly TauriDictationCommand[] = [
  "localAsrModelStatus",
  "downloadLocalAsrModel",
  "whisperModelStatus",
  "downloadWhisperModel",
  "prewarmEngine",
  "dismissModelUpdate",
];

export interface TauriDictationClientOptions {
  invoke: TauriInvoke;
  listen: TauriListen;
  /** Command name per client member. A member with no entry rejects with an Error naming it. */
  commands: Partial<Record<TauriDictationCommand, string>>;
  /** Event name per subscribable event. An event with no entry throws from `on` with an Error naming it. */
  events: Partial<Record<TauriClientEvent, string>>;
  /** The settings store (`@rightkit/settings-ui` port) the host implements over its native settings IPC. */
  settings: SettingsBackend;
  /** Delivery history as a library source. Omit when the host has none; the `history` member then rejects. */
  history?: LibrarySource;
  /** Pro entitlement for `entitlement()`; `null` while unknown. */
  entitlement: () => boolean | null;
  /** Host open dialog for media files (`pickMediaFile`); resolves a path, or null when cancelled. Omit to reject. */
  pickMediaFile?: () => Promise<string | null>;
  /** Host save dialog for `saveTextFile`; resolves the chosen path, or null when cancelled. Omit to reject. */
  chooseSavePath?: (suggestedName: string) => Promise<string | null>;
}

/** Builds the `DictationClient` (with its `onboarding` and `speech` groups when mapped) over host IPC. */
export function createTauriDictationClient(options: TauriDictationClientOptions): DictationClient {
  const { invoke, listen, commands, events } = options;

  function unmapped(member: string): Error {
    return new Error(`@rightkit/dictation-ui: the Tauri client has no command mapped for \`${member}\``);
  }

  // Async so that a missing mapping or a synchronous IPC throw becomes a rejection.
  async function call<T>(member: TauriDictationCommand, args?: Record<string, unknown>): Promise<T> {
    const name = commands[member];
    if (name === undefined) throw unmapped(member);
    return invoke<T>(name, args);
  }

  const mapped = (members: readonly TauriDictationCommand[]): boolean =>
    members.some((member) => commands[member] !== undefined);

  function subscriber<M>(): Subscribe<M> {
    return async <K extends keyof M>(event: K, handler: (payload: M[K]) => void): Promise<Unsubscribe> => {
      const name = events[String(event) as TauriClientEvent];
      if (name === undefined) {
        throw new Error(`@rightkit/dictation-ui: the Tauri client has no event mapped for \`${String(event)}\``);
      }
      return listen<M[K]>(name, (message) => handler(message.payload));
    };
  }

  const history: LibrarySource = options.history ?? {
    query: () =>
      Promise.reject(new Error("@rightkit/dictation-ui: the Tauri client has no LibrarySource for `history`")),
    copyText: () =>
      Promise.reject(new Error("@rightkit/dictation-ui: the Tauri client has no LibrarySource for `history`")),
  };

  const client: DictationClient = {
    settings: options.settings,
    on: subscriber<DictationEventMap>(),

    appState: () => call<AppState>("appState"),
    run: (command: DictationCommand) => call<void>(command),

    history,
    recentHistory: () => call<HistoryItem[]>("recentHistory"),
    updateHistoryItem: (deliveryId, transcript) => call<boolean>("updateHistoryItem", { deliveryId, transcript }),
    copyText: (text) => call<void>("copyText", { text }),

    listVocabulary: () => call<VocabularyEntry[]>("listVocabulary"),
    listVocabularySuggestions: () => call<VocabularySuggestion[]>("listVocabularySuggestions"),
    addVocabulary: (term, soundsLike) => call<VocabularyEntry>("addVocabulary", { term, sounds_like: soundsLike }),
    importVocabulary: (terms) => call<VocabularyEntry[]>("importVocabulary", { terms }),
    removeVocabulary: (id) => call<boolean>("removeVocabulary", { id }),
    acceptVocabularySuggestion: (term) => call<VocabularyEntry | null>("acceptVocabularySuggestion", { term }),
    dismissVocabularySuggestion: async (term) => {
      await call<void>("dismissVocabularySuggestion", { term });
    },

    aiKeyStatus: () => call<AiKeyStatus>("aiKeyStatus"),
    validateAiKey: (provider: AiKeyProvider, key: string) => call<AiKeyCheck>("validateAiKey", { provider, key }),
    setAiKeys: (update: AiKeysUpdate) => call<AiKeyStatus>("setAiKeys", { update }),

    micDevices: () => call<MicDevice[]>("micDevices"),

    hotkeyConflict: () => call<HotkeyConflict | null>("hotkeyConflict"),
    registerHotkeys: () => call<void>("registerHotkeys"),

    entitlement: () => options.entitlement(),

    pickMediaFile: async () => {
      if (!options.pickMediaFile) throw new Error("@rightkit/dictation-ui: the Tauri client has no `pickMediaFile` dialog");
      return options.pickMediaFile();
    },
    transcribeFile: (path) => call<FileTranscript>("transcribeFile", { path }),
    saveTextFile: async (suggestedName, contents) => {
      if (!options.chooseSavePath) {
        throw new Error("@rightkit/dictation-ui: the Tauri client has no `chooseSavePath` dialog for `saveTextFile`");
      }
      const path = await options.chooseSavePath(suggestedName);
      if (!path) return false;
      await call<void>("saveTextFile", { path, contents });
      return true;
    },
  };

  if (commands.frontendReady !== undefined) {
    client.frontendReady = (surface: string) => {
      void call<void>("frontendReady", { surface }).catch(() => undefined);
    };
  }
  // Optional: set only when the host maps `queryHistory`. Args are wrapped as the host command's `query` parameter.
  if (commands.queryHistory !== undefined) {
    client.queryHistory = (args) => call<HistoryPageResult>("queryHistory", { query: args });
  }
  if (commands.accessibilityGranted !== undefined) {
    client.accessibilityGranted = (prompt: boolean) => call<boolean>("accessibilityGranted", { prompt });
  }
  if (commands.activateShortcuts !== undefined) {
    client.activateShortcuts = () => call<void>("activateShortcuts");
  }
  if (commands.openAccessibilitySettings !== undefined) {
    client.openAccessibilitySettings = () => call<void>("openAccessibilitySettings");
  }
  if (commands.openFilesSettings !== undefined) {
    client.openFilesSettings = (removable: boolean) => call<void>("openFilesSettings", { removable });
  }

  if (mapped(SPEECH_COMMANDS)) {
    const speech: SpeechClient = {
      localAsrModelStatus: (backend: LocalBackend) => call<LocalAsrModelStatus>("localAsrModelStatus", { backend }),
      downloadLocalAsrModel: (backend: LocalBackend) =>
        call<LocalAsrModelStatus>("downloadLocalAsrModel", { backend }),
      whisperModelStatus: () => call<WhisperModelStatus>("whisperModelStatus"),
      downloadWhisperModel: () => call<void>("downloadWhisperModel"),
      prewarmEngine: () => call<void>("prewarmEngine"),
      dismissModelUpdate: () => call<void>("dismissModelUpdate"),
    };
    client.speech = speech;
  }

  if (mapped(ONBOARDING_COMMANDS)) {
    const onboarding: OnboardingClient = {
      on: subscriber<OnboardingEventMap>(),
      microphoneAccessStatus: () => call<string>("microphoneAccessStatus"),
      requestMicrophoneAccess: () => call<string>("requestMicrophoneAccess"),
      openMicrophoneSettings: () => call<void>("openMicrophoneSettings"),
      startMicMeter: (deviceSetting: string | undefined) => call<void>("startMicMeter", { deviceSetting }),
      stopMicMeter: () => call<void>("stopMicMeter"),
      setShortcutsActive: (active: boolean, previewOnly?: boolean) =>
        call<void>("setShortcutsActive", previewOnly === undefined ? { active } : { active, previewOnly }),
      startRecognitionSetup: () => call<RecognitionSetupStart>("startRecognitionSetup"),
      stopRecognitionSetup: (sessionId: string) =>
        call<RecognitionSetupResult>("stopRecognitionSetup", { sessionId }),
      cancelRecognitionSetup: (sessionId: string) => call<void>("cancelRecognitionSetup", { sessionId }),
    };
    client.onboarding = onboarding;
  }

  return client;
}
