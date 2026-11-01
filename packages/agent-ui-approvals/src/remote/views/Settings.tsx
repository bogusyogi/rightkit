import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import { useSearchKeys, useSettingsSearch, useSettingsSearchField } from "@rightkit/settings-ui/headless";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Badge, Button, Checkbox, ConfirmDialog, IconButton, SegmentedControl, Select, Slider, Toggle, Input } from "@rightkit/react-shell/react";
import { formatChord, getPlatform } from "@rightkit/platform-ui";
import type { AutoUpdateStatus } from "@rightkit/app-update";
import { UpdatesControls, type UpdatesController } from "@rightkit/settings-ui/standard";
import { ChevronDown, ChevronUp, Cpu, HardDrive, MemoryStick, Pencil, Send as SendIcon, Wrench, X } from "lucide-react";
import { LauncherLists, LauncherSearchRows, chordName, type LauncherConfig } from "@rightkit/system-ui/views/LauncherSettings";
import { LocalNetworkRow, NearbyGroup } from "@rightkit/system-ui/views/NearbySettings";
import { FileIndexLadderRows, HubHealthRows, NumberRow, ProtectedPinPrompt, RemoteAccessGroup } from "./RemoteSettings";
import { SecureBoxGroup } from "./SecureBoxSettings";
import { DevicesSection } from "./Devices";
import { api, devicesApi, isProtectedKey, isWindows, setProtectedSetting, tone, type DeviceRecord, type DiskIndexHealth } from "@rightkit/system-ui/api";
import { Bar } from "@rightkit/system-ui/views/Storage";
import { SettingRow, SettingsGroup, SettingsItem, SettingsSearchProvider, SettingsSearchSection } from "@rightkit/settings-ui/core";
import { NeedsLine, openSectionLabel, windowShortcutFrom } from "@rightkit/system-ui/views/settingsKit";
import { SHOW_ONBOARDING } from "@rightkit/system-ui/onboarding/gate";
import "@rightkit/system-ui/views/settings.css";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";

export interface Limit {
  label: string;
  /** 0–1, how much of the window is used. */
  usedFraction: number;
  /** Length of the window; absent when the provider did not say. */
  seconds?: number;
}

/** One window of a Claude account's last saved reading (Sources/Providers/ClaudeAccountBook.swift). */
export interface ClaudeWindow {
  label: string;
  usedFraction: number;
  /** Epoch seconds when the window resets; past it the saved percentage is no longer true. */
  resetsAt: number;
  seconds?: number;
}

/** A Claude account on this Mac: each account folder Claude Desktop has, the signed-in one live, the rest as last saved or with no reading yet. */
export interface ClaudeAccount {
  id: string;
  name: string;
  email?: string;
  plan?: string;
  active: boolean;
  /** False only when the account's folder is gone; only such an account can be forgotten. */
  onDisk?: boolean;
  /** Epoch seconds of the last reading; null before the first. */
  capturedAt: number | null;
  windows: ClaudeWindow[];
}

export interface Account {
  id: string;
  name: string;
  connected: boolean;
  usesKeychain: boolean;
  refusedAccess: boolean;
  needsRenewal: boolean;
  summary?: string;
  signInTitle?: string;
  signInExplanation: string;
  limits?: Limit[];
  /** ISO 8601 UTC time of the last successful read. Absent on notches that do not publish it. */
  lastReadAt?: string;
  /** Short category of the latest failed attempt ("rate limited", "offline"...); absent when it succeeded. */
  lastError?: string;
  /** A plain sentence about whose numbers these are (for example Claude Code signed in to a different plan than Claude Desktop); absent when there is nothing to say. */
  notice?: string;
  /** Only on the Claude row. */
  claudeAccounts?: ClaudeAccount[];
  /** Only on the Claude row: the command-line account new terminals use (`pulse claude use`); absent until one exists. */
  claudeCliAccount?: { active?: string; accounts: string[] };
}

interface AppRef {
  id: string;
  name: string;
}

interface Conveniences {
  accessibility: boolean;
  inputMonitoring: boolean;
  wanted: boolean;
  fnStatus: "off" | "running" | "needsAccessibility" | "failed";
  fnDetail: string;
  active: boolean;
  runningApps: AppRef[];
  autoQuitApps: AppRef[];
  cutPasteResults: { name: string; ok: boolean; detail: string }[];
  windowManagement?: WindowManagementState;
  /** The optional global shortcut that opens Settings (Conveniences/Windows/SettingsHotKey.swift). */
  settingsHotkey?: { shortcut: string; display: string; error: string };
}

/** One window action, as the notch lists it (Conveniences/Windows/WindowAction.swift). */
interface WindowShortcutState {
  id: string;
  title: string;
  group: string;
  groupTitle: string;
  /** Shortcut text such as "ctrl+opt+left"; empty when the action has none. */
  shortcut: string;
  display: string;
  default: string;
  error: string;
}

interface WindowManagementState {
  enabled: boolean;
  actions: WindowShortcutState[];
}

export interface NotchState {
  /** "windows" from the Windows notch; the Mac notch does not say. */
  platform?: string;
  permissions?: Permission[];
  permissionErrors?: Record<string, string>;
  conveniences?: Conveniences;
  version: string;
  settings: Record<string, string | number | boolean>;
  options: Record<string, string[]>;
  displays: { id: string; name: string }[];
  accounts: Account[];
  /** Every notch cell in the notch's order; absent from a notch that has no Gauges group yet. */
  gauges?: Gauge[];
  providerOrder: string[];
  launcherStatus?: string | null;
  /** The clipboard strip's shortcut problem ("In use by another program"), as `launcherStatus` is the launcher's; absent or null when fine. */
  clipboardStatus?: string | null;
  /** What the clipboard holds, for the Clear row: items and bytes. Counts only, never the items. */
  clipboardCount?: number;
  clipboardBytes?: number;
  /** The Windows notch's window shortcuts (the Mac's are under `conveniences`). */
  windowManagement?: WindowManagementState;
  /** The Windows notch's Auto Quit lists (the Mac's are under `conveniences`): the apps chosen, by id and name, and the running apps that can be added. */
  autoQuit?: { autoQuitApps: AppRef[]; runningApps: AppRef[] };
  /** The Windows notch's shortcut that opens Settings (the Mac's is under `conveniences`). */
  settingsHotkey?: { shortcut: string; display: string; error: string };
  helper?: "notRegistered" | "requiresApproval" | "enabled" | "notFound";
  helperError?: string | null;
  /** The resident Membrane: "running", "starting", "not installed" or "latched: <reason>"; absent from an older notch. */
  membrane?: string;
  /** The hub process as the notch supervises it: "running", "starting", "restarting", "not installed" or "latched: <reason>"; absent from an older notch. */
  hubProcess?: string;
  updates?: UpdateState;
}

/** the host app's updater, as the notch reports it (Sources/App/Updater.swift). */
export interface UpdateState {
  current: string;
  /** The build number (CFBundleVersion); absent from older notches. */
  build?: string | null;
  /** The newer version waiting, if any. */
  available: string | null;
  lastChecked: string | null;
  status: "idle" | "checking" | "upToDate" | "available" | "downloading" | "installing" | "failed";
  message?: string;
  /** 0–1 while downloading. */
  progress?: number;
}

function updateLine(u: UpdateState): string | undefined {
  // Nothing until the notch has reported a status; saying "Not checked yet" before that would be a guess.
  if (!u.status) return undefined;
  switch (u.status) {
    case "checking": return "Checking for updates…";
    case "upToDate": return u.message ?? "Pulse is up to date.";
    case "available": return `Pulse ${u.available} is available.`;
    case "downloading":
      return u.progress === undefined ? "Downloading…" : `Downloading… ${Math.round(u.progress * 100)}%`;
    case "installing": return "Installing. Pulse reopens when it is done.";
    case "failed": return `Update failed: ${u.message ?? "unknown reason"}`;
    default: return u.lastChecked ? "Not checked since launch." : "Not checked yet.";
  }
}

/** the host app's update state as the shared Updates controls show it (`UpdatesController`). */
function updatesStatus(u: UpdateState): AutoUpdateStatus {
  switch (u.status) {
    case "checking": return { kind: "checking" };
    case "upToDate": return { kind: "up_to_date" };
    case "available": return u.available ? { kind: "available", version: u.available, tier: null, eligible: true } : { kind: "idle" };
    case "downloading": return { kind: "downloading", version: u.available ?? "", tier: null, pct: (u.progress ?? 0) * 100 };
    case "failed": return { kind: "error", message: u.message ?? "unknown reason" };
    default: return { kind: "idle" };
  }
}

/** The the host app version row: its status line, and Check now / Update from the shared controls. Check and Update send the notch's own commands. */
function UpdatesRow({ u, send }: { u: UpdateState; send: Send }) {
  const installing = ["downloading", "installing"].includes(u.status);
  const controller: UpdatesController = {
    status: updatesStatus(u),
    busy: ["checking", "downloading", "installing"].includes(u.status),
    pending: null,
    check: async () => send({ command: "checkUpdates" }),
    install: async () => send({ command: "installUpdate" }),
  };
  const note = [updateLine(u), u.available && !installing ? "Downloads from GitHub, checks Apple's signature and notarization, then replaces Pulse in Applications and reopens it." : undefined];
  return (
    <SettingRow className="ck-set" label={`Version ${u.current}${u.build ? ` (build ${u.build})` : ""}`} description={note.filter(Boolean).join(" ")}>
      <UpdatesControls controller={controller} showStatus={false} labels={{ checkNow: "Check now", installUpdate: "Update" }} />
    </SettingRow>
  );
}

export interface Permission {
  id: string;
  title: string;
  why: string;
  status: "granted" | "needsApproval" | "off" | "unknown";
  required: boolean;
  /** The features that need this permission, from what is switched on now (Mac notch only). */
  needs?: string[];
}

/** One notch cell the Gauges group can turn on or off (the Mac notch's `gauges`). */
export interface Gauge {
  id: string;
  name: string;
  /** The notch's glyph for the cell (cpu, memory, disk, send, tools, claude, openai, ...). */
  glyph: string;
  connected: boolean;
  /** Position in the notch's provider order, from 0. */
  order: number;
}

type Send = (command: Record<string, unknown>) => void;

export function useNotch() {
  const [state, setState] = useState<NotchState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(() => {
    invoke<NotchState>("notch_state")
      .then((s) => {
        setState(s);
        setError(null);
      })
      .catch((e) => setError(String(e)));
  }, []);
  useEffect(() => {
    const un = listen("notch-state", load).then((unlisten) => { load(); return unlisten; });
    const poll = visibleInterval(load, 3000);
    return () => {
      clearVisibleInterval(poll);
      void un.then((f) => f());
    };
  }, [load]);
  const send: Send = (command) => {
    // A security setting never goes through a hub-commands file: the engine checks the caller and,
    // for a change that loosens a gate, the the host app PIN (`ProtectedPinPrompt` asks for it).
    if (command.command === "set" && typeof command.key === "string" && isProtectedKey(command.key)) {
      void setProtectedSetting(command.key, command.value).catch((e) => setError(String(e)));
      return;
    }
    void invoke<{ status?: string; reason?: string }>("notch_command", { command })
      .then((result) => {
        // The notch removes the request only after it applied and saved it; say so when it did not.
        if (result?.status === "rejected" || result?.status === "not_saved") {
          setError(`The change was not saved (${result.reason ?? result.status}).`);
        }
      })
      .catch((e) => setError(String(e)));
  };
  return { state, error, send };
}

const isFiniteNumber = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

/** "darkGlass" → "Dark glass". */
// "mac" is the notification channel that uses the system's own notifications: the Mac's reads
// "Mac", so the Windows notch's reads "Windows".
const label = (raw: string) =>
  isWindows && raw === "mac" ? "Windows" : raw.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/^./, (c) => c.toUpperCase()).replace(/ ([A-Z])/g, (_, c) => ` ${c.toLowerCase()}`);

/** The Ask row picker's names, by the value `launcherAskTarget` holds. */
const ASK_TARGET_NAMES: Record<string, string> = {
  inLauncher: "In the launcher",
  chatgpt: "ChatGPT",
  off: "Off",
};

/** The Language picker's names, by the code the Mac notch publishes (its `AppLanguage.title`). */
const LANGUAGE_NAMES: Record<string, string> = {
  system: "Follow System",
  en: "English",
  fr: "Français",
  de: "Deutsch",
  id: "Bahasa Indonesia",
  ja: "日本語",
  ko: "한국어",
  "pt-BR": "Português (Brasil)",
  ru: "Русский",
  "zh-Hans": "简体中文",
  "zh-Hant": "繁體中文",
  uk: "Українська",
  uz: "Oʻzbekcha",
  tr: "Türkçe",
};

/** The Accent picker's names, by the raw value the notch publishes (the Mac's `AccentColorChoice`: "36a8eb" is Blue). */
const ACCENT_NAMES: Record<string, string> = {
  // The Windows notch's "system" follows the accent colour chosen in Windows Settings > Personalisation > Colours.
  system: isWindows ? "Windows accent colour" : "Device accent color",
  ff33e1: "Pink",
  eb4236: "Red",
  eb8436: "Orange",
  ffd400: "Yellow",
  "00ff88": "Green",
  "00e5cc": "Teal",
  "36a8eb": "Blue",
  "6c5ce7": "Indigo",
  b026ff: "Purple",
  f7f6f5: "Off-white",
};

/** The Tools cell's actions (Sources/Conveniences/Tools.swift), each with its own switch. */
const TOOLS = [
  { id: "snip", name: "Snip", note: "Drag a region to copy the screenshot to the clipboard." },
  { id: "screen", name: "Screen", note: "Copy a screenshot of the whole screen to the clipboard." },
  { id: "window", name: "Window", note: "Copy a screenshot of one window to the clipboard." },
  { id: "lock", name: "Lock screen", note: "Lock the screen." },
  { id: "clip", name: "Clipboard", note: "Open the clipboard strip. Absent while clipboard history is off." },
];

// What the middle-click wheel can hold, in wheel order; `on` marks the default seven.
const WHEEL = [
  { id: "snip", name: "Snip", note: "Drag a region to copy the screenshot to the clipboard.", on: true },
  { id: "screen", name: "Screen", note: "Copy a screenshot of the whole screen to the clipboard.", on: true },
  { id: "window", name: "Window", note: "Copy a screenshot of one window to the clipboard.", on: true },
  { id: "settings", name: "System Settings", note: "Open macOS System Settings (not Pulse's settings).", on: true },
  { id: "hub", name: "Open hub", note: "Open the Pulse hub.", on: true },
  { id: "launcher", name: "Launcher", note: "Open the Pulse launcher; absent from the wheel while the launcher is off.", on: true },
  { id: "clip", name: "Clipboard history", note: "Opens the clipboard strip (Clipboard history in the launcher when the strip is off).", on: true },
  { id: "notes", name: "Notes", note: "Open the launcher on notes; absent while the launcher or its notes are off.", on: false },
  { id: "lock", name: "Lock screen", note: "Lock the screen.", on: false },
  { id: "mission", name: "Mission Control", note: "Open Mission Control.", on: false },
];
const WHEEL_DEFAULT = WHEEL.filter((t) => t.on).map((t) => t.id);

/**
 * Settings with a search box. Typing shows every section at once with only the rows whose
 * label, note, group title or section title contain every typed word (see settingsKit.tsx), the match
 * marked; each row stays live, and "Open <section>" jumps to its page. Escape clears; the
 * platform's find shortcut (Command F, Control F on Windows) focuses the box.
 */
export function Settings({ section, notch, onNavigate }: {
  section: string;
  notch: ReturnType<typeof useNotch>;
  onNavigate: (section: string) => void;
}) {
  const [text, setText] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const query = text.trim();
  const open = (id: string) => {
    setText("");
    onNavigate(id);
  };
  return (
    <SettingsSearchProvider
      highlight={(match: string) => <mark className="ck-mark">{match}</mark>}
      wrapHighlight={false}
      query={query}
    >
      <div className="view ck-settings">
        <ProtectedPinPrompt />
        <SearchBar text={text} setText={setText} input={input} />
        <SettingsBody section={section} notch={notch} onNavigate={onNavigate} searching={query !== ""} open={open} />
      </div>
    </SettingsSearchProvider>
  );
}

/**
 * The search box and the "no match" line. Escape clears only from the box or the page itself
 * (not from a rename field or a dialog); the platform's find shortcut focuses the box.
 */
function SearchBar({ text, setText, input }: {
  text: string;
  setText: (text: string) => void;
  input: RefObject<HTMLInputElement | null>;
}) {
  const field = useSettingsSearchField({ query: text, setQuery: setText });
  useSearchKeys({
    inputRef: input,
    query: text,
    setQuery: setText,
    findShortcut: (e) => (isWindows ? e.ctrlKey : e.metaKey) && !e.altKey && e.key.toLowerCase() === "f",
    escapeClears: "unless-in-input-or-dialog",
  });
  return (
    <>
      <div className="ck-search" role="search">
        <Input ref={input} type="search" className="ck-input" aria-label="Search settings" placeholder="Search settings"
          value={field.value} onChange={field.onChange} />
        {text !== "" && <Button size="sm" variant="ghost" onClick={() => { field.clear(); input.current?.focus(); }}>Clear</Button>}
      </div>
      {field.active && field.count === 0 && (
        <div className="ck-sub ck-search-none" role="status">No setting matches “{text.trim()}”.</div>
      )}
    </>
  );
}

/** The file index's health from the hub's own status, polled while Settings is open; null when healthy, absent or unsupported. */
/** A process the notch supervises (the hub, Membrane): its status in words, and Retry once it is latched. */
function ProcessRow({ label, status, onRetry }: { label: string; status: string; onRetry: () => void }) {
  const latched = status.startsWith("latched:");
  const words: Record<string, string> = {
    running: "Running", starting: "Starting", restarting: "Restarting", "not installed": "Not installed",
    "engine starting": "Starting (waiting for the engine)", "engine down": "Stopped (the engine is not running)",
  };
  return (
    <SettingRow className="ck-set" label={label} description={latched ? `Stopped:${status.slice("latched:".length)}` : words[status] ?? status}>
      {latched ? <Button size="sm" variant="secondary" onClick={onRetry}>Retry</Button> : null}
    </SettingRow>
  );
}

function useFileIndexHealth(): DiskIndexHealth | null {
  const [health, setHealth] = useState<DiskIndexHealth | null>(null);
  useEffect(() => {
    let alive = true;
    const probe = () => {
      api.diskIndexStatus()
        .then((status) => { if (alive) setHealth(status.health?.state === "unhealthy" ? status.health : null); })
        .catch(() => { if (alive) setHealth(null); });
    };
    probe();
    const timer = visibleInterval(probe, 10000);
    return () => {
      alive = false;
      clearVisibleInterval(timer);
    };
  }, []);
  return health;
}

/** `sendDefaultDevice` (#471): which paired device a paste on the Send cell goes to. Empty is automatic. */
function DefaultPasteRow({ value, onChange }: { value: string; onChange: (v: string) => void }) {
  const [devices, setDevices] = useState<DeviceRecord[]>([]);
  useEffect(() => {
    let live = true;
    devicesApi.list().then((r) => { if (live) setDevices(r.devices); }).catch(() => {});
    return () => { live = false; };
  }, []);
  const known = devices.some((d) => d.deviceId === value);
  const options = [
    { value: "", label: "Automatic (the only other computer)" },
    ...devices.map((d) => ({ value: d.deviceId, label: d.name || d.deviceId })),
    ...(value && !known ? [{ value, label: value }] : []),
  ];
  return (
    <SettingRow className="ck-set" label="Default computer for paste"
      description="Pasting on the Send cell sends the clipboard straight here, with no picker. Automatic: with exactly one other Mac or Windows computer paired, that one; otherwise Send asks.">
      <Select label="Default computer for paste" value={value} onChange={onChange} options={options} />
    </SettingRow>
  );
}

function SettingsBody({ section, notch, onNavigate, searching, open }: {
  section: string;
  notch: ReturnType<typeof useNotch>;
  onNavigate: (section: string) => void;
  searching: boolean;
  open: (section: string) => void;
}) {
  const { state, error, send } = notch;
  const fileIndexHealth = useFileIndexHealth();
  const show = (id: string) => searching || section === id;
  // Windows reads its Permissions rows from the system; no notch state is needed for them.
  if (section === "permissions" && isWindows && !searching) return <WindowsPermissions notchRows={state?.permissions} nearby={Boolean(state?.settings.nearbyEnabled)} />;
  // Devices come from the resident engine, not the notch, so the page does not wait for notch state.
  // `agentBrowser` (bogusyogi/pulse#460) is read by key name until the generated settings carry it; off hides the logins.
  const browserOn = (state?.settings as unknown as Record<string, unknown> | undefined)?.agentBrowser === true;
  if (section === "devices" && !searching) return <DevicesSection onOpen={open} browserOn={browserOn} />;
  if (!state) {
    return <div className="ck-sub" role="status">{error ?? "Reading the notch's settings…"}</div>;
  }
  const s = state.settings;
  const toolsOff = Array.isArray((s as Record<string, unknown>).toolsOff) ? ((s as Record<string, unknown>).toolsOff as string[]) : [];
  const wheelTools = Array.isArray((s as Record<string, unknown>).wheelTools) ? ((s as Record<string, unknown>).wheelTools as string[]) : WHEEL_DEFAULT;
  const windows = isWindows || state.platform === "windows";
  // The Windows notch says which wheel tools it offers; the Mac offers them all.
  const offered = (s as Record<string, unknown>).wheelToolsOffered;
  const wheel = WHEEL.filter((t) => !Array.isArray(offered) || offered.includes(t.id)).map((t) =>
    windows && t.id === "settings" ? { ...t, name: "Windows Settings", note: "Open Windows Settings (not Pulse's settings)." } : t);
  // The Windows notch publishes and accepts only part of the Mac's settings: a control shows
  // only when the notch published its key (a picker, when it published options for it).
  const has = (key: string) => key in s;
  const set = (key: string, value: unknown) => send({ command: "set", key, value });
  const bool = (key: string, text: string, note?: string) => !has(key) ? null : (
    <SettingRow className="ck-set" label={text} description={note}>
      <Toggle checked={Boolean(s[key])} onChange={(v) => set(key, v)} label={text} />
    </SettingRow>
  );
  const choice = (key: string, text: string, segmented = true, note?: string) => {
    const options = state.options[key] ?? [];
    if (options.length === 0) return null;
    // Accent colours and languages are published as raw values (hex, language codes); the picker names them.
    const names = key === "accentColor" ? ACCENT_NAMES : key === "language" ? LANGUAGE_NAMES : undefined;
    const name = (o: string) => names?.[o] ?? (key === "launcherHotkey" ? chordName(o) : key === "launcherAskTarget" ? ASK_TARGET_NAMES[o] ?? label(o) : key.endsWith("SoundName") ? o : label(o));
    return (
      <SettingRow className="ck-set" label={text} description={note}>
        {segmented && options.length <= 4 ? (
          <SegmentedControl
            label={text}
            value={String(s[key])}
            options={options.map((o) => ({ value: o, label: name(o) }))}
            onChange={(v) => set(key, v)}
          />
        ) : (
          <Select label={text} value={String(s[key])} onChange={(v) => set(key, v)}
            options={options.map((o) => ({ value: o, label: name(o) }))} />
        )}
      </SettingRow>
    );
  };
  const slider = (key: string, text: string, min: number, max: number, step: number, percent = false) => !isFiniteNumber(s[key]) ? null : (
    <SettingRow className="ck-set" label={text}>
      <span className="ck-slider">
        <Slider min={min} max={max} step={step} value={Number(s[key])} label={text}
          onChange={(v) => set(key, v)} />
        <span className="ck-sub ck-num">
          {percent ? `${Math.round(Number(s[key]) * 100)}%` : `${Number(s[key]).toFixed(2)}×`}
        </span>
      </span>
    </SettingRow>
  );

  return (
    <>
      {error && <div className="error" role="alert">{error}</div>}

      {show("permissions") && (
        <SettingsSearchSection headingLevel={3} passthroughWhenInactive openLabel={openSectionLabel} className="ck-search-section" id="permissions" title="Permissions" onOpen={open}>
          {isWindows && searching ? <WindowsPermissions notchRows={state.permissions} nearby={Boolean(s.nearbyEnabled)} /> : (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Permissions">
              {state.permissions ? <PermissionLead permissions={state.permissions} /> : null}
              {!state.permissions ? <div className="ck-set ck-sub" role="status">Waiting for permission status from Pulse notch…</div> : (
                <PermissionRows permissions={state.permissions} errors={state.permissionErrors} send={send} />
              )}
            </SettingsGroup>
          )}
        </SettingsSearchSection>
      )}

      {show("accounts") && (
        <SettingsSearchSection headingLevel={3} passthroughWhenInactive openLabel={openSectionLabel} className="ck-search-section" id="accounts" title="Accounts" onOpen={open}>
          <Accounts state={state} send={send} />
        </SettingsSearchSection>
      )}

      {searching && <DevicesSection onOpen={open} />}

      {show("appearance") && (
        <SettingsSearchSection headingLevel={3} passthroughWhenInactive openLabel={openSectionLabel} className="ck-search-section" id="appearance" title="Appearance" onOpen={open}>
          {state.gauges && state.gauges.length > 0 && <GaugesGroup gauges={state.gauges} send={send} />}
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Placement">
            {choice("notchEdge", "Edge")}
            {(state.options.notchEdge ?? []).length > 0 && (
              <div className="ck-sub ck-foot">
                {windows ? "Alt-drag the notch to slide it along its edge, or towards another edge to dock there." : "Option-drag the notch to slide it along its edge, or towards another edge to dock there."}
              </div>
            )}
            {choice("notchScope", "Displays")}
            {has("displayPreference") && (
              <SettingRow className="ck-set" label="Display">
                <Select label="Display" value={String(s.displayPreference)}
                  onChange={(v) => set("displayPreference", v)}
                  options={[{ value: "followActiveWindow", label: "Follow active window" }, ...state.displays.map((d) => ({ value: d.id, label: d.name }))]} />
              </SettingRow>
            )}
            {choice("notchVisibility", "Show")}
            {bool("notifications", "Notifications",
              "Notifications turns off Pulse's toasts and alert sounds; cards that answer something you did, or that need an answer, still show. On by default. With Show set to Hidden, or the notch folded for a full-screen app, a toast brings the notch out and it goes again.")}
            {bool("foldsForFullScreen", "Fold for full-screen apps")}
            <SettingRow className="ck-set" label="Position"><Button size="sm" variant="secondary" onClick={() => send({ command: "resetPosition" })}>Reset position</Button></SettingRow>
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Size and surface">
            {choice("notchSize", "Size")}
            {bool("usesCustomNotchScale", "Custom size")}
            {has("usesCustomNotchScale") && s.usesCustomNotchScale ? slider("customNotchScale", "Scale", 0.5, 1.5, 0.05) : null}
            {choice("notchSurfaceStyle", "Surface", true,
              windows ? "Windows paints every surface solid black; the choice is saved for the Mac." : undefined)}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Notch parts">
            {bool("showsSettingsHandle", "Settings handle",
              "The gear at the end of the notch, with its update and permission dots. On by default. Off hides it; Settings stays reachable by clicking a ring, from the hub's sidebar, and with the Open Settings shortcut.")}
            {bool("notchOptionDrag", windows ? "Alt-drag to move the notch" : "Option-drag to move the notch",
              `Holding ${windows ? "Alt" : "Option"} and dragging slides the notch along its edge. On by default. Off leaves the Edge row and Reset position; the grip beside the gear still moves it.`)}
            {bool("showsHoverCards", "Hover cards",
              "The card that opens when the pointer rests on a ring. On by default. The Tools buttons and the Send device list live on those cards"
                + (has("toolWheelEnabled") ? "; the middle-click wheel still offers the tools." : "."))}
            {bool("notchMinimizeButton", "Minimise button",
              "The button at the end of the notch opposite the drag handle: minimises the notch to its Send cell and restores it. Off: the notch is never minimised.")}
            {bool("sendReceivedShelf", "Show received files on the Send card",
              "Received files stay on the Send card to drag out or open.")}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Rings">
            {bool("showsNotchReadings", "Show % under rings")}
            {choice("weeklyRing", "Second ring")}
            {bool("weeklyRingDashed", "Dashed second ring")}
            {bool("weeklyHeadline", "Weekly limit as main ring")}
            {bool("weeklyReading", "Show both readings")}
            {bool("claudeDailyPaceRing", "Claude daily pace ring")}
            {bool("claudeShowAllAccounts", "Ring per Claude account", "One Claude ring for each known account, tagged A, B and so on. Off shows only the signed-in account's ring.")}
            {bool("showUsagePace", "Show usage pace")}
            {bool("showCodexExtraLimits", "Codex extra limits")}
            {choice("resetTimeFormat", "Reset time")}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Colour">
            {choice("accentColor", "Accent", false)}
            {choice("colorTransitionStyle", "Colour transition")}
            {slider("watchLimit", "Watch limit", 0.1, 0.95, 0.05, true)}
            {slider("criticalLimit", "Critical limit", 0.15, 1, 0.05, true)}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Language">{choice("language", "Language", false)}</SettingsGroup>
        </SettingsSearchSection>
      )}

      {show("notifications") && (
        <SettingsSearchSection headingLevel={3} passthroughWhenInactive openLabel={openSectionLabel} className="ck-search-section" id="notifications" title="Notifications" onOpen={open}>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Where">
            {choice("notificationChannel", "Channel")}
            {choice("peekDuration", "Open the notch for", false)}
            {!windows && has("channelBannerSound")
              ? bool("channelBannerSound", "Play a sound on banners",
                "Off by default. With Mac notifications chosen, a banner is silent unless this is on; it then plays the default alert sound. The notch's own sounds are the switches below.")
              : null}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Agent sessions">
            {bool("watchesAgentSessions", "Watch agent sessions",
              "Shows running Claude Code and Codex chats on the rings and opens the notch when one finishes. Off by default: nothing is watched, so there is no movement, no announcement and no sound for a finished chat.")}
            {bool("announceSessionEnd", "When a session finishes")}
            {bool("sessionEndSound", "Play a sound",
              "Off by default: a finished chat is silent unless this is on. It plays the sound chosen below.")}
            {choice("sessionEndSoundName", "Sound", false, windows
              ? "Which stock Windows sound a finished session plays. Windows Notify System Generic by default."
              : "Which macOS alert sound a finished session plays. Glass by default.")}
            {choice("sessionBlockedSoundName", "Blocked sound", false, windows
              ? "Which stock Windows sound a Claude Code session plays when it stops to wait for you. Windows Notify Calendar by default."
              : "Which macOS alert sound a session plays when it stops to wait for you. Funk by default.")}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Limits">
            {bool("announceUsageReset", "When a limit resets")}
            {bool("usageResetSound", "Play a sound on reset")}
            {choice("usageResetSoundName", "Reset sound", false, windows
              ? "Which stock Windows sound a limit reset plays, and a drive that is filling. Windows Notify Messaging by default."
              : "Which macOS alert sound a limit reset plays. Glass by default.")}
            {bool("announceSessionLimitReached", "When the session limit is reached")}
            {bool("announceWeeklyLimitReached", "When the weekly limit is reached")}
            {bool("limitReachedSound", "Play a sound at a limit")}
            {choice("limitReachedSoundName", "Limit sound", false, windows
              ? "Which stock Windows sound a spent limit plays, and a full or failing drive. Windows Exclamation by default."
              : "Which macOS alert sound a spent limit plays, and a full or failing drive. Funk by default.")}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Drives">
            {bool("announceDriveAlerts", "When a drive fills up or its health changes",
              "The notch opens with the message when a drive is filling, full or failing. On by default.")}
            {bool("driveAlertSound", "Play a sound for a drive alert")}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Remote approvals">
            {bool("remoteApprovalAlerts", "When a remote tool waits for your approval",
              "The notch shows what ChatGPT or another remote tool wants to do while it waits for you, and a click opens General > Remote access to approve or deny it. The card goes when the request is answered or expires. On by default.")}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Mute alerts">
            {bool("mute_claude_alerts", "Mute Claude alerts", "No notch cards for the limits of Claude or any of its extra accounts.")}
            {bool("mute_codex_alerts", "Mute Codex alerts", "No notch cards for the limits of Codex or any of its extra profiles.")}
          </SettingsGroup>
          {(
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Try them">
              <SettingRow className="ck-set" label="Try the alerts" description="Send a test notification, or preview the reset, session limit and weekly limit alerts.">
              <div className="ck-line">
                <Button size="sm" variant="secondary" onClick={() => send({ command: "sendTestNotification" })}>Send a test</Button>
                <Button size="sm" variant="secondary" onClick={() => send({ command: "previewResetAlert" })}>Preview reset</Button>
                <Button size="sm" variant="secondary" onClick={() => send({ command: "previewSessionLimitAlert" })}>Preview session limit</Button>
                <Button size="sm" variant="secondary" onClick={() => send({ command: "previewWeeklyLimitAlert" })}>Preview weekly limit</Button>
              </div>
              </SettingRow>
            </SettingsGroup>
          )}
        </SettingsSearchSection>
      )}

      {show("general") && (
        <SettingsSearchSection headingLevel={3} passthroughWhenInactive openLabel={openSectionLabel} className="ck-search-section" id="general" title="General" onOpen={open}>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Startup">
            {bool("launchAtLogin", "Open Pulse at login")}
            <SettingRow className="ck-set" label="Show onboarding again" description="Opens the first-run setup again: the feature switches with their permissions, and the optional licence step. Nothing changes until you finish.">
              <Button size="sm" variant="secondary" onClick={() => window.dispatchEvent(new Event(SHOW_ONBOARDING))}>Show onboarding</Button>
            </SettingRow>
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Diagnostics and Claude accounts">
            {bool("hubDiagnosticsPage", "Show the Diagnostics page in the hub",
              "Pulse's crash and hang reports and warnings, with only times, process names and short codes. On by default.")}
            {bool("claudeAccountScan", "Look for new Claude accounts",
              "Looks for new Claude account folders and offers to bring this computer's chats into them. On by default. Mac only: the Windows card is not built.")}
          </SettingsGroup>
          {has("agentRequests") && (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Agent">
              {bool("agentRequests", "Answer agent requests (uses your Claude subscription)",
                "Lets Pulse's background agent answer `pulse task ask` and voice requests with one bounded run of Claude Code, which uses your Claude subscription quota. Off until you turn it on.")}
              {has("agentActions") && bool("agentActions", "Let the agent act on this computer (uses your Claude subscription)",
                "Lets Pulse's background agent act for `pulse task act` and voice requests, using only the Pulse tools you have switched on under Remote access (files, terminals, screen). Every act waits for your approval in Pulse; you can cancel a run at any time. Off until you turn it on.")}
              {has("ledgerTranscriptSearch") && bool("ledgerTranscriptSearch", "Search ChatGPT and Codex history",
                "Off by default. On: Membrane indexes this computer's Codex sessions and any ChatGPT data export you put in the chosen folder, for `pulse memory history`. Off: Membrane deletes its copy.")}
              {has("ledgerTranscriptShare") && bool("ledgerTranscriptShare", "Share transcripts with paired computers",
                "Lets your other paired computer search the chats you allow in Remote chat read. Off by default.")}
            </SettingsGroup>
          )}
          {has("deviceLinkListen") && (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Devices">
              {bool("deviceLinkListen", "Accept device links (LAN only)",
                "Lets another of your computers on this local network pair and link with this one. Nothing listens while this is off. Each pairing needs a code typed on the other computer and your approval here.")}
              {bool("deviceMeshLocal", "Find my devices on this network",
                "Finds your other computers on the local network, in addition to any relay. On by default.")}
              {bool("sendOverMesh", "Send over the mesh",
                "Sends files to your other computers over the RightKit mesh as well as nearby sharing. On by default.")}
              {bool("agentFileAutoAccept", "Accept agent files from my computers",
                "Files an agent chat on one of your own paired computers sends a chat here (pulse chat send --file) are saved in <Save folder>/agents/<chat> without asking. Never from a guest, server or unclassified device. At most 20 files and 2 GiB a send and 10 GiB an hour; refused if under 5 GiB or 5% of the disk would stay free. Off: the usual Accept prompt (devices set to Allow or Deny skip it, as with any send). On by default.")}
              {has("sendDefaultDevice") && (
                <DefaultPasteRow value={String(s.sendDefaultDevice ?? "")} onChange={(v) => set("sendDefaultDevice", v)} />
              )}
              {bool("devicePhoneChats", "Let paired phones read and message my chats",
                "Lets the Pulse iPhone app read and message your chats over the device link. Does nothing until a phone is paired.")}
              {bool("devicePhoneTypeIntoChat", "Type my phone's messages into the chat",
                "A message from one of your own paired phones is typed into the chat's composer in Claude Desktop and sent (Accessibility on the Mac, UI Automation on Windows), so it arrives as your own message. Never a guest, server or another chat. Pulse never overwrites a draft and never types while you are typing; when it cannot type it sends the message the usual way and says why. Claude Desktop chats in the main profile only. On by default.")}
              {bool("devicePhoneReceive", "Let paired phones pick up items I send them",
                "Send lists your paired phones, and a phone can pick up the items sent to it from this computer. Off, Send lists no phone. On by default.")}
              <NumberRow s={s} set={set} k="devicePhoneReceiveTtlMinutes" label="Minutes an item waits for a phone" min={1} max={1440}
                note="An item the phone has not picked up in this time is deleted from this computer. 60 by default." />
              <TextSettingRow s={s} set={set} k="deviceLinkRelayUrl" label="Device link relay URL"
                note="The relay used when the other computer is not on this network. Empty by default: no relay." />
            </SettingsGroup>
          )}
          <NearbyGroup s={s} set={set} />
          <RemoteAccessGroup s={s} set={set} />
          <SecureBoxGroup s={s} set={set} />
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Keyboard shortcuts">
            {bool("mac_shortcuts", "Mac-style editing",
              "Left Alt+A, C, V, X, Z, S, F, T and W act as Ctrl+the same letter; Left Alt+Shift+Z redoes.")}
            {bool("altRemapSaveFindTabClose", "Alt+S, F, T and W too",
              "Includes Save, Find, new Tab and Close among the Alt shortcuts. Off leaves those four keys to the app, for programs that use Alt+letter for their own menus. Needs Mac-style editing.")}
            {bool("altAsCtrlGuards", "Not in terminals or full-screen apps",
              "On by default. Alt+letter editing keys stand down while a terminal is in front (Ctrl+C is its interrupt there) and while a full-screen program or game covers the screen. Windows only.")}
            {bool("screenshot_shortcuts", "Screenshot shortcuts",
              "Alt+Shift+4 drags a region; Alt+Shift+5 opens the screenshot toolbar.")}
            {!state.conveniences && state.settingsHotkey && (
              <SettingsHotkeyRow h={state.settingsHotkey} set={set} />
            )}
            {bool("screenshot_to_desktop", "Save screenshots to the Desktop",
              "Off keeps them on the clipboard only. The Alt+Shift+5 toolbar changes this too.")}
            {has("screenshotTimer") && Array.isArray((s as Record<string, unknown>).screenshotTimerOptions) ? (
              <SettingRow className="ck-set" label="Screenshot timer"
                description="Waits this long after you pick before the screenshot is taken, so a menu or hover can be set up. No countdown is drawn. Off by default. Windows only; on the Mac the system capture tool has its own timer.">
                <Select label="Screenshot timer"
                  value={String(s.screenshotTimer || "off")}
                  onChange={(v) => set("screenshotTimer", v)}
                  options={((s as Record<string, unknown>).screenshotTimerOptions as string[]).map((o) => ({
                    value: o,
                    label: o === "off" ? "Off" : `${o} seconds`,
                  }))} />
              </SettingRow>
            ) : null}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Installers">
            {bool("convDiskImageInstaller", "Watch Downloads for installers",
              "A new installer in Downloads brings up a card on the notch. Off, Pulse does not look at Downloads.")}
            {bool("installer_auto", "Install signed installers automatically",
              "Watches Downloads and installs signed MSIX and MSI packages, with Undo. Unsigned ones always ask.")}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Updates">
            {state.updates ? (
              <UpdatesRow u={state.updates} send={send} />
            ) : (
              <div className="ck-set ck-sub" role="status">Waiting for the notch to report its version.</div>
            )}
            {bool("autoUpdateCheck", "Automatically check",
              "Looks for a new release at most every six hours. Nothing installs until you choose Update.")}
            {bool("appAutoUpdate", "Update apps automatically",
              "Upgrades your installed apps through winget without asking, only when a verified previous package exists to restore with Undo, and says an app was updated only after reading its new version. Off by default: you press Update on the Apps page.")}
          </SettingsGroup>
          {state.hubProcess || state.membrane ? (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Background processes">
              {state.hubProcess ? (
                <ProcessRow label="Hub" status={state.hubProcess} onRetry={() => send({ command: "hubRetry" })} />
              ) : null}
              {state.membrane ? (
                <ProcessRow label="Agent memory" status={state.membrane} onRetry={() => send({ command: "membraneRetry" })} />
              ) : null}
            </SettingsGroup>
          ) : null}
          {!windows && (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Uninstalling">
              <SettingRow className="ck-set"
                label="Uninstall without password"
                description="Lets Pulse move root-owned apps and their files to the Trash with no administrator password. Approve once in System Settings."
              >
                <Toggle
                  checked={state.helper === "enabled" || state.helper === "requiresApproval"}
                  onChange={(v) => send({ command: v ? "helperEnable" : "helperDisable" })}
                  label="Uninstall without password"
                />
              </SettingRow>
              <div className={`ck-sub ck-foot ck-tone-${state.helper === "enabled" ? "ok" : state.helper === "requiresApproval" ? "warn" : "off"}`} role="status">
                {state.helper === "enabled"
                  ? "On. Root-owned items go to the Trash without a password."
                  : state.helper === "requiresApproval"
                    ? "Waiting for your approval in System Settings."
                    : state.helper === "notFound"
                      ? "This build of Pulse does not include the helper."
                      : "Off. Root-owned items ask for an administrator password in Finder."}
              </div>
              <div className="ck-line ck-foot">
                <Button size="sm" variant="ghost" onClick={() => onNavigate("permissions")}>Manage permissions</Button>
              </div>
              {state.helperError ? <div className="error ck-foot" role="alert">{state.helperError}</div> : null}
            </SettingsGroup>
          )}
          {has("toolsOff") && (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Tools">
              {bool("toolWheelEnabled", "Middle-click wheel", windows
                ? "Press the middle mouse button anywhere to open a wheel of the tools below; it passes through on links in programs that report them. On by default."
                : "Press the middle mouse button anywhere to open a wheel of the tools below; it passes through on links. On by default; needs Accessibility permission.")}
              {has("wheelMiddleClickConflict") && Array.isArray((s as Record<string, unknown>).wheelMiddleClickConflictOptions) ? (
                <SettingRow className="ck-set" label="When another program uses the middle button"
                  description={windows
                    ? "Logitech Options+, X-Mouse Button Control and AutoHotkey can take the middle button too. Pulse asks once, the first time the wheel opens while one of them is running: use Pulse for the wheel, or leave the button to the other program. Windows cannot list other programs' mouse hooks, so only these three are noticed. Ask by default."
                    : "BetterTouchTool, Mac Mouse Fix, SteerMouse, Logitech Options, USB Overdrive and Karabiner-Elements can take the middle button too. Pulse asks once per run, the first time the wheel would open while one of them is running: use Pulse for the wheel, or leave the button to the other program. macOS cannot list other programs' mouse hooks, so only these are noticed. Ask by default."}>
                  <Select label="When another program uses the middle button"
                    value={String(s.wheelMiddleClickConflict || "ask")}
                    onChange={(v) => set("wheelMiddleClickConflict", v)}
                    options={((s as Record<string, unknown>).wheelMiddleClickConflictOptions as string[]).map((o) => ({
                      value: o,
                      label: o === "override" ? "Use Pulse" : o === "standDown" ? "Leave it to the other program" : "Ask",
                    }))} />
                </SettingRow>
              ) : null}
              {TOOLS.filter((t) => t.id !== "clip" || has("launcherClipboard")).map((t) => (
                <SettingRow className="ck-set" key={t.id} label={t.name} description={`${t.note} On by default.`}>
                  <Toggle checked={!toolsOff.includes(t.id)} label={`${t.name} tool`}
                    onChange={(on) => set("toolsOff", on ? toolsOff.filter((id) => id !== t.id) : [...toolsOff, t.id])} />
                </SettingRow>
              ))}
            </SettingsGroup>
          )}
          {has("wheelTools") && (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Wheel">
              {wheel.map((t) => (
                <SettingRow className="ck-set" key={t.id} label={t.name} description={`${t.note} ${t.on ? "On" : "Off"} by default.`}>
                  <Toggle checked={wheelTools.includes(t.id)} label={`${t.name} on the wheel`}
                    onChange={(on) => set("wheelTools", wheel.map((w) => w.id).filter((id) => (id === t.id ? on : wheelTools.includes(id))))} />
                </SettingRow>
              ))}
            </SettingsGroup>
          )}
          {has("claudeChatMirror") && (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Claude accounts">
              {bool("claudeChatMirror", "Keep every account's chat list complete",
                "Copies the signed-in Claude account's chats into the other accounts' lists whenever they change. Off by default: press Sync chats on the Claude card after you switch account; it closes Claude, syncs every account and opens Claude again.")}
              {bool("claudeChatRemember", "Remember which chats were running",
                "Lets a restart or an account switch reopen the chats that were open. On by default.")}
              {bool("claudeAccountSwitcher", "Claude account switcher",
                "Account chips on the Claude card: switch Claude Desktop between your accounts (one data folder each, one open at a time) with the chats merged; Add account quits Claude for a one-time sign-in")}
            </SettingsGroup>
          )}
          {has("hubDriveHealth") && (
            <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Background">
              {bool("hubDriveHealth", "Sample drive health",
                "Reads each drive's health every two minutes for alerts and history. On by default.")}
              {bool("usageTimedPolling", "Poll usage on a timer",
                "Reads Claude and Codex usage every five minutes (every 30 seconds while an agent works). On by default. Off holds the timed read back; Refresh now still reads, and turning it on polls at once if one is due.")}
              {bool("driveSampling", "Sample drive free space",
                "Reads each drive's capacity and free space every 30 seconds for the System card and drive alerts. On by default. Off keeps the last reading.")}
              {bool("networkSampling", "Sample network speed",
                "Reads the adapters' byte counters every two seconds while the System card or Monitor shows them. On by default. Off leaves the network row pending.")}
              {bool("hubMetricsHistory", "Keep CPU, memory and network history",
                "Samples every two seconds and keeps 30 minutes for Monitor's charts. On by default.")}
              {bool("hubFileIndex", "Index file names",
                isWindows
                  ? "Adds the launcher's own walk of the folders chosen below, for file rows Windows Search may miss. Storage and the launcher otherwise answer from Windows Search whatever this says; folders it does not index are not found (Windows Indexing Options sets what it covers). Off by default."
                  : "Builds a file-name index of each disk for fast search in Storage. On by default; applies the next time the hub starts.")}
              {has("hubFileIndex") && fileIndexHealth && (
                <div className="ck-sub ck-foot ck-tone-warn" role="status">Unhealthy: {fileIndexHealth.reasons.join("; ")}</div>
              )}
              {has("hubFileIndex") && <FileIndexLadderRows s={s} set={set} />}
              {bool("hubFileIndexWholeDisk", "Index the whole disk (default: home, Applications and chosen volumes)",
                "Off: the index covers your home folder, /Applications and the extra volumes below. On: every mounted volume. Off by default; applies the next time the hub starts.")}
              <ClipboardListRow s={s} set={set} k="hubFileIndexVolumes" label="Extra volumes to index"
                note="Volumes the index covers as well, one mount path per line (for example /Volumes/Archive). Empty by default." />
              <ClipboardListRow s={s} set={set} k="hubFileIndexExcludes" label="Excluded folders (names or paths; also read from .pulseignore)"
                note="Folders the index skips, one per line: a name skips it wherever it appears; a path such as Library/Caches skips that folder. Default: target, node_modules, .git/objects, DerivedData, .build, build, dist, Library/Caches, Library/Logs, .Trash, .gradle, .cache, __pycache__, venv, .venv. Windows adds AppData\Local\Temp, $Recycle.Bin and System Volume Information." />
              <HubHealthRows s={s} set={set} />
              {bool("chromeSnapshotCount", "Count Chrome's signing copies",
                "Once a day, counts the leftover code-signing copies Chrome keeps, for Cleanup's history line. On by default.")}
              {bool("hubAutoScan", "Scan when Storage or Cleanup opens",
                "Scans by itself when there is no saved result or it is old. On by default. Off shows what was saved until you press Rescan.")}
              {bool("mediaCompression", "Compress images and videos",
                "Adds \"Compress a copy\" to a JPEG, PNG, MP4 or MOV file in Storage. It writes a smaller copy in a Pulse Compressed folder beside the file and never changes the original. Off by default.")}
              {bool("cleanupDeveloperItems", "Developer items in Cleanup",
                "Cleanup also offers developer caches, build output and old release builds (newest two kept).")}
            </SettingsGroup>
          )}
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Launcher">
            {bool("launcherEnabled", "Enable the launcher",
              isWindows ? "Alt+Space opens a search of your apps, running programs and Pulse's pages. Off by default."
                : "Search apps, files and Pulse commands, and calculate. Off by default.")}
            {s.launcherEnabled || (searching && has("launcherEnabled")) ? (
              <>
                {choice("launcherHotkey", "Shortcut")}
                {!isWindows ? (
                  <div className="ck-sub ck-foot">
                    Command space only works after Spotlight's shortcut is turned off in System Settings.
                  </div>
                ) : null}
                {state.launcherStatus ? <div className="error ck-foot" role="alert">{state.launcherStatus}</div> : null}
                {bool("launcherDictionary", "Dictionary", isWindows ? "“define word” looks the word up online (dictionaryapi.dev) when you press Return." : "“define word” looks words up in the Mac's dictionaries.")}
                {isWindows ? bool("launcherNoteEditor", "Edit notes in the launcher", "A note opens in the panel and saves as you type; off opens it in its own program.") : null}
                {bool("launcherShortcuts", "Apple Shortcuts", "Search and run the shortcuts in the Shortcuts app.")}
                {bool("launcherCalculator", "Calculator and unit conversion", "Answers sums and conversions such as “12 km in mi”. On by default.")}
                {isWindows ? bool("launcherFileResults", "File results from Windows Search",
                  "Shows file rows from Windows Search as you type. It finds only files in folders Windows indexes. On by default; off shows file rows only from the launcher's own folder walk.") : null}
                {choice("launcherAskTarget", "Ask row", false,
                  "Where the launcher's Ask row sends what you typed: answered in the launcher by the resident engine, or opened in ChatGPT. In the launcher by default.")}
                {bool("launcherPulseCommands", "Pulse commands",
                  `Open Storage, Cleanup, Monitor, Apps and Settings${has("launcherClipboard") ? ", and clear clipboard history," : ""} from the launcher. On by default.`)}
                <LauncherSearchRows s={s} set={set} />
                {has("launcherSnippetExpansion") ? bool("launcherSnippetExpansion", "Expand snippet keywords as you type",
                  "Watches what you type to spot a snippet's keyword and replaces it. Nothing typed is stored or sent. Needs Accessibility. A keyword must start with a symbol (such as ;sig or /addr) and be 2 to 32 characters. It expands only in a text field, never in password fields, in Pulse's own windows, or while a password manager's item is on the clipboard. Snippets that use {argument} are skipped. Off until you turn it on.") : null}
                {bool("launcherCurrency", "Currency and crypto rates",
                  "Fetches exchange rates once a day from open.er-api.com and CoinGecko. Off until you turn it on.")}
                {has("launcherConfig") ? (
                  <LauncherLists
                    raw={s.launcherConfig}
                    shortcutNames={Array.isArray(s.launcherShortcutNames) ? s.launcherShortcutNames.map(String) : []}
                    onSave={(next: LauncherConfig) => set("launcherConfig", JSON.stringify(next))}
                  />
                ) : null}
              </>
            ) : null}
            {bool("launcherClipboard", "Clipboard history",
              `Keeps what you copy on this ${isWindows ? "PC" : "Mac"}: text, links, colours, code, images and files, searchable with “clip” and in the clipboard strip. It works with the launcher on or off. Off until you turn it on. Turning it off keeps the history and reads nothing from the clipboard; Clear clipboard history deletes it.`)}
            {s.launcherClipboard || (searching && has("launcherClipboard")) ? (
              <div className="ck-subgroup">
                {!searching ? <div className="ck-subhead">Clipboard</div> : null}
                {bool("clipboardStripEnabled", "Show the clipboard strip",
                  "Allows the hot key, the Tools cell item and the wheel item to open the strip of cards. It works with the launcher on or off. Off: history is still kept, the hot key is released, and the items open the launcher's “clip” when the launcher is on. On by default (only matters once Clipboard history is on).")}
                {has("clipboardHotkey") ? (
                  <ClipboardHotkeyRow value={String(s.clipboardHotkey ?? "")} status={state.clipboardStatus} set={set} />
                ) : null}
                {choice("clipboardCardSize", "Card size",
                  true, "Small, Medium or Large clipboard cards. The size is multiplied by the notch size scale, clamped to 0.8 to 1.25. Medium by default.")}
                {choice("clipboardStripPlacement", "Strip position",
                  true, "Where the strip appears. Notch: out of the notch, on its edge. Bottom: along the bottom of the display under the pointer, with no neck. Notch by default.")}
                {bool("clipboardShowNumbers", "Show numbers on cards", "Shows the 1 to 9 quick-paste number on clipboard cards. On by default.")}
                {choice("clipboardPasteMode", "When I choose a clip",
                  true, `Paste the chosen clipboard item into the app you were in, or only copy it.${isWindows ? "" : " Paste needs Accessibility."} Paste by default.`)}
                {bool("clipboardPlainDefault", "Paste as plain text by default",
                  "In the clipboard strip, Enter pastes plain text and Shift+Enter pastes as copied (swaps the two). Off by default.")}
                {bool("clipboardWriteBackExcluded", "Keep pasted clips out of Windows history",
                  "Windows only. When Pulse pastes a clipboard item it marks it so Windows' own Win+V history does not record it again. On by default.")}
                {bool("clipboardCaptureImages", "Keep images", "Stores copied images and screenshots in the clipboard history. Off: image copies are skipped. On by default.")}
                {bool("clipboardCaptureFiles", "Keep copied files", "Stores copied file lists (paths only) in the clipboard history. Off: file copies are skipped. On by default.")}
                {bool("clipboardCaptureRich", "Keep formatting", "Stores HTML and RTF with the text in the clipboard history. Off: rich copies are stored as plain text. On by default.")}
                <ClipboardNumberRow s={s} set={set} k="clipboardMaxItems" label="Keep at most (items)" min={50} max={5000}
                  note="Cap on clipboard items in the main lane (50 to 5000). Pinned and board items stay. 500 by default." />
                <ClipboardNumberRow s={s} set={set} k="clipboardMaxMB" label="Keep at most (MB)" min={20} max={4096}
                  note="Cap on the clipboard history's total size in MB (20 to 4096). 200 by default." />
                <ClipboardNumberRow s={s} set={set} k="clipboardMaxAgeDays" label="Keep for (days)" min={0} max={365}
                  note="Days since last use before an unpinned clipboard item goes (1 to 365; 0 is forever). 30 by default." />
                <ClipboardNumberRow s={s} set={set} k="clipboardScreenshotHours" label="Keep screenshots for (hours)" min={0} max={720}
                  note="Hours before a clipboard screenshot goes (1 to 720; 0 is until a size cap). 24 by default." />
                {bool("clipboardSkipAutoGenerated", "Skip copies made by programs",
                  "Ignores clipboard copies that apps mark as made by a program (AutoGeneratedType). Mac only. On by default.")}
                {bool("clipboardSkipRemote", "Skip copies from other devices",
                  "Ignores Universal Clipboard copies from your iPhone or iPad. Mac only. On by default.")}
                {bool("clipboardSecretFilter", "Skip keys and tokens",
                  "Ignores a clipboard copy that is only a private key, access key, token or password-like string from a known pattern. On by default.")}
                {bool("clipboardEncryption", "Encrypt clipboard history",
                  "Keeps the clipboard history sealed on this computer with a key the system keeps for this user (Keychain on Mac; DPAPI on Windows). On by default.")}
                <ClipboardListRow s={s} set={set} k="clipboardExcludedApps" label="Apps to ignore"
                  note="Clipboard copies from these apps are never kept. One app id per line. Password managers by default." />
                <ClipboardListRow s={s} set={set} k="clipboardIgnoredTypes" label="Types to ignore"
                  note="Extra pasteboard types that make the clipboard history skip a copy. One per line. Mac only. Empty by default." />
                <ClipboardListRow s={s} set={set} k="clipboardShiftPasteApps" label="Terminals"
                  note="Windows only. Terminals where the clipboard strip pastes with Ctrl+Shift+V: app names, one per line. Windows Terminal, WezTerm, Alacritty and kitty by default." />
                {bool("clipboardRememberBoard", "Remember the last board", "Opens the clipboard strip on the board you left it on. Off by default.")}
                {bool("clipboardHideFromCapture", "Hide the clipboard from screenshots", "The clipboard strip and its preview stay out of screenshots and screen shares. On by default. Off lets them appear in a capture.")}
                {bool("clipboardWheelRing", "Clipboard ring on the wheel",
                  "Resting on Clipboard on the middle-click wheel opens a ring of your last 8 items, pinned first; click one to paste it. Mac only. On by default.")}
                {bool("clipboardRestoreAfterPaste", "Put the previous clipboard back after pasting", "After a paste, the clipboard that was there before comes back 1.5 s later, if nothing else was copied meanwhile. Off by default.")}
                {bool("clipboardCopyBurst", "Copy burst: paste a run of copies in order",
                  "Two or more copies under 60 s apart show as dots on the Send cell; plain paste puts them in copied order.")}
                <ClipboardClearRow count={clipboardNumber(state, "clipboardCount")} bytes={clipboardNumber(state, "clipboardBytes")} send={send} />
              </div>
            ) : null}
          </SettingsGroup>
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Keyboard">
            {state.conveniences?.settingsHotkey && (
              <SettingsHotkeyRow h={state.conveniences.settingsHotkey} set={set} />
            )}
            {bool("convFnCommand", "Fn works as Command", "Fn+C/V/X/A/Z/S/F/T/W; Fn+arrows move by word")}
            {Boolean(s.convFnCommand) && state.conveniences && (
              <div className={`ck-sub ck-foot ck-tone-${state.conveniences.fnStatus === "running" ? "ok" : state.conveniences.fnStatus === "needsAccessibility" ? "warn" : "bad"}`} role="status">
                {state.conveniences.fnStatus === "running"
                  ? "Running."
                  : state.conveniences.fnStatus === "needsAccessibility"
                    ? "Needs Accessibility permission."
                    : `Failed: ${state.conveniences.fnDetail || "unknown reason"}.`}
              </div>
            )}
            {Boolean(s.convFnCommand) && (
              <div className="ck-line ck-foot">
                <Button size="sm" variant="ghost" onClick={() => onNavigate("permissions")}>Manage permissions</Button>
              </div>
            )}
          </SettingsGroup>
          {!state.conveniences && has("convAutoQuit") && <WindowsAutoQuit state={state} s={s} set={set} searching={searching} />}
          <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Readings">
            {bool("gpuTemperature", "GPU temperature",
              "Shows the NVIDIA graphics card's temperature on the System card. It is read only while that card is open. On by default.")}
            {bool("monitorProcessGpu", "Per-process GPU",
              "Windows reads per-process GPU engine counters for Monitor rows; the Mac has none.")}
            {bool("asksProviderOnLook", "Ask the provider every time you look",
              "Spends a request each time. Useful to check against a provider's own page.")}
            <SettingRow className="ck-set" label="Refresh"><Button size="sm" variant="secondary" onClick={() => send({ command: "refresh" })}>Refresh now</Button></SettingRow>
          </SettingsGroup>
          {state.conveniences && (
            <ConveniencesGroup c={state.conveniences} s={s} set={set} onNavigate={onNavigate} searching={searching} />
          )}
          {state.conveniences?.windowManagement && (
            <WindowManagementGroup w={state.conveniences.windowManagement} accessibility={state.conveniences.accessibility}
              s={s} set={set} />
          )}
          {!state.conveniences && state.windowManagement && (
            <WindowManagementGroup w={state.windowManagement} accessibility s={s} set={set} />
          )}
          <div className="ck-sub ck-about">
            {windows ? `Pulse notch ${state.version}` : `Pulse notch ${state.version} · built on Codenotch (MIT)`}
          </div>
        </SettingsSearchSection>
      )}
    </>
  );
}

const AUTO_QUIT_NOTE = "Quits the apps below when their last window closes. Only apps you add; windows on other Spaces or minimized keep an app open.";

/** The Auto Quit list: the apps chosen, each with Remove, and a picker for the running apps. Shared by both notches. */
function AutoQuitRows({ enabled, searching, apps, running, set }: {
  enabled: boolean;
  searching: boolean;
  apps: AppRef[];
  running: AppRef[];
  set: (key: string, value: unknown) => void;
}) {
  const [pick, setPick] = useState("");
  const listed = apps.map((a) => a.id);
  const addable = running.filter((a) => !listed.includes(a.id));
  if (!enabled && !searching) return null;
  return (
    <>
      {apps.map((a) => (
        <SettingRow className="ck-set" key={a.id} label={a.name} description={enabled ? a.id : `${a.id}. Does nothing until Auto Quit is on.`}>
          <Button size="sm" variant="ghost"
            onClick={() => set("convAutoQuitApps", listed.filter((id) => id !== a.id))}>Remove</Button>
        </SettingRow>
      ))}
      <SettingRow className="ck-set" label="Add a running app">
        <span className="ck-line ck-inline">
          <Select label="Running app to add" value={pick} onChange={setPick}
            options={[{ value: "", label: "Choose…" }, ...addable.map((a) => ({ value: a.id, label: a.name }))]} />
          <Button size="sm" variant="secondary" disabled={!pick}
            onClick={() => { set("convAutoQuitApps", [...listed, pick]); setPick(""); }}>Add</Button>
        </span>
      </SettingRow>
    </>
  );
}

/** The Windows notch's Auto Quit group: it has no `conveniences`, so the lists come from `autoQuit`, or from the saved ids alone. */
function WindowsAutoQuit({ state, s, set, searching }: {
  state: NotchState;
  s: NotchState["settings"];
  set: (key: string, value: unknown) => void;
  searching: boolean;
}) {
  const raw: unknown = s.convAutoQuitApps;
  const saved = Array.isArray(raw) ? raw.filter((id): id is string => typeof id === "string") : [];
  const apps = state.autoQuit?.autoQuitApps ?? saved.map((id) => ({ id, name: id }));
  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Auto Quit">
      <SettingRow className="ck-set" label="Auto Quit" description="Closes the apps below when their last window closes. Only apps you add; an app with a minimized window, or one that keeps running in the tray, is left open.">
        <Toggle checked={Boolean(s.convAutoQuit)} onChange={(v) => set("convAutoQuit", v)} label="Auto Quit" />
      </SettingRow>
      <AutoQuitRows enabled={Boolean(s.convAutoQuit)} searching={searching} apps={apps} running={state.autoQuit?.runningApps ?? []} set={set} />
    </SettingsGroup>
  );
}

function ConveniencesGroup({ c, s, set, onNavigate, searching }: {
  c: Conveniences;
  s: NotchState["settings"];
  set: (key: string, value: unknown) => void;
  onNavigate: (section: string) => void;
  searching: boolean;
}) {
  const toggle = (key: string, text: string, note: string) => (
    <SettingRow className="ck-set" label={text} description={note}>
      <Toggle checked={Boolean(s[key])} onChange={(v) => set(key, v)} label={text} />
    </SettingRow>
  );
  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Conveniences">
      <SettingRow className="ck-set"
        label="Accessibility"
        description={c.accessibility
          ? "Allowed."
          : "Needs Accessibility permission. Until it is granted these stay off."}
      >
        <Button size="sm" variant="secondary" onClick={() => onNavigate("permissions")}>Permissions</Button>
      </SettingRow>
      {toggle("convFinderCutPaste", "Cut and paste in Finder",
        "⌘X (or Fn X) marks the selected items; ⌘V in a Finder window moves them there. Finder asks before replacing anything.")}
      {Boolean(s.convFinderCutPaste) && c.accessibility && !c.inputMonitoring && (
        <div className="ck-sub ck-foot ck-tone-warn" role="status">
          Key shortcuts may also need Input Monitoring (System Settings, Privacy and Security).
        </div>
      )}
      {c.cutPasteResults.length > 0 && (
        <div className="ck-sub ck-foot" role="status">
          Last paste: {c.cutPasteResults.map((r) => `${r.name} — ${r.ok ? r.detail : `failed: ${r.detail}`}`).join("; ")}
        </div>
      )}
      {toggle("finderPathField", "Path field in Finder",
        "Shows the front Finder window's folder in a small field above it. Type or paste a path and press Return to go there. Needs Accessibility and Finder Automation (macOS asks the first time).")}
      {toggle("convWindowMaximizer", "Green button maximizes",
        "Fills the screen without a full-screen Space. Option-click keeps the usual behaviour.")}
      {toggle("convDockClickMinimize", "Dock click minimizes",
        "Clicking the Dock icon of the frontmost app minimizes its windows.")}
      {toggle("convDiskImageInstaller", "Install apps from disk images",
        "When a disk image holding an app mounts, the notch installs it to Applications and ejects the image, or asks first. Needs no permission.")}
      {(Boolean(s.convDiskImageInstaller) || searching) && toggle("convDiskImageAuto", "Install apps from disk images automatically",
        "Only signed, notarized apps that aren't installed yet; others ask in the notch.")}
      {(Boolean(s.convDiskImageInstaller) || searching) && toggle("convDiskImageAutoUpdate", "Also update apps that are already installed",
        "Replaces an older installed version; Undo restores it from the Trash.")}
      {(Boolean(s.convDiskImageInstaller) || searching) && toggle("convDiskImageTrashDownload", "Move the downloaded disk image to the Trash",
        "After a successful install, the .dmg you opened goes to the Trash. Off by default.")}
      {"finderMenuCut" in s && toggle("finderMenuCut", "Finder menu: Cut",
        "Cut in Finder's right-click menu, for selected items. On by default. Shown while Pulse runs.")}
      {"finderMenuCopyPath" in s && toggle("finderMenuCopyPath", "Finder menu: Copy Path",
        "Copy Path in Finder's right-click menu. On by default. Shown while Pulse runs.")}
      {"finderMenuTerminal" in s && toggle("finderMenuTerminal", "Finder menu: Open in Terminal",
        "Open in Terminal in Finder's right-click menu. On by default. Shown while Pulse runs.")}
      {toggle("convAutoQuit", "Auto Quit", AUTO_QUIT_NOTE)}
      <AutoQuitRows enabled={Boolean(s.convAutoQuit)} searching={searching} apps={c.autoQuitApps} running={c.runningApps} set={set} />
    </SettingsGroup>
  );
}

const asList = (v: unknown): string[] => {
  if (Array.isArray(v)) return v.map(String);
  if (typeof v === "string" && v.trim() !== "") {
    try {
      const parsed: unknown = JSON.parse(v);
      return Array.isArray(parsed) ? parsed.map(String) : [];
    } catch {
      return [];
    }
  }
  return [];
};

/** A number the notch publishes about the clipboard (item count, bytes), at the top level of the state; null when unpublished. */
function clipboardNumber(state: NotchState, key: "clipboardCount" | "clipboardBytes"): number | null {
  const top = state[key];
  return isFiniteNumber(top) ? top : null;
}

/** "Ctrl+Shift+V" from "ctrl+shift+v" (the shared hot key grammar, spec 7.12). The notch's own names for the modifiers stay as they were. */
function clipboardHotkeyText(raw: string): string {
  if (!raw) return "None";
  const labels = { ctrl: isWindows ? "Ctrl" : "Control", alt: isWindows ? "Alt" : "Option", shift: "Shift", cmd: "Command" };
  return formatChord(raw, getPlatform(), { labels }).join(isWindows ? "+" : " ");
}

/** The clipboard strip's shortcut: the same recorder as the Settings shortcut row; its value uses the notch's grammar (alt for Option, no Windows key). */
function ClipboardHotkeyRow({ value, status, set }: {
  value: string;
  status?: string | null;
  set: (key: string, value: unknown) => void;
}) {
  const [recording, setRecording] = useState(false);
  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setRecording(false);
        return;
      }
      const shortcut = windowShortcutFrom(e);
      if (!shortcut) return;
      // The recorder names Option "opt"; the clipboard grammar says "alt". Windows has no Command or Windows key here.
      const parts = shortcut.split("+").map((p) => (p === "opt" ? "alt" : p));
      if (isWindows && parts.includes("cmd")) return;
      set("clipboardHotkey", parts.join("+"));
      setRecording(false);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, set]);
  return (
    <>
      <SettingRow className="ck-set" label="Clipboard strip shortcut"
        description={`The global key that opens the clipboard strip. Needs at least one of ${isWindows ? "Ctrl, Alt or Shift" : "Control, Option, Shift or Command"}. Escape cancels. ${isWindows ? "Alt+Shift+V" : "Command Shift V"} by default.`}>
        <span className="ck-line ck-inline">
          <span className="ck-sub ck-num ck-keys">{recording ? "Press a shortcut…" : clipboardHotkeyText(value)}</span>
          <Button size="sm" variant="ghost" onClick={() => setRecording(!recording)}>{recording ? "Cancel" : "Set"}</Button>
        </span>
      </SettingRow>
      {status ? <div className="error ck-foot" role="alert">{status}</div> : null}
    </>
  );
}

/** A whole-number setting (the clipboard's caps), saved when the field loses focus or Enter is pressed; out-of-range values snap back. */
function ClipboardNumberRow({ s, set, k, label, note, min, max }: {
  s: NotchState["settings"];
  set: (key: string, value: unknown) => void;
  k: string;
  label: string;
  note: string;
  min: number;
  max: number;
}) {
  const published = s[k];
  const [draft, setDraft] = useState<string | null>(null);
  if (!isFiniteNumber(published)) return null;
  const commit = () => {
    const n = Math.round(Number(draft));
    setDraft(null);
    if (draft === null || draft.trim() === "" || !Number.isFinite(n)) return;
    const next = Math.min(max, Math.max(min, n));
    if (next !== published) set(k, next);
  };
  return (
    <SettingRow className="ck-set" label={label} description={note}>
      <Input type="number" className="ck-input" aria-label={label} min={min} max={max} step={1}
        value={draft ?? String(published)}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
    </SettingRow>
  );
}

/** One line of text, saved when the box loses focus or Enter is pressed. Absent from the notch's state, nothing shows. */
function TextSettingRow({ s, set, k, label, note }: {
  s: NotchState["settings"];
  set: (key: string, value: unknown) => void;
  k: string;
  label: string;
  note: string;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const published = s[k];
  if (typeof published !== "string") return null;
  const commit = () => {
    const next = (draft ?? "").trim();
    setDraft(null);
    if (draft === null || next === published) return;
    set(k, next);
  };
  return (
    <SettingRow className="ck-set" label={label} description={note}>
      <Input type="text" className="ck-input" aria-label={label} spellCheck={false}
        value={draft ?? published}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
    </SettingRow>
  );
}

/** A list of strings, one per line, saved when the box loses focus. Sent in the shape the notch published (array or JSON text). */
function ClipboardListRow({ s, set, k, label, note }: {
  s: NotchState["settings"];
  set: (key: string, value: unknown) => void;
  k: string;
  label: string;
  note: string;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  if (!(k in s)) return null;
  const published = s[k] as unknown;
  const list = asList(published);
  const commit = () => {
    const next = (draft ?? "").split("\n").map((l) => l.trim()).filter((l) => l !== "");
    setDraft(null);
    if (draft === null || next.join("\n") === list.join("\n")) return;
    set(k, typeof published === "string" ? JSON.stringify(next) : next);
  };
  return (
    <SettingRow className="ck-set" label={label} description={note}>
      <textarea className="ck-input" aria-label={label} rows={Math.min(8, Math.max(3, list.length + 1))} spellCheck={false}
        value={draft ?? list.join("\n")}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit} />
    </SettingRow>
  );
}

/** "Clear clipboard history": a confirm, then the notch's `clipboardClear`. Counts only; the hub never reads the items. */
function ClipboardClearRow({ count, bytes, send }: {
  count: number | null;
  bytes: number | null;
  send: Send;
}) {
  const [confirm, setConfirm] = useState(false);
  const items = count === null ? "all items" : `${count} item${count === 1 ? "" : "s"}`;
  const size = bytes === null ? "" : `, ${bytes < 1024 * 1024 && bytes > 0 ? "under 1" : (bytes / (1024 * 1024)).toFixed(bytes < 10 * 1024 * 1024 ? 1 : 0)} MB`;
  return (
    <>
      <SettingRow className="ck-set" label="Clear clipboard history"
        description={`${count === null ? "Deletes every clipboard item" : `${items}${size}.`} Deletes every item, including pinned and board items; boards stay. Nothing is wiped securely.`}>
        <Button size="sm" variant="secondary" disabled={count === 0} onClick={() => setConfirm(true)}>Clear…</Button>
      </SettingRow>
      {confirm && (
        <ConfirmDialog
          title="Clear clipboard history?"
          description={`Delete ${items}? This can't be undone. Pinned and board items are deleted too; the boards stay.`}
          confirmLabel="Delete"
          onConfirm={() => { setConfirm(false); send({ command: "clipboardClear" }); }}
          onCancel={() => setConfirm(false)}
        />
      )}
    </>
  );
}

/** The Keyboard row that sets the global shortcut opening Settings. Off until a key is recorded. */
function SettingsHotkeyRow({ h, set }: {
  h: { shortcut: string; display: string; error: string };
  set: (key: string, value: unknown) => void;
}) {
  const [recording, setRecording] = useState(false);
  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setRecording(false);
        return;
      }
      const shortcut = windowShortcutFrom(e);
      if (!shortcut) return;
      set("settingsHotkey", shortcut);
      setRecording(false);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, set]);
  return (
    <SettingRow className="ck-set" label="Open Settings with a shortcut"
      description={isWindows
        ? "Off until you set one. Needs at least one of Ctrl, Alt, Shift or the Windows key. Escape cancels."
        : "Off until you set one. Needs at least one of Control, Option, Shift or Command. Escape cancels."}>
      <span className="ck-line ck-inline">
        <span className="ck-sub ck-num ck-keys">{recording ? "Press a shortcut…" : h.display || "None"}</span>
        <Button size="sm" variant="ghost" onClick={() => setRecording(!recording)}>
          {recording ? "Cancel" : "Set"}
        </Button>
        {h.shortcut !== "" && !recording && (
          <Button size="sm" variant="ghost" onClick={() => set("settingsHotkey", "")}>Clear</Button>
        )}
      </span>
      {h.error && <div className="error" role="alert">{h.error}</div>}
    </SettingRow>
  );
}

function WindowManagementGroup({ w, accessibility, s, set }: {
  w: WindowManagementState;
  accessibility: boolean;
  s: NotchState["settings"];
  set: (key: string, value: unknown) => void;
}) {
  const [recording, setRecording] = useState<string | null>(null);
  // While an action waits for its shortcut, the next key press with a modifier is taken.
  useEffect(() => {
    if (!recording) return;
    const onKey = (e: KeyboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      if (e.code === "Escape") {
        setRecording(null);
        return;
      }
      const shortcut = windowShortcutFrom(e);
      if (!shortcut) return;
      set(`windowHotkey.${recording}`, shortcut);
      setRecording(null);
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [recording, set]);

  const searching = useSettingsSearch().active;
  const wmOn = Boolean(s.windowManagementEnabled);
  const wmShown = wmOn || searching;
  const parentNote = wmOn ? undefined : "Does nothing until \"Move windows with shortcuts\" is on.";
  const groups = new Map<string, { title: string; items: WindowShortcutState[] }>();
  for (const action of w.actions) {
    const group = groups.get(action.group) ?? { title: action.groupTitle, items: [] };
    group.items.push(action);
    groups.set(action.group, group);
  }

  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Window management">
      <SettingRow className="ck-set"
        label="Move windows with shortcuts"
        description={isWindows
          ? "Halves, quarters, thirds, maximize, larger and smaller, displays and nudges. Off by default; each shortcut can be turned off or changed. On a keyboard with AltGr, a Ctrl+Alt shortcut takes the place of the character AltGr types with that key."
          : "Halves, quarters, thirds, maximize, larger and smaller, displays and nudges. Off by default; each shortcut can be turned off or changed. Needs Accessibility permission."}
      >
        <Toggle checked={Boolean(s.windowManagementEnabled)} onChange={(v) => set("windowManagementEnabled", v)}
          label="Move windows with shortcuts" />
      </SettingRow>
      {Boolean(s.windowManagementEnabled) && !accessibility && (
        <div className="ck-sub ck-foot ck-tone-warn" role="status">Shortcuts do nothing until Pulse has Accessibility permission.</div>
      )}
      {wmShown && [...groups.entries()].map(([id, group]) => (
        <div key={id} className="ck-subgroup">
          <div className="ck-subhead">{group.title}</div>
          {group.items.map((action) => (
            <SettingRow className="ck-set" key={action.id} label={action.title} description={parentNote}>
              <span className="ck-line ck-inline">
                <span className="ck-sub ck-num ck-keys">
                  {recording === action.id ? "Press a shortcut…" : action.display || "None"}
                </span>
                <Toggle checked={action.shortcut !== ""} label={`${action.title} shortcut`}
                  onChange={(on) => {
                    if (!on) set(`windowHotkey.${action.id}`, "");
                    else if (action.default) set(`windowHotkey.${action.id}`, action.default);
                    else setRecording(action.id);
                  }} />
                <Button size="sm" variant="ghost"
                  onClick={() => setRecording(recording === action.id ? null : action.id)}>
                  {recording === action.id ? "Cancel" : "Change"}
                </Button>
              </span>
              {action.error && <div className="error" role="alert">{action.error}</div>}
            </SettingRow>
          ))}
        </div>
      ))}
      {wmShown && (
        <SettingRow className="ck-set" label="Shortcuts" description={isWindows
          ? "Recording needs at least one of Ctrl, Alt, Shift or the Windows key. Escape cancels."
          : "Recording needs at least one of Control, Option, Shift or Command. Escape cancels."}>
          <Button size="sm" variant="secondary" onClick={() => set("windowHotkeys", isWindows ? [] : {})}>Reset to defaults</Button>
        </SettingRow>
      )}
    </SettingsGroup>
  );
}

/** "2 h 10 min" until `secs` (epoch seconds), or null once it has passed. */
function resetsIn(secs: number): string | null {
  const left = secs - Date.now() / 1000;
  if (left <= 0) return null;
  const minutes = Math.max(1, Math.round(left / 60));
  const days = Math.floor(minutes / 1440);
  const hours = Math.floor((minutes % 1440) / 60);
  if (days > 0) return `${days} d ${hours} h`;
  if (hours > 0) return `${hours} h ${minutes % 60} min`;
  return `${minutes} min`;
}

/** Session and weekly (all models) windows; the first two the notch saved if neither is found. */
function claudeWindows(windows: ClaudeWindow[]): { title: string; window: ClaudeWindow }[] {
  const HOUR = 3600;
  const session = windows.find((w) => w.seconds != null && Math.abs(w.seconds - 5 * HOUR) < 60);
  const weeklies = windows.filter((w) => w.seconds != null && w.seconds >= 6 * 24 * HOUR && w.seconds <= 8 * 24 * HOUR);
  const weekly = weeklies.find((w) => w.label === "All models") ?? weeklies[0];
  const rows: { title: string; window: ClaudeWindow }[] = [];
  if (session) rows.push({ title: "Session", window: session });
  if (weekly) rows.push({ title: "Weekly", window: weekly });
  return rows.length > 0 ? rows : windows.slice(0, 2).map((w) => ({ title: w.label, window: w }));
}

function asOf(secs: number): string {
  const d = new Date(secs * 1000);
  const time = d.toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
  return d.toDateString() === new Date().toDateString()
    ? time
    : `${d.toLocaleDateString(undefined, { month: "short", day: "numeric" })}, ${time}`;
}

/** "Updated 12 min ago" once a reading is more than 10 minutes old; null while it is fresh or unreadable. */
function readingAge(lastReadAt: string, nowMs: number): string | null {
  const at = Date.parse(lastReadAt);
  if (Number.isNaN(at) || nowMs - at <= 10 * 60_000) return null;
  const minutes = Math.floor((nowMs - at) / 60_000);
  if (minutes < 60) return `Updated ${minutes} min ago`;
  if (minutes < 1440) return `Updated ${Math.floor(minutes / 60)} h ago`;
  return `Updated ${Math.floor(minutes / 1440)} d ago`;
}

/** The freshness line under an account, or null. A sign-in renewal already says it, so the row keeps its own message. */
function accountFreshness(a: Account, nowMs: number): string | null {
  if (a.needsRenewal) return null;
  if (a.lastError) {
    const at = a.lastReadAt ? Date.parse(a.lastReadAt) : NaN;
    return Number.isNaN(at) ? `Not updated: ${a.lastError}` : `Not updated since ${asOf(at / 1000)}: ${a.lastError}`;
  }
  return a.lastReadAt ? readingAge(a.lastReadAt, nowMs) : null;
}

/** The two columns every read account shows: Session and Weekly, "—" where a window is not known. */
function claudeColumns(windows: ClaudeWindow[]): { title: string; window?: ClaudeWindow }[] {
  const found = claudeWindows(windows);
  if (found.length > 0 && !found.every((r) => r.title === "Session" || r.title === "Weekly")) return found;
  return ["Session", "Weekly"].map((title) => ({ title, window: found.find((r) => r.title === title)?.window }));
}

function ClaudeAccountRow({ account, send, unseen = false }: { account: ClaudeAccount; send: Send; unseen?: boolean }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(account.name);
  const cancelled = useRef(false);
  useEffect(() => { if (!editing) setDraft(account.name); }, [account.name, editing]);
  const commit = () => {
    if (cancelled.current) { cancelled.current = false; return; }
    const next = draft.trim();
    if (next !== account.name) send({ command: "renameClaudeAccount", id: account.id, name: next });
    setEditing(false);
  };
  const cached = !account.active && account.capturedAt != null;
  return (
    <div className="ck-claude-account" data-account={account.id}>
      <div className="ck-account-name">
        {editing ? (
          <Input
            className="ck-input ck-claude-edit"
            autoFocus
            value={draft}
            placeholder="Name this account"
            onChange={(e) => setDraft(e.target.value)}
            onFocus={(e) => e.target.select()}
            onBlur={commit}
            onKeyDown={(e) => {
              if (e.key === "Enter") (e.target as HTMLInputElement).blur();
              if (e.key === "Escape") { cancelled.current = true; setDraft(account.name); setEditing(false); }
            }}
            aria-label={`Name for ${account.name}`}
            title="Enter saves, Escape cancels, empty restores the default name"
          />
        ) : (
          <Button variant="ghost" className="ck-claude-name" onClick={() => setEditing(true)}
            aria-label={`Rename ${account.name}`} title="Rename this account">
            <span className="ck-claude-name-text">{account.name}</span>
            <Pencil size={12} strokeWidth={1.75} aria-hidden="true" />
          </Button>
        )}
        {account.active && <span className="ck-status ck-status-granted">Active</span>}
        {account.plan && !unseen && <span className="ck-sub ck-claude-plan">{account.plan}</span>}
        {cached && <span className="ck-sub ck-claude-asof">as of {asOf(account.capturedAt as number)}</span>}
        {!account.active && account.onDisk === false && (
          <IconButton className="ck-forget" label={`Forget ${account.name}`}
            onClick={() => send({ command: "forgetClaudeAccount", id: account.id })}
            title="Its folder is gone. Removes this account from the list; it returns if you sign in to it again.">
            <X size={14} strokeWidth={1.75} aria-hidden="true" />
          </IconButton>
        )}
      </div>
      {unseen ? (
        <span className="ck-sub ck-claude-none">no reading</span>
      ) : (
        <div className="ck-claude-meters">
          {claudeColumns(account.windows).map(({ title, window: w }) => {
            const left = w ? resetsIn(w.resetsAt) : null;
            const pct = w ? Math.round(Math.min(Math.max(w.usedFraction, 0), 1) * 100) : 0;
            return (
              <div className="ck-claude-win" key={title} data-state={left ? "live" : w ? "reset" : "none"}>
                <div className="ck-claude-head">
                  <span className="ck-sub">{title}</span>
                  <span className="ck-claude-value">{left ? `${pct}%` : "\u2014"}</span>
                  <span className="ck-sub ck-claude-eta">{left ? `resets in ${left}` : w ? "reset" : ""}</span>
                </div>
                {left ? <Bar fraction={w!.usedFraction} color={tone(w!.usedFraction)} height={4} /> : <span className="ck-claude-bar-off" />}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

/** Accounts read at least once (and the signed-in one) as rows; the rest fold into one disclosure line. */
function ClaudeAccountList({ accounts, send }: { accounts: ClaudeAccount[]; send: Send }) {
  const [open, setOpen] = useState(false);
  const seen = accounts.filter((c) => c.active || c.capturedAt != null);
  const unseen = accounts.filter((c) => !seen.includes(c));
  return (
    <div className="ck-claude-list" aria-label="Claude accounts">
      {seen.map((c) => <ClaudeAccountRow key={c.id} account={c} send={send} />)}
      {unseen.length > 0 && (
        <Button variant="ghost" className="ck-claude-unseen" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? <ChevronUp size={14} strokeWidth={1.75} aria-hidden="true" /> : <ChevronDown size={14} strokeWidth={1.75} aria-hidden="true" />}
          {unseen.length} {unseen.length === 1 ? "account" : "accounts"} never seen signed in
        </Button>
      )}
      {open && unseen.map((c) => <ClaudeAccountRow key={c.id} account={c} send={send} unseen />)}
      <div style={{ padding: "8px 14px", fontSize: 12, color: "var(--rk-ink-3)", borderTop: "1px solid var(--rk-hairline)" }}>
        A name set here is shared with your paired computer and phone; the newest rename wins.
      </div>
    </div>
  );
}

const GAUGE_ICONS: Record<string, { Icon: typeof Cpu; text: string }> = {
  cpu: { Icon: Cpu, text: "Processor chip" },
  memory: { Icon: MemoryStick, text: "Memory module" },
  disk: { Icon: HardDrive, text: "Drive" },
  send: { Icon: SendIcon, text: "Paper plane" },
  tools: { Icon: Wrench, text: "Wrench" },
};
const GAUGE_MARKS: Record<string, string> = { claude: "Claude mark", openai: "OpenAI mark" };

/**
 * Every notch cell, in the notch's order, each with its own switch. The notch keeps at
 * least one cell on, so the last one that is on cannot be turned off.
 */
function GaugesGroup({ gauges, send }: { gauges: Gauge[]; send: Send }) {
  const rows = [...gauges].sort((a, b) => a.order - b.order);
  const on = rows.filter((g) => g.connected).length;
  const move = (id: string, by: number) => {
    const ids = rows.map((g) => g.id);
    const i = ids.indexOf(id);
    const j = i + by;
    if (j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    send({ command: "order", value: ids });
  };
  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Gauges">
      {rows.map((g, i) => {
        const icon = GAUGE_ICONS[g.glyph];
        return (
          <SettingRow className="ck-set" key={g.id} label={g.name} description={icon?.text ?? GAUGE_MARKS[g.glyph] ?? g.glyph}>
            {icon && <icon.Icon size={16} strokeWidth={1.75} aria-hidden="true" />}
            <span className="ck-order">
              <IconButton disabled={i === 0} onClick={() => move(g.id, -1)}
                label={`Move ${g.name} up`} title="Move up"><ChevronUp size={16} strokeWidth={1.75} aria-hidden="true" /></IconButton>
              <IconButton disabled={i === rows.length - 1} onClick={() => move(g.id, 1)}
                label={`Move ${g.name} down`} title="Move down"><ChevronDown size={16} strokeWidth={1.75} aria-hidden="true" /></IconButton>
            </span>
            <Toggle checked={g.connected} disabled={g.connected && on <= 1}
              onChange={(v) => send({ command: "connect", provider: g.id, value: v })} label={`Show ${g.name}`} />
          </SettingRow>
        );
      })}
      <div className="ck-sub ck-foot">At least one gauge stays on.</div>
    </SettingsGroup>
  );
}

function Accounts({ state, send }: { state: NotchState; send: Send }) {
  const order = state.providerOrder;
  const rank = (id: string) => {
    const i = order.indexOf(id);
    return i < 0 ? 999 : i;
  };
  const accounts = [...state.accounts].sort((a, b) => rank(a.id) - rank(b.id));
  const now = Date.now();
  const move = (id: string, by: number) => {
    const ids = accounts.map((a) => a.id);
    const i = ids.indexOf(id);
    const j = i + by;
    if (j < 0 || j >= ids.length) return;
    [ids[i], ids[j]] = [ids[j], ids[i]];
    send({ command: "order", value: ids });
  };
  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Logins Pulse reads (it never signs in itself)">
      {accounts.map((a, i) => {
        const freshness = accountFreshness(a, now);
        return (
        <SettingsItem key={a.id} className="ck-account" text={`${a.name} ${a.summary ?? a.signInExplanation} ${freshness ?? ""} ${(a.claudeAccounts ?? []).map((c) => `${c.name} ${c.email ?? ""} ${c.plan ?? ""}`).join(" ")}`}>
          <div className="ck-text">
            <div className="ck-account-name">
              <strong>{a.name}</strong>
              {!state.gauges && <span className={`ck-status ck-status-${a.connected ? "granted" : "off"}`}>{a.connected ? "Shown" : "Hidden"}</span>}
              {a.refusedAccess && <Badge tone="warn">Access refused</Badge>}
              {a.needsRenewal && <Badge tone="warn">Sign-in needed</Badge>}
            </div>
            <div className="ck-sub">{a.summary ?? a.signInExplanation}</div>
            {a.notice && <div className="ck-sub">{a.notice}</div>}
            {freshness && <div className="ck-sub">{freshness}</div>}
            <div className="ck-line ck-account-actions">
              {!a.summary && a.signInTitle && (
                <Button size="sm" variant="secondary" onClick={() => send({ command: "signIn", provider: a.id })}>{a.signInTitle}</Button>
              )}
              {a.usesKeychain && a.refusedAccess && (
                <Button size="sm" variant="secondary" onClick={() => send({ command: "allowAccess", provider: a.id })}>Allow access…</Button>
              )}
              {a.summary && (
                <Button size="sm" variant="ghost" onClick={() => send({ command: "signOut", provider: a.id })}
                  title={`Clears what Pulse read. You stay signed in to ${a.name.split(" ")[0]} itself.`}>Forget reading</Button>
              )}
            </div>
          </div>
          <div className="ck-ctl">
            <span className="ck-order">
              <IconButton disabled={i === 0} onClick={() => move(a.id, -1)}
                label={`Move ${a.name} up`} title="Move up"><ChevronUp size={16} strokeWidth={1.75} aria-hidden="true" /></IconButton>
              <IconButton disabled={i === accounts.length - 1} onClick={() => move(a.id, 1)}
                label={`Move ${a.name} down`} title="Move down"><ChevronDown size={16} strokeWidth={1.75} aria-hidden="true" /></IconButton>
            </span>
            {!state.gauges && (
              <Toggle checked={a.connected} onChange={(v) => send({ command: "connect", provider: a.id, value: v })} label={`Show ${a.name}`} />
            )}
          </div>
          {a.claudeAccounts && a.claudeAccounts.length > 0 && (
            <ClaudeAccountList accounts={a.claudeAccounts} send={send} />
          )}
          {a.claudeCliAccount && (
            <p className="ck-claude-cli">
              Command line: {a.claudeCliAccount.active ?? "none chosen"} for new terminals
              {a.claudeCliAccount.accounts.length > 1 ? ` (${a.claudeCliAccount.accounts.join(", ")}). Switch with pulse claude use <name>` : ""}
            </p>
          )}
        </SettingsItem>
        );
      })}
      <ClaudeMoveOffers />
    </SettingsGroup>
  );
}

/** One dry run of a Claude chat move (core `move_plan`): counts only, never chat contents. */
interface ClaudeMovePlan {
  from: string;
  to: string;
  would_add: number;
  would_update_if_newer: number;
  plan_id: string;
  target_running: boolean;
}

/**
 * "Move chats" (#353): what the account scan found in another Claude folder that the signed-in one
 * lacks. The confirm shows the dry-run counts; Move applies that plan by its id. the host app never closes
 * Claude: while it runs on the target, the move is refused and says so.
 */
function ClaudeMoveOffers() {
  const [offers, setOffers] = useState<ClaudeMovePlan[]>([]);
  const [pending, setPending] = useState<ClaudeMovePlan | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const load = useCallback(() => {
    invoke<ClaudeMovePlan[]>("claude_move_offers").then(setOffers).catch(() => setOffers([]));
  }, []);
  const confirming = useRef(false);
  confirming.current = pending !== null;
  useEffect(() => {
    load();
    // Not while the confirm is open: a reload would replace the plan the user is reading.
    const poll = visibleInterval(() => { if (!confirming.current) load(); }, 30000);
    return () => clearVisibleInterval(poll);
  }, [load]);
  if (offers.length === 0 && !message) return null;
  const apply = (plan: ClaudeMovePlan) => {
    setPending(null);
    invoke<{ files_changed?: number }>("claude_move_apply", { planId: plan.plan_id })
      .then((r) => setMessage(`Moved chats from ${plan.from} into ${plan.to} (${r.files_changed ?? 0} files, backed up first).`))
      .catch((e) => setMessage(String(e)))
      .finally(load);
  };
  return (
    <SettingsItem className="ck-account" text="Move chats between Claude accounts">
      <div className="ck-text">
        {offers.map((o) => (
          <div key={o.plan_id} className="ck-line">
            <span className="ck-sub">{o.from} has chats {o.to} lacks.</span>
            <Button size="sm" variant="secondary" disabled={o.target_running}
              title={o.target_running ? "Claude is running on the target account; close it yourself first" : undefined}
              onClick={() => setPending(o)}>Move chats…</Button>
          </div>
        ))}
        {message && <div className="ck-sub">{message}</div>}
      </div>
      {pending && (
        <ConfirmDialog
          title="Move chats?"
          description={`From ${pending.from} into ${pending.to}: ${pending.would_add} to add, ${pending.would_update_if_newer} to update if newer. A backup is taken first (pulse claude restore undoes it). ${pending.from} is left as it is. Claude is not closed.`}
          confirmLabel="Move"
          onConfirm={() => apply(pending)}
          onCancel={() => setPending(null)}
        />
      )}
    </SettingsItem>
  );
}

/**
 * The hub's own Full Disk Access, polled while the Permissions section is open.
 * Null until the first answer arrives.
 */
function useHubFullDiskAccess(): string | null {
  const [hub, setHub] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    const probe = () => {
      invoke<string>("fda_status")
        .then((value) => { if (alive) setHub(value); })
        .catch(() => { if (alive) setHub("unknown"); });
    };
    probe();
    const timer = visibleInterval(probe, 3000);
    return () => {
      alive = false;
      clearVisibleInterval(timer);
    };
  }, []);
  return hub;
}

/** What a Windows Permissions row's status says. */
const windowsStatusText = (id: string, status: Permission["status"]) =>
  status === "granted" ? (id === "startup" ? "On" : "Allowed")
  : status === "needsApproval" ? "Blocked"
  : status === "off" ? "Off" : "Unknown";

/**
 * The Permissions page on Windows: the rows Windows can answer (notifications, Start with
 * Windows, and the firewall row for nearby sharing). The Mac-only rows (Full Disk Access,
 * Accessibility, old grants) do not apply. The status comes from the system, read here; what
 * each row is needed for comes from the notch's rows (`notchRows`), as on the Mac.
 */
function WindowsPermissions({ notchRows, nearby }: { notchRows?: Permission[]; nearby: boolean }) {
  const [rows, setRows] = useState<Permission[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    const load = () => {
      invoke<Permission[]>("windows_permissions")
        .then((value) => { if (alive) { setRows(value); setError(null); } })
        .catch((e) => { if (alive) setError(String(e)); });
    };
    load();
    const timer = visibleInterval(load, 3000);
    return () => {
      alive = false;
      clearVisibleInterval(timer);
    };
  }, []);
  return (
    <>
      <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Permissions">
        {error && <div className="error ck-foot" role="alert">{error}</div>}
        {rows && <PermissionLead permissions={rows} />}
        {rows?.map((permission) => (
          <SettingsItem key={permission.id} className="ck-permrow" text={`${permission.title} ${permission.why}`}>
            <SettingRow className="ck-set" label={permission.title} description={permission.why}>
              <span className="ck-ctls">
                <span className={`ck-status ck-status-${permission.status}`}>{windowsStatusText(permission.id, permission.status)}</span>
                <Button size="sm" variant="secondary"
                  onClick={() => invoke<void>("windows_permission_open", { id: permission.id }).catch((e) => setError(String(e)))}>
                  Open Settings
                </Button>
              </span>
            </SettingRow>
            <NeedsLine needs={notchRows?.find((p) => p.id === permission.id)?.needs} />
          </SettingsItem>
        ))}
        <LocalNetworkRow enabled nearby={nearby} needs={notchRows?.find((p) => p.id === "firewall")?.needs} />
      </SettingsGroup>
    </>
  );
}

function PermissionLead({ permissions }: { permissions: Permission[] }) {
  const missing = permissions.filter((p) => p.required && p.status !== "granted").length;
  return (
    <div className={`ck-lead ck-tone-${missing > 0 ? "warn" : "ok"}`} role="status">
      {missing > 0
        ? `${missing} required permission${missing === 1 ? "" : "s"} still need${missing === 1 ? "s" : ""} approval.`
        : "Every required permission is granted."}
    </div>
  );
}

/**
 * Permission rows from the notch. The hub runs as the notch's child process, so
 * the notch's Full Disk Access covers it; the hub's own probe only confirms.
 */
function PermissionRows({ permissions, errors, send }: {
  permissions: Permission[];
  errors?: Record<string, string>;
  send: Send;
}) {
  const hub = useHubFullDiskAccess();
  return (
    <>
      {permissions.map((permission) => {
        const fda = permission.id === "fullDiskAccess";
        const awaitingHub = fda && permission.status === "granted" && hub === null;
        const restart = fda && permission.status === "granted" && hub !== null && hub !== "granted";
        const status = awaitingHub ? "unknown" : restart ? "needsApproval" : permission.status;
        const text = awaitingHub ? "Checking…"
          : restart ? "Restart Pulse to apply"
          : status === "granted" ? "Granted"
          : status === "needsApproval" ? "Needs approval"
          : status === "off" ? "Off" : "Unknown";
        const opensSettings = fda || status === "needsApproval";
        return (
          <SettingsItem key={permission.id} className="ck-permrow" text={`${permission.title} ${permission.why}`}>
            <SettingRow className="ck-set" label={permission.title} description={permission.why}>
              <span className="ck-ctls">
                <span className={`ck-status ck-status-${status}`}>{text}</span>
                <Button size="sm" variant="secondary" disabled={status === "granted"}
                  onClick={() => fda
                    ? invoke<void>("fda_request")
                    : send({ command: "permissionRequest", id: permission.id })}>
                  {permission.id === "finderMenu" ? "Turn On" : permission.id === "automation" ? "Allow" : permission.id.startsWith("engine") ? "Request" : opensSettings ? "Open Settings" : "Allow"}
                </Button>
              </span>
            </SettingRow>
            <NeedsLine needs={permission.needs} />
            {errors?.[permission.id] && <div className="error ck-foot" role="alert">{errors[permission.id]}</div>}
          </SettingsItem>
        );
      })}
      <StaleGrants />
    </>
  );
}

type StaleService = { service: string; label: string };
type StaleApp = { id: string; kind: "bundle" | "path"; services: StaleService[] };
type ResetOutcome = { id: string; service: string; ok: boolean; message: string };

/**
 * Permission grants that belong to apps that no longer exist. Read from TCC.db
 * (read-only); removed with `tccutil reset`, never by writing the database.
 */
function StaleGrants() {
  const [apps, setApps] = useState<StaleApp[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [results, setResults] = useState<ResetOutcome[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const key = (id: string, service: string) => `${id}\u0000${service}`;

  const scan = () => {
    setBusy(true);
    setError(null);
    setResults(null);
    invoke<StaleApp[]>("tcc_stale_scan")
      .then((found) => {
        setApps(found);
        setPicked(new Set(found.filter((a) => a.kind === "bundle")
          .flatMap((a) => a.services.map((s) => key(a.id, s.service)))));
      })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };

  const toggle = (k: string) => setPicked((prev) => {
    const next = new Set(prev);
    if (next.has(k)) next.delete(k); else next.add(k);
    return next;
  });

  const run = () => {
    setConfirm(false);
    if (!apps) return;
    const items = apps.filter((a) => a.kind === "bundle")
      .map((a) => ({ id: a.id, services: a.services.filter((s) => picked.has(key(a.id, s.service))).map((s) => s.service) }))
      .filter((i) => i.services.length > 0);
    setBusy(true);
    invoke<ResetOutcome[]>("tcc_reset", { items })
      .then((outcomes) => { setResults(outcomes); setApps(null); })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  };

  const bundles = (apps ?? []).filter((a) => a.kind === "bundle");
  const paths = (apps ?? []).filter((a) => a.kind === "path");
  const count = picked.size;
  const cleared = results?.filter((r) => r.ok).length ?? 0;
  const failed = results?.filter((r) => !r.ok) ?? [];

  return (
    <SettingsItem className="ck-permrow" text="Clean up old apps Finds permissions still granted to apps that are no longer installed">
      <SettingRow className="ck-set" label="Clean up old apps"
        description="Finds permissions still granted to apps that are no longer installed. Needs Full Disk Access to look.">
        <Button size="sm" variant="secondary" disabled={busy} onClick={scan}>
          {busy ? "Working…" : "Find old permissions"}
        </Button>
      </SettingRow>
      {error && <div className="error ck-foot" role="alert">{error}</div>}
      {apps && apps.length === 0 && <div className="ck-sub ck-foot" role="status">No permissions from removed apps were found.</div>}
      {bundles.map((a) => (
        <div key={a.id} className="ck-stale">
          <strong>{a.id}</strong>
          {a.services.map((s) => (
            <Checkbox key={s.service} className="ck-stale-check" checked={picked.has(key(a.id, s.service))}
              onChange={() => toggle(key(a.id, s.service))} label={s.label} />
          ))}
        </div>
      ))}
      {bundles.length > 0 && (
        <div className="ck-line ck-foot">
          <Button size="sm" variant="secondary" disabled={busy || count === 0} onClick={() => setConfirm(true)}>
            Clear {count} selected
          </Button>
        </div>
      )}
      {paths.length > 0 && (
        <div className="ck-sub ck-foot">
          <div>These were added by file path, which macOS will not let Pulse remove. Remove each with − in the matching System Settings list:</div>
          {paths.map((a) => (
            <div key={a.id} className="ck-indent">{a.id}: {a.services.map((s) => s.label).join(", ")}</div>
          ))}
        </div>
      )}
      {results && (
        <div className="ck-foot" role="status">
          <div>{cleared} cleared{failed.length > 0 ? `, ${failed.length} failed` : ""}.</div>
          {failed.map((r) => (
            <div key={key(r.id, r.service)} className="error">{r.id} ({r.service}): {r.message}</div>
          ))}
        </div>
      )}
      {confirm && (
        <ConfirmDialog
          title={`Clear ${count} old permission${count === 1 ? "" : "s"}?`}
          description="These apps are no longer installed. Pulse asks macOS to forget their grants with tccutil. Nothing else changes, and an app you reinstall will simply ask again."
          confirmLabel="Clear"
          onConfirm={run}
          onCancel={() => setConfirm(false)}
        />
      )}
    </SettingsItem>
  );
}
