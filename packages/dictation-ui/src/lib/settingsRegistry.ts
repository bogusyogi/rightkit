// Settings search registry — one entry per user-facing setting row rendered
// with a `data-setting-id` attribute. Quick controls that live in the title
// bar (Push-to-talk, Middle-click, Pill) carry the same attribute
// on the title-bar control and use the "Title bar" group. Screenshot settings
// live under Controls > Voice commands. The command palette
// merges these into its results; choosing one navigates to the page and reveals the row (see
// `revealSetting`). Ids must match the `data-setting-id` values in the pages.

import type { TabKey } from "../components/Sidebar";
import type { OS } from "../hooks/usePlatform";

export interface SettingEntry {
  /** Matches the row's `data-setting-id` attribute, e.g. `settings.input-device`. */
  id: string;
  label: string;
  keywords: string[];
  page: TabKey;
  /** Group name shown as the palette hint: "Settings › Audio". */
  group: string;
  /** Platforms where its rendered control exists. */
  platforms?: readonly OS[];
  /** License state where its rendered control exists. */
  licenseTier?: "free" | "pro";
  /** Settings tab that holds the row (`page: "settings"` only); absent for rows in the title bar. */
  section?: SettingsSectionId;
  /** The row renders only while developer mode is on. */
  developerOnly?: boolean;
}

/** Ids of the Settings page tabs (see `productSections` in `pages/Settings.tsx`). */
export type SettingsSectionId =
  | "speech"
  | "audio"
  | "application"
  | "apple-sync"
  | "sync-provider"
  | "developer";

export const SETTINGS_REGISTRY: SettingEntry[] = [
  // __REGISTRY_ENTRIES__
  // Home
  {
    id: "home.dictate-into-any-app",
    label: "Dictate into any app",
    keywords: ["dictate", "start", "record", "dictation"],
    page: "home",
    group: "Home",
  },
  {
    id: "home.transcribe-a-file",
    label: "Transcribe a file",
    keywords: ["transcribe", "file", "audio", "video", "import"],
    page: "home",
    group: "Home",
  },

  // History
  {
    id: "history.select-mode",
    label: "Select",
    keywords: ["select", "export", "delete", "entries"],
    page: "history",
    group: "History",
  },
  {
    id: "history.date-filter",
    label: "Filter by date",
    keywords: ["filter", "date", "today", "week"],
    page: "history",
    group: "Filters",
  },
  {
    id: "history.app-filter",
    label: "Filter by app",
    keywords: ["filter", "app", "application"],
    page: "history",
    group: "Filters",
  },
  {
    id: "history.keep-history-for",
    label: "Keep history for",
    keywords: ["history", "retention", "storage", "delete"],
    page: "history",
    group: "Storage",
  },
  {
    id: "history.export",
    label: "Export history",
    keywords: ["export", "history", "transcripts"],
    page: "history",
    group: "Storage",
  },
  {
    id: "history.delete-all",
    label: "Delete all history",
    keywords: ["delete", "clear", "history", "transcripts"],
    page: "history",
    group: "Storage",
  },

  // Controls
  {
    id: "controls.start-or-stop-recording",
    label: "Start or stop recording",
    keywords: ["shortcut", "recording", "start", "stop"],
    page: "controls",
    group: "Keyboard",
  },
  {
    id: "controls.stop-and-send",
    label: "Stop and send",
    keywords: ["shortcut", "stop", "send", "enter"],
    page: "controls",
    group: "Keyboard",
  },
  {
    id: "controls.cancel",
    label: "Cancel recording",
    keywords: ["shortcut", "cancel", "discard", "recording"],
    page: "controls",
    group: "Keyboard",
  },
  {
    id: "controls.open-hub",
    label: "Open Hub",
    keywords: ["shortcut", "hub", "open", "window"],
    page: "controls",
    group: "Keyboard",
  },
  {
    id: "controls.zephyr-stop",
    label: "Zephyr: stop",
    keywords: ["zephyr", "wake word", "voice", "stop"],
    page: "controls",
    group: "Voice Commands",
  },
  {
    id: "controls.zephyr-send",
    label: "Zephyr: send",
    keywords: ["zephyr", "wake word", "voice", "send"],
    page: "controls",
    group: "Voice Commands",
  },
  {
    id: "controls.zephyr-cancel",
    label: "Zephyr: cancel",
    keywords: ["zephyr", "wake word", "voice", "cancel"],
    page: "controls",
    group: "Voice Commands",
  },
  {
    id: "controls.push-to-talk",
    label: "Push to talk",
    keywords: ["push to talk", "ptt", "hold", "record"],
    page: "controls",
    group: "Title bar",
  },
  {
    id: "controls.push-to-talk-key",
    label: "Push-to-talk key",
    keywords: ["push to talk", "ptt", "key", "modifier", "right alt", "right ctrl", "option"],
    page: "controls",
    group: "Title bar",
  },
  {
    id: "controls.add-to-send",
    label: "Add to send",
    keywords: ["push to talk", "ptt", "send", "enter"],
    page: "controls",
    group: "Push to Talk",
  },
  {
    id: "controls.middle-click",
    label: "Middle-click to start and stop",
    keywords: ["middle click", "mouse", "recording"],
    page: "controls",
    group: "Title bar",
  },

  // Dictation
  {
    id: "dictation.spoken-formatting",
    label: "Spoken formatting",
    keywords: ["formatting", "punctuation", "new line", "period"],
    page: "dictation",
    group: "Dictation",
  },
  {
    id: "dictation.vocabulary",
    label: "Vocabulary",
    keywords: ["vocabulary", "words", "names", "spelling"],
    page: "dictation",
    group: "Vocabulary",
  },
  {
    id: "dictation.snippets",
    label: "Snippets",
    keywords: ["snippets", "templates", "insert", "text"],
    page: "dictation",
    group: "Snippets",
  },
  {
    id: "dictation.text-replacements",
    label: "Text replacements",
    keywords: ["replacements", "substitutions", "abbreviations", "expand"],
    page: "dictation",
    group: "Text Replacements",
  },

  // Commands
  {
    id: "controls.voice-commands",
    label: "Voice commands",
    keywords: ["commands", "shortcuts", "actions", "catalog"],
    page: "controls",
    group: "Voice commands",
  },
  {
    id: "commands.send-screenshots-to",
    label: "Screenshots: clipboard or save",
    keywords: [
      "screenshots",
      "send screenshots to",
      "clipboard",
      "save",
      "pictures",
      "destination",
    ],
    page: "controls",
    group: "Voice commands",
  },

  // Polish
  {
    id: "polish.advanced-cleanup",
    label: "Advanced cleanup",
    keywords: ["cleanup", "fillers", "local", "on-device"],
    page: "polish",
    group: "Local cleanup",
  },
  {
    id: "polish.polish-with-ai",
    label: "Polish with AI",
    keywords: ["ai", "polish", "cleanup", "rewrite"],
    page: "polish",
    group: "AI polish",
  },
  {
    id: "polish.provider-key",
    label: "Provider key",
    keywords: ["api key", "provider", "openai", "anthropic", "key"],
    page: "polish",
    group: "AI polish",
  },

  // Settings
  {
    id: "settings.open-at-startup",
    label: "Open at startup",
    keywords: ["startup", "launch", "login", "boot"],
    page: "settings",
    group: "App",
    platforms: ["windows", "linux"],
    section: "application",
  },
  {
    id: "settings.open-at-startup",
    label: "Open at Login",
    keywords: ["startup", "launch", "login", "boot", "login items"],
    page: "settings",
    group: "App",
    platforms: ["macos"],
    section: "application",
  },
  {
    id: "settings.install-updates-automatically",
    label: "Install updates automatically",
    keywords: ["updates", "automatic", "install"],
    page: "settings",
    group: "App",
    section: "application",
  },
  {
    id: "settings.rotate-history-key",
    label: "Rotate encryption key",
    keywords: ["encryption", "key", "history", "rotate", "re-encrypt", "security"],
    page: "settings",
    group: "App",
    section: "application",
  },
  {
    id: "settings.show-pill",
    label: "Pill: always on or while recording",
    keywords: ["pill", "show pill", "floating", "widget", "indicator", "recording", "always on"],
    page: "settings",
    group: "Title bar",
  },
  {
    id: "settings.input-device",
    label: "Input device",
    keywords: ["microphone", "mic", "input", "audio", "device"],
    page: "settings",
    group: "Audio",
    section: "audio",
  },
  {
    id: "settings.sound-effects",
    label: "Sound effects",
    keywords: ["sounds", "effects", "ding", "audio", "feedback"],
    page: "settings",
    group: "Audio",
    section: "audio",
  },
  {
    id: "settings.speech-recognition-model",
    label: "Speech recognition model",
    keywords: ["model", "speech", "recognition", "parakeet", "whisper", "download"],
    page: "settings",
    group: "Speech Recognition",
    section: "speech",
  },
  {
    id: "settings.dictation-language",
    label: "Dictation language",
    keywords: ["language", "dictation", "speech", "multilingual"],
    page: "settings",
    group: "Speech Recognition",
    section: "speech",
  },
  {
    id: "settings.sync-with-apple-devices",
    label: "Sync with Apple devices",
    keywords: ["sync", "icloud", "apple", "iphone", "ipad", "mac"],
    page: "settings",
    group: "Apple Sync",
    platforms: ["macos"],
    section: "apple-sync",
  },
  {
    id: "settings.sync-provider",
    label: "Sync provider",
    keywords: ["sync", "google", "drive", "icloud", "provider", "migrate"],
    page: "settings",
    group: "Sync provider",
    section: "sync-provider",
  },
  {
    id: "settings.developer-mode",
    label: "Developer mode",
    keywords: ["developer", "advanced", "diagnostics"],
    page: "settings",
    group: "Developer Tools",
    section: "developer",
  },
  {
    id: "settings.diagnostics-and-routing",
    label: "Diagnostics and routing",
    keywords: ["diagnostics", "routing", "route test", "core ml", "cache", "telemetry"],
    page: "settings",
    group: "Developer Tools",
    section: "developer",
    developerOnly: true,
  },
  {
    id: "settings.export-crash-dumps",
    label: "Include crash dumps in the export",
    keywords: ["crash", "dumps", "diagnostics", "export", "developer"],
    page: "settings",
    group: "Developer Tools",
    section: "developer",
    developerOnly: true,
  },
  {
    id: "settings.local-experiments",
    label: "Local experiments",
    keywords: ["experiments", "flags", "developer"],
    page: "settings",
    group: "Developer Tools",
    section: "developer",
    developerOnly: true,
  },
  {
    id: "settings.save-recording-audio",
    label: "Save recording audio locally",
    keywords: ["save", "audio", "recordings", "diagnostics", "wav"],
    page: "settings",
    group: "Local experiments",
    section: "developer",
    developerOnly: true,
  },
  {
    id: "settings.experimental-hey-zephyr",
    label: "Hey Zephyr",
    keywords: ["hey zephyr", "wake word", "wake", "hands-free", "experimental"],
    page: "settings",
    group: "Local experiments",
    section: "developer",
    developerOnly: true,
  },
  {
    id: "settings.streaming-insertion",
    label: "Stream words into the text box",
    keywords: ["streaming", "live", "real-time", "type as you speak", "experimental"],
    page: "settings",
    group: "Local experiments",
    section: "developer",
    developerOnly: true,
  },

  // About
  {
    id: "about.send-feedback",
    label: "Send feedback",
    keywords: ["feedback", "bug", "report", "diagnostics"],
    page: "about",
    group: "Support",
  },
  {
    id: "about.email-support",
    label: "Email support",
    keywords: ["email", "support", "contact", "help"],
    page: "about",
    group: "Support",
  },
  {
    id: "about.call-support",
    label: "Call support",
    keywords: ["phone", "call", "support", "contact", "help"],
    page: "about",
    group: "Support",
  },
  {
    id: "about.plan",
    label: "Plan",
    keywords: ["plan", "pro", "free", "subscription", "license"],
    page: "about",
    group: "License",
  },
  {
    id: "about.license-key",
    label: "License key",
    keywords: ["license", "key", "activation", "activate", "pro"],
    page: "about",
    group: "License",
  },
  {
    id: "about.get-pro",
    label: "Get Pro",
    keywords: ["upgrade", "pro", "buy", "purchase"],
    page: "about",
    group: "License",
  },
  {
    id: "about.link-from-a-mac-or-iphone",
    label: "Link from a Mac or iPhone",
    keywords: ["link", "mac", "iphone", "ipad", "license"],
    page: "about",
    group: "License",
    platforms: ["windows"],
    licenseTier: "free",
  },
  {
    id: "about.link-a-windows-pc",
    label: "Link a Windows PC",
    keywords: ["link", "windows", "license"],
    page: "about",
    group: "License",
    platforms: ["macos"],
    licenseTier: "pro",
  },
  {
    id: "about.manage",
    label: "Manage license",
    keywords: ["manage", "license", "deactivate", "key"],
    page: "about",
    group: "License",
  },
  {
    id: "about.status",
    label: "Account status",
    keywords: ["account", "status", "signed in"],
    page: "about",
    group: "Account",
  },
  {
    id: "about.session",
    label: "Session",
    keywords: ["session", "token", "signed in"],
    page: "about",
    group: "Account",
  },
  {
    id: "about.sign-in",
    label: "Sign in",
    keywords: ["sign in", "login", "account", "email"],
    page: "about",
    group: "Account",
  },
  {
    id: "about.have-a-link-token",
    label: "Have a link token?",
    keywords: ["token", "link", "sign in", "email"],
    page: "about",
    group: "Account",
  },
  {
    id: "about.manage-on-the-web",
    label: "Manage on the web",
    keywords: ["web", "account", "orders", "devices"],
    page: "about",
    group: "Account",
  },
  {
    id: "about.sign-out",
    label: "Sign out",
    keywords: ["sign out", "logout", "account"],
    page: "about",
    group: "Account",
  },
  {
    id: "about.delete-account",
    label: "Delete account",
    keywords: ["delete", "account", "remove"],
    page: "about",
    group: "Account",
  },
  {
    id: "about.delete-account-confirm",
    label: "Confirm account deletion",
    keywords: ["delete", "account", "confirm"],
    page: "about",
    group: "Account",
  },
  {
    id: "about.devices",
    label: "Devices",
    keywords: ["devices", "linked", "deactivate"],
    page: "about",
    group: "Devices",
  },
];

const FOCUSABLE = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

const FLASH_MS = 1800;
const REVEAL_TIMEOUT_MS = 2500;

export function settingsForPlatform(
  platform: OS,
  isPro: boolean,
  developerMode = false,
): SettingEntry[] {
  return SETTINGS_REGISTRY.filter(
    (entry) =>
      (!entry.platforms || entry.platforms.includes(platform)) &&
      (!entry.licenseTier || (entry.licenseTier === "pro") === isPro) &&
      (!entry.developerOnly || developerMode),
  );
}

/** The searchable rows of one Settings tab, from the registry so the tab, search and palette agree. */
export function settingsEntriesForSection(
  section: SettingsSectionId,
  platform: OS,
  developerMode: boolean,
): { id: string; label: string; keywords: string[] }[] {
  return settingsForPlatform(platform, false, developerMode)
    .filter((entry) => entry.page === "settings" && entry.section === section)
    .map(({ id, label, keywords }) => ({ id, label, keywords }));
}

/** The Settings tab that holds a row, or undefined for rows that sit outside any tab. */
export function settingsSectionFor(id: string): SettingsSectionId | undefined {
  return SETTINGS_REGISTRY.find((entry) => entry.page === "settings" && entry.id === id)?.section;
}

// Page switches may suspend while their destination mounts. Focus its title-bar
// heading once the page is available so keyboard users land on visible destination context.
export function focusPageDestination(expectedTab?: TabKey): void {
  const deadline = performance.now() + REVEAL_TIMEOUT_MS;
  const attempt = () => {
    const page = expectedTab
      ? document.querySelector<HTMLElement>(`[data-page="${CSS.escape(expectedTab)}"]`)
      : document.querySelector<HTMLElement>("[data-page]");
    // The page title lives in the title bar, so wait for the page body and focus that heading.
    const heading = page ? document.querySelector<HTMLElement>(".rk-top h1") : null;
    const control = expectedTab ? null : page?.querySelector<HTMLElement>(FOCUSABLE) ?? null;
    const target = heading ?? control;
    if (target) {
      if (target === heading) target.tabIndex = -1;
      target.focus({ preventScroll: true });
      return;
    }
    if (performance.now() < deadline) window.requestAnimationFrame(attempt);
  };
  window.requestAnimationFrame(attempt);
}

// Native dialogs restore focus only while their opener remains usable. A
// removed launcher falls back to current page rather than losing focus.
export function focusConnectedOrPageDestination(opener: HTMLElement | null): void {
  window.requestAnimationFrame(() => {
    if (opener?.isConnected && opener.matches(FOCUSABLE)) {
      opener.focus({ preventScroll: true });
      return;
    }
    focusPageDestination();
  });
}

// After a palette-driven page switch the target page may still be inside its
// lazy Suspense boundary, so poll each frame until the row mounts (bounded).
// Highlights briefly (the CSS animation is stripped under reduced motion, in
// which case the static ring below remains for the same duration) and moves
// focus to the row's first control.
// `focus: false` scrolls and flashes without moving focus, for route aliases
// that land on a section rather than a specific control.
export function revealSetting(id: string, { focus = true }: { focus?: boolean } = {}): void {
  const deadline = performance.now() + REVEAL_TIMEOUT_MS;
  const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
  const attempt = () => {
    const el = document.querySelector<HTMLElement>(`[data-setting-id="${CSS.escape(id)}"]`);
    if (!el) {
      if (performance.now() < deadline) window.requestAnimationFrame(attempt);
      return;
    }
    el.scrollIntoView({ block: "center", behavior: reduced ? "auto" : "smooth" });
    el.classList.add("hr-setting-flash");
    window.setTimeout(() => el.classList.remove("hr-setting-flash"), FLASH_MS);
    const explicitFocusTarget = el.matches('[tabindex="-1"]')
      ? el
      : el.querySelector<HTMLElement>('[tabindex="-1"]');
    const control = explicitFocusTarget ?? (el.matches(FOCUSABLE) ? el : el.querySelector<HTMLElement>(FOCUSABLE));
    if (focus) control?.focus({ preventScroll: true });
  };
  window.requestAnimationFrame(attempt);
}
