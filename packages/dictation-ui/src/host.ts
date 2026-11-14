// Host configuration for the dictation pill and pages.
//
// The package holds no IPC: every command and event goes through the two clients the host supplies
// (`DictationClient` for the pages, `PillClient` for the pill windows). Storage keys and the product
// name also come from here, so no app's literals are baked in. Call `configureDictationHost` once per
// window, before rendering.

import type { ComponentType, ReactNode } from "react";
import type { LibrarySource } from "@rightkit/library-ui";
// The `./host` entry holds only the list's host configuration, so the pill entry does not pull in the list.
import { configureLibraryHost, type LibraryHostConfig } from "@rightkit/library-ui/host";
import type { SettingsBackend } from "@rightkit/settings-ui/core";
import type { UseSettings } from "./hooks/useSettings.js";
import type {
  AccountClient,
  AppSettingsClient,
  LocalBackend,
  DeveloperClient,
  LicenseSettingsClient,
  SpeechClient,
  SyncClient,
} from "./settings/client.js";
import type { AppState, HistoryItem } from "./lib/homeMetrics.js";
import type { OnboardingClient } from "./onboarding/client.js";
import type { Platform } from "@rightkit/platform-ui";
import type { PillEvent } from "./pill/pillBars.js";
import type { VocabularyEntry, VocabularySuggestion } from "./types/vocabulary.js";

export type Unsubscribe = () => void;

/** Typed event subscription: resolves to an unsubscribe once the host has registered the listener. */
export type Subscribe<M> = <K extends keyof M>(event: K, handler: (payload: M[K]) => void) => Promise<Unsubscribe>;

// ---------------------------------------------------------------------------------------------
// Pages

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

/** Recording controls the Home page offers. */
export type DictationCommand = "startDictation" | "stopDictation" | "cancelDictation";

/** Events the pages listen to. The host maps each to its native event. */
export interface DictationEventMap {
  /** The dictation state machine changed. */
  appState: AppState;
  /** A transcript was delivered (the recent list refetches). */
  deliveryRecorded: unknown;
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

/** Everything the dictation pages need from the host. One method per operation; no command names. */
export interface DictationClient {
  /** Settings store (the `@rightkit/settings-ui` port); values are the `SettingsBlob` keys. */
  settings: SettingsBackend;
  on: Subscribe<DictationEventMap>;

  appState(): Promise<AppState>;
  run(command: DictationCommand): Promise<void>;
  /** The page finished its first paint (`surface` names it). */
  frontendReady?(surface: string): void;

  /** Delivery history as a library source (rows mapped with `deliveryRow`); the History page and row icons use it. */
  history: LibrarySource;
  recentHistory(): Promise<HistoryItem[]>;
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
  openPricing(): Promise<void>;

  /** Shows an open dialog for an audio or video file; resolves the path, or null when cancelled. */
  pickMediaFile(): Promise<string | null>;
  transcribeFile(path: string): Promise<FileTranscript>;
  /** Shows a save dialog and writes `contents`; resolves false when cancelled. */
  saveTextFile(suggestedName: string, contents: string): Promise<boolean>;
  /** macOS only: open the Files and Folders privacy pane. */
  openFilesSettings?(removable: boolean): Promise<void>;

  // Settings sections (`@rightkit/dictation-ui/settings`). Each group is needed only by the sections that use it.
  app?: AppSettingsClient;
  speech?: SpeechClient;
  developer?: DeveloperClient;
  sync?: SyncClient;
  account?: AccountClient;
  license?: LicenseSettingsClient;
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

/** The pages that render inside the host's page wrapper. */
export type DictationPageName = "dictation" | "polish" | "home" | "history";

export interface DictationPageShellProps {
  page: DictationPageName;
  /** Only the `history` page: its filter and actions row, in its own `h-11` flex row. The shell frames it (border, padding) above `children`. */
  header?: ReactNode;
  children: ReactNode;
}

export interface DictationToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  /** Accessible name. */
  label: string;
  disabled?: boolean;
}

export interface DictationOriginIconProps {
  item: HistoryItem;
  size?: number;
  className?: string;
}

/** Visual pieces a host may replace. Each default is the package's own markup. */
export interface DictationComponents {
  /** Wrapper around the Dictation, Polish, Home and History pages (History also gets `header`). Default: `PageShell` from `@rightkit/react-shell/react`, `maxWidth` 1320, class `dictation-page dictation-page--<page>`. */
  PageShell?: ComponentType<DictationPageShellProps>;
  /** Switch on the Polish page. Default: `Toggle` from `@rightkit/react-shell/react`. */
  Toggle?: ComponentType<DictationToggleProps>;
  /** A chord on the Home page (`chord` is a `lib/chords` token string such as `fn+space`). Default: `Kbd` from `@rightkit/platform-ui/react`. */
  Kbd?: ComponentType<{ chord: string }>;
  /** The Developer tools' route test (`context` is `"developer"`). Without it, and without `hooks.useRecognitionSetup`, the section has no route-test button. */
  RecognitionSetupStep?: ComponentType<{ setup: RecognitionSetup; context: "developer" }>;
  /** Brand mark in the "where it lives" setup step. Default: nothing. */
  AppMark?: ComponentType;
  /** Reel under the model download row of the setup. It is shown while the model is not installed; pass a `React.lazy` component to keep its assets out of the first bundle. Default: no reel. */
  ModelReel?: ComponentType<{ className?: string }>;
  /** Leading app tile of a recent-history row on the Home page. Default: library-ui `OriginIcon` over `DictationClient.history`. */
  OriginIcon?: ComponentType<DictationOriginIconProps>;
}

/** What the Developer tools' route test works with: the host's own recognition-setup state (an opaque object with the `phase` the section reads). */
export interface RecognitionSetup {
  /** `"processing"` disables the route-test button. */
  phase: string;
}

/** Arguments of `hooks.useRecognitionSetup`. */
export interface RecognitionSetupOptions {
  active: boolean;
  setError: (message: string) => void;
}

/** Class names a host may replace. */
export interface DictationClassNames {
  /** Row action buttons (copy, edit, ...) on the Home page. Default: library-ui `rowActionChip`. */
  rowChip?: string;
}

/** Hooks a host may replace. A hook must be the same function for the life of the window. */
export interface DictationHooks {
  /**
   * The settings store every page reads and patches through. Default: `@rightkit/settings-ui`'s store over
   * `DictationClient.settings`. A host with its own store passes a hook with the `UseSettings` shape.
   */
  useSettings?: () => UseSettings;
  /** Recognition-setup state for the Developer tools' route test; pairs with `components.RecognitionSetupStep`. */
  useRecognitionSetup?: (options: RecognitionSetupOptions) => RecognitionSetup;
}

/** Browser-storage values the package keeps. Hosts with stored data pass their existing keys. */
export type DictationStorageKey =
  | "promptHintCount"
  | "promptHintSeen"
  | "promptHintDisabled"
  // The History list's keys: forwarded to `@rightkit/library-ui`'s host configuration.
  | "skipDeleteConfirm"
  | "groupSelection"
  | "recoveryDismissed";

const LIBRARY_STORAGE_KEYS = ["skipDeleteConfirm", "groupSelection", "recoveryDismissed"] as const;

export interface DictationHostConfig {
  /** Product name in user-facing copy. Default `"App"`. */
  appName?: string;
  /** Pages client. Required in the hub window. */
  client?: DictationClient;
  /** Pill client. Required in the pill and popup windows. */
  pill?: PillClient;
  /** Prefix for browser-storage keys without an explicit entry in `storageKeys`. Default `"rk-dictation"`. */
  storagePrefix?: string;
  /** Exact keys for stored values, e.g. a host's existing keys. */
  storageKeys?: Partial<Record<DictationStorageKey, string>>;
  /** What the first-run setup needs to know about the product; required by `@rightkit/dictation-ui/onboarding`. */
  onboarding?: DictationOnboardingConfig;
  /** Key labels that replace the platform's default in chords drawn by the setup, by platform (for example `{ macos: { esc: "esc" } }`). */
  chordLabels?: Partial<Record<Platform, Record<string, string>>>;
  /** URL of the spoken-formatting guide linked from the Dictation page. */
  formattingGuideUrl?: string;
  /** App shell (`@rightkit/dictation-ui/shell`): version and updater. */
  shell?: DictationShellConfig;
  /** Replacement visual pieces; see `DictationComponents`. */
  components?: DictationComponents;
  /** Replacement class names; see `DictationClassNames`. */
  classNames?: DictationClassNames;
  /** Replacement hooks; see `DictationHooks`. */
  hooks?: DictationHooks;
  /**
   * Where focus goes when the file-transcription modal closes; `opener` is the control that opened it (or
   * null). Default: focus `opener` on the next animation frame when it is still on the page.
   */
  restoreFocus?: (opener: HTMLElement | null) => void;
}

/** The speech model the first-run setup installs and selects. */
export interface DictationOnboardingModel {
  /** The backend id the native model commands take. */
  id: LocalBackend;
  /** Name shown in the model row and the setup copy. */
  label: string;
  /** Download size as shown, by platform. */
  size: { macos: string; other: string };
  /** The model's attribution line: `Powered by <name> · <licenseLabel>`. */
  credit: { name: string; licenseLabel: string; licenseUrl: string };
}

/** What the first-run setup (`@rightkit/dictation-ui/onboarding`) needs to know about the product. */
export interface DictationOnboardingConfig {
  model: DictationOnboardingModel;
  /** The spoken word that starts the voice commands (`<word> send`). */
  wakeWord: string;
  /** Written as `onboarding_version` when setup finishes. Default 3. */
  version?: number;
}

/** What the app shell (`@rightkit/dictation-ui/shell`) needs from the app's updater. Results are not read; rejections are swallowed by the shell where the app's code did. */
export interface DictationShellConfig {
  /** The installed version as shown, without the leading `v`. */
  appVersion: string;
  update: {
    /** Manual update check (never installs). */
    check(): Promise<unknown>;
    /** Downloads the available update, waits for dictation work to finish, installs and relaunches. */
    installAndRelaunch(): Promise<unknown>;
    /** Restarts into the installed update. */
    relaunch(): Promise<unknown>;
  };
}

let config: DictationHostConfig = {};

export function configureDictationHost(next: DictationHostConfig): void {
  config = { ...config, ...next };
  forwardToLibrary(next);
}

// The History page is `@rightkit/library-ui`'s list, which keeps its own host configuration (product name,
// storage keys). What this host sets for it goes there too, so an app makes one call.
function forwardToLibrary(next: DictationHostConfig): void {
  const library: LibraryHostConfig = {};
  if (next.appName !== undefined) library.appName = next.appName;
  if (next.storagePrefix !== undefined) library.storagePrefix = next.storagePrefix;
  if (next.storageKeys) {
    const keys: NonNullable<LibraryHostConfig["storageKeys"]> = {};
    for (const key of LIBRARY_STORAGE_KEYS) {
      const value = next.storageKeys[key];
      if (value !== undefined) keys[key] = value;
    }
    if (Object.keys(keys).length > 0) library.storageKeys = { ...keys };
  }
  if (Object.keys(library).length > 0) configureLibraryHost(library);
}

export function hostAppName(): string {
  return config.appName ?? "App";
}

export function dictationStorageKey(key: DictationStorageKey): string {
  return config.storageKeys?.[key] ?? `${config.storagePrefix ?? "rk-dictation"}.${key}`;
}

export function formattingGuideUrl(): string | undefined {
  return config.formattingGuideUrl;
}

export function dictationClient(): DictationClient {
  if (!config.client) throw new Error("@rightkit/dictation-ui: configureDictationHost({ client }) was not called");
  return config.client;
}

export function pillClient(): PillClient {
  if (!config.pill) throw new Error("@rightkit/dictation-ui: configureDictationHost({ pill }) was not called");
  return config.pill;
}

export function hostOnboarding(): DictationOnboardingConfig {
  if (!config.onboarding) throw new Error("@rightkit/dictation-ui: configureDictationHost({ onboarding }) was not called");
  return config.onboarding;
}

export function hostChordLabels(): NonNullable<DictationHostConfig["chordLabels"]> {
  return config.chordLabels ?? {};
}

export function hostShell(): DictationShellConfig {
  if (!config.shell) throw new Error("@rightkit/dictation-ui: configureDictationHost({ shell }) was not called");
  return config.shell;
}

export function hostComponents(): DictationComponents {
  return config.components ?? {};
}

export function hostClassNames(): DictationClassNames {
  return config.classNames ?? {};
}

export function hostHooks(): DictationHooks {
  return config.hooks ?? {};
}

export function hostRestoreFocus(): ((opener: HTMLElement | null) => void) | undefined {
  return config.restoreFocus;
}

/** Opens the pricing page. A failed open is logged, never thrown: callers are click handlers. */
export function openPricing(): void {
  void (async () => {
    try {
      await dictationClient().openPricing();
    } catch (error) {
      console.warn("@rightkit/dictation-ui: openPricing failed", error);
    }
  })();
}
