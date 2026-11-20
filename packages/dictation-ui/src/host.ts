// Host-supplied dictation & pill capabilities. No UI composition or native transport.
import type { LibrarySource } from "@rightkit/library-ui";
import type { SettingsBackend } from "@rightkit/settings-ui/core";
import type { HistoryPageResult, HistoryQueryArgs } from "./history/historyQueryTypes.js";
import type { UseSettings } from "./hooks/useSettings.js";
import type { SpeechClient } from "./settings/client.js";
import type { AppState, HistoryItem } from "./lib/homeMetrics.js";
import type { OnboardingClient } from "./onboarding/client.js";
import type { Platform } from "@rightkit/platform-ui";
import type { PillEvent } from "./pill/pillBars.js";
import type { VocabularyEntry, VocabularySuggestion } from "./types/vocabulary.js";

export type Unsubscribe = () => void;

/** Typed event subscription: resolves to an unsubscribe once the host has registered the listener. */
export type Subscribe<M> = <K extends keyof M>(event: K, handler: (payload: M[K]) => void) => Promise<Unsubscribe>;

// ---------------------------------------------------------------------------------------------
// Dictation

export interface NativeDragDropPosition {
  x: number;
  y: number;
}

/** File drags over the hub window, pointer position in physical pixels. */
export type NativeDragDropPayload =
  | { type: "enter"; paths: string[]; position: NativeDragDropPosition }
  | { type: "over"; position: NativeDragDropPosition }
  | { type: "drop"; paths: string[]; position: NativeDragDropPosition }
  | { type: "leave" };

export interface HotkeyConflict {
  shortcuts: string[];
  message: string;
}

export interface MicDevice {
  id: number;
  name: string;
  default: boolean;
  channels: number;
  platform_id?: string | null;
}

export type AiKeyProvider = "groq" | "openrouter";

export interface AiKeyStatus {
  groq_saved: boolean;
  openrouter_saved: boolean;
  nvidia_saved: boolean;
}

/** Every field optional; `clear_*` removes a stored key. */
export interface AiKeysUpdate {
  groq_key?: string | null;
  openrouter_key?: string | null;
  nvidia_key?: string | null;
  clear_groq?: boolean;
  clear_openrouter?: boolean;
  clear_nvidia?: boolean;
}

export type AiKeyCheck = { kind: "Valid" | "Rejected" | "Unverified" };

export interface FileTranscript {
  text: string;
  srt: string;
  vtt: string;
}

/** Recording controls available to hosts. */
export type DictationCommand = "startDictation" | "stopDictation" | "cancelDictation";

/** Events dictation consumers listen to. The host maps each to its native event. */
export interface DictationEventMap {
  /** The dictation state machine changed. */
  appState: AppState;
  /** A transcript was delivered (the recent list refetches). */
  deliveryRecorded: unknown;
  /** History changed outside a delivery, such as after syncing records. */
  historyChanged: unknown;
  /** The wake phrase fired. */
  wake: unknown;
  /** Hotkey registration result; `null` when every hotkey registered. */
  hotkeyStatus: HotkeyConflict | null;
  /** File drags over the window. */
  nativeDragDrop: NativeDragDropPayload;
  /** Pro entitlement changed; `null` while it is still being checked. */
  entitlement: boolean | null;
  /** Local speech model install progress (Parakeet lanes); `phase` is `downloading`, `extracting`, `verifying`, `installing`, `preparing` or `installed`. */
  localAsrModel: { backend: string; phase: string; received: number; total: number; message?: string };
  /** Whisper model install progress; the same phases. */
  whisperModel: { phase: string; received: number; total: number };
}

/** Dictation capabilities supplied by each host. One method per operation; no command names. */
export interface DictationClient {
  /** Settings store (the `@rightkit/settings-ui` port); values are the `SettingsBlob` keys. */
  settings: SettingsBackend;
  on: Subscribe<DictationEventMap>;

  appState(): Promise<AppState>;
  run(command: DictationCommand): Promise<void>;
  /** The host surface finished its first paint (`surface` names it). */
  frontendReady?(surface: string): void;

  /** Delivery history as a library source (rows mapped with `deliveryRow`); the History page and row icons use it. */
  history: LibrarySource;
  recentHistory(): Promise<HistoryItem[]>;
  queryHistory?(args: HistoryQueryArgs & { offset: number; limit: number }): Promise<HistoryPageResult>;
  updateHistoryItem(deliveryId: string, transcript: string): Promise<boolean>;
  copyText(text: string): Promise<void>;

  listVocabulary(): Promise<VocabularyEntry[]>;
  listVocabularySuggestions(): Promise<VocabularySuggestion[]>;
  addVocabulary(term: string, soundsLike?: string[]): Promise<VocabularyEntry>;
  importVocabulary(terms: string[]): Promise<VocabularyEntry[]>;
  removeVocabulary(id: number): Promise<boolean>;
  acceptVocabularySuggestion(term: string): Promise<VocabularyEntry | null>;
  dismissVocabularySuggestion(term: string): Promise<void>;

  aiKeyStatus(): Promise<AiKeyStatus>;
  validateAiKey(provider: AiKeyProvider, key: string): Promise<AiKeyCheck>;
  setAiKeys(update: AiKeysUpdate): Promise<AiKeyStatus>;

  micDevices(): Promise<MicDevice[]>;

  hotkeyConflict(): Promise<HotkeyConflict | null>;
  registerHotkeys(): Promise<void>;

  /** macOS only: accessibility permission (`prompt` asks the system to show its prompt). */
  accessibilityGranted?(prompt: boolean): Promise<boolean>;
  activateShortcuts?(): Promise<void>;
  openAccessibilitySettings?(): Promise<void>;

  /** Pro entitlement; `null` while unknown. */
  entitlement(): boolean | null;

  /** Shows an open dialog for an audio or video file; resolves the path, or null when cancelled. */
  pickMediaFile(): Promise<string | null>;
  transcribeFile(path: string): Promise<FileTranscript>;
  /** Shows a save dialog and writes `contents`; resolves false when cancelled. */
  saveTextFile(suggestedName: string, contents: string): Promise<boolean>;
  /** macOS only: open the Files and Folders privacy pane. */
  openFilesSettings?(removable: boolean): Promise<void>;

  /** Optional local speech-model capabilities. */
  speech?: SpeechClient;
  /** First-run setup (`@rightkit/dictation-ui/onboarding`). */
  onboarding?: OnboardingClient;
}

// ---------------------------------------------------------------------------------------------
// Pill

export type DeliveryTarget = { process_name?: string | null; window_title?: string | null };

export interface PillHistoryItem {
  delivery_id: string;
  transcript: string;
  raw_transcript?: string | null;
  target?: DeliveryTarget | null;
}

/** Button actions on the pill and its popups. */
export type PillAction =
  | "startDictation"
  | "cancelDictation"
  | "stopDictation"
  | "stopAndSend"
  | "openHubHome"
  | "openHubHistory"
  | "openClipboard";

export interface PillHitbox {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** Events the pill windows listen to. */
export interface PillEventMap {
  /** Recording state for the pill. */
  event: PillEvent;
  /** The native pointer entered (true) or left (false) the pill. */
  hover: boolean;
  /** Native pointer position over the pill window, in CSS pixels. */
  cursor: [number, number];
  /** First reveal after launch: play the intro once. */
  intro: unknown;
  deliveryRecorded: { delivery_id?: string; target?: DeliveryTarget | null };
  /** Local model install phase; anything but `installed` shows the preparing state. */
  modelPhase: { phase: string };
  /** The hint popup should read its payload. */
  hintShow: unknown;
  /** The clipboard popup should reload its items. */
  clipboardShow: unknown;
}

export interface PillClient {
  on: Subscribe<PillEventMap>;
  action(name: PillAction): Promise<void>;
  /** Structured renderer log line (`event`, `message`). Must not throw. */
  log(event: string, message: string): Promise<void>;

  resize(width: number, height: number): Promise<void>;
  setHitbox(hitbox: PillHitbox): Promise<void>;
  takeIntro(): Promise<boolean>;
  revealPill(): Promise<void>;
  /** Generation of the current terminal error, or `null`. */
  terminalErrorGeneration(): Promise<number | null>;
  recoverTerminalError(generation: number | null): Promise<void>;

  recentHistory(): Promise<PillHistoryItem[]>;
  showPromptHint(): Promise<void>;
  copyText(text: string): Promise<void>;
  updateHistoryItem(deliveryId: string, transcript: string): Promise<boolean>;

  clipboardItems(): Promise<PillHistoryItem[]>;
  dismissClipboard(): Promise<void>;
  resizeClipboard(width: number, height: number): Promise<void>;
  clipboardReady(): Promise<void>;

  takeHintPayload(): Promise<{ kind?: string } | null>;
  dismissHint(): Promise<void>;
  resizeHint(width: number, height: number): Promise<void>;
  hintReady(): Promise<void>;
  /** Store the screenshot destination chosen in the hint. */
  setScreenshotDestination(destination: "clipboard" | "disk"): Promise<void>;
}

// ---------------------------------------------------------------------------------------------
// Configuration

export interface DictationHooks {
  useSettings?: () => UseSettings;
}

export type DictationStorageKey = "promptHintCount" | "promptHintSeen" | "promptHintDisabled";

/** Configure capability clients independently in each host window. */
export interface DictationHostConfig {
  client?: DictationClient;
  pill?: PillClient;
  appName?: string;
  storagePrefix?: string;
  storageKeys?: Partial<Record<DictationStorageKey, string>>;
  chordLabels?: Partial<Record<Platform, Record<string, string>>>;
  hooks?: DictationHooks;
}

let config: DictationHostConfig = {};

export function configureDictationHost(next: DictationHostConfig): void {
  config = { ...config, ...next };
}

export function hostAppName(): string {
  return config.appName ?? "App";
}

export function dictationStorageKey(key: DictationStorageKey): string {
  return config.storageKeys?.[key] ?? `${config.storagePrefix ?? "rk-dictation"}.${key}`;
}

export function dictationClient(): DictationClient {
  if (!config.client) throw new Error("@rightkit/dictation-ui: configureDictationHost({ client }) was not called");
  return config.client;
}

export function pillClient(): PillClient {
  if (!config.pill) throw new Error("@rightkit/dictation-ui: configureDictationHost({ pill }) was not called");
  return config.pill;
}

export function hostChordLabels(): NonNullable<DictationHostConfig["chordLabels"]> {
  return config.chordLabels ?? {};
}

export function hostHooks(): DictationHooks {
  return config.hooks ?? {};
}
