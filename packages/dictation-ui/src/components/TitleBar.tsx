import { memo } from "react";
import { Check, MagnifyingGlass, ShieldCheck } from "@phosphor-icons/react";
import { Titlebar as SharedTitlebar } from "@rightkit/react-shell/react";
import { useSegmentedControl } from "@rightkit/react-shell/headless";
import { cx } from "../lib/ui.js";
import { usePlatform } from "../hooks/usePlatform.js";
import { useHomeDashboard } from "../hooks/useHomeDashboard.js";
import { useHistorySourceSelection } from "../hooks/useHistorySourceSelection.js";
import { useSettings } from "../hooks/useSettings.js";
import type { SettingsBlob } from "../types/settings.js";
import { historySearch, useField } from "../lib/fieldStore.js";
import { HISTORY_SOURCES } from "../pages/history/historyUtils.js";
import type { TabKey } from "./Sidebar.js";
import { noMaximizeCaptionClasses, tauriWindowControls } from "./WindowControls.js";
import Toggle from "./ui/Toggle.js";
import SegmentedControl from "./settings/SegmentedControl.js";
import { MAC_PTT_KEY_OPTIONS, WINDOWS_PTT_KEY_OPTIONS } from "../lib/pushToTalk.js";

export const HomeStatsMeta = memo(function HomeStatsMeta() {
  const { wordsThisWeek, timeSaved, history } = useHomeDashboard("titlebar");
  const stats = [
    { value: wordsThisWeek.toLocaleString(), label: "words" },
    { value: timeSaved, label: "saved" },
    { value: history.length.toLocaleString(), label: "inserts" },
  ];
  return <span className="flex min-w-0 items-center justify-center gap-[6px] overflow-hidden">
    {stats.map((stat) => <span key={stat.label} className="inline-flex h-[25px] max-w-[128px] items-center gap-[5px] rounded-full border border-[color-mix(in_srgb,var(--line),transparent_42%)] bg-[color-mix(in_srgb,var(--surface-2),transparent_18%)] px-[9px] text-[length:var(--size-xs)] text-[var(--muted)]"><span className="min-w-0 truncate [font-family:var(--font-mono)] text-[var(--paper)]">{stat.value}</span><span className="min-w-0 truncate">{stat.label}</span></span>)}
  </span>;
});

const HomePrivacyBadge = memo(function HomePrivacyBadge() {
  return <span className="inline-flex h-[24px] shrink-0 items-center gap-[6px] whitespace-nowrap rounded-full border border-[color-mix(in_srgb,var(--line),transparent_30%)] bg-[color-mix(in_srgb,var(--surface-2),transparent_18%)] px-[10px] text-[length:var(--size-xs)] text-[var(--muted)]" title="Audio never leaves this device"><ShieldCheck className="h-[13px] w-[13px] text-[var(--accent)]" weight="fill" aria-hidden="true" />On-device · no audio uploaded</span>;
});

function HistoryTitlebarControls() {
  const { sourceSelection, setSourceSelection } = useHistorySourceSelection();
  const [query, setQuery] = useField(historySearch);
  // Apple sync, and so the iPhone and Watch sources, exists on macOS only; Windows lists this PC alone.
  const mac = usePlatform() === "macos";
  return <div className="flex h-full shrink-0 items-center gap-[var(--space-3)] pr-[var(--space-4)]">{mac ? <fieldset className="m-0 flex items-center gap-[5px] border-0 p-0"><legend className="sr-only">History sources</legend>{HISTORY_SOURCES.map((source) => <label key={source.key} className="inline-flex h-8 shrink-0 cursor-pointer items-center gap-[6px] rounded-[var(--radius-sm)] px-[7px] text-[length:var(--size-xs)] text-[var(--muted)] hover:bg-[var(--surface-hover)] hover:text-[var(--paper)]"><span className="relative grid size-4 shrink-0 place-items-center"><input type="checkbox" className="peer absolute inset-0 m-0 size-4 cursor-pointer appearance-none rounded-[4px] border border-[color-mix(in_srgb,var(--muted),transparent_25%)] bg-transparent checked:bg-[color-mix(in_srgb,var(--paper),transparent_78%)]" checked={sourceSelection[source.key]} aria-label={`Show ${source.label} history`} onChange={(event) => setSourceSelection({ ...sourceSelection, [source.key]: event.target.checked })} /><Check aria-hidden="true" className="pointer-events-none relative z-10 size-3 text-[var(--hr-ink)] opacity-0 peer-checked:opacity-100" weight="bold" /></span><span>{source.label}</span></label>)}</fieldset> : null}<span className="relative flex h-7 w-[170px] min-w-0 items-center rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_30%)] bg-[var(--surface-2)]"><MagnifyingGlass aria-hidden="true" className="pointer-events-none absolute left-2 top-1/2 h-[13px] w-[13px] -translate-y-1/2 text-[var(--muted)]" /><input type="search" className="h-full w-full min-w-0 border-0 bg-transparent pl-[26px] pr-2 text-[length:var(--size-sm)] text-[var(--paper)] outline-none" aria-label="Search history" placeholder="Search" value={query} onChange={(e) => setQuery(e.target.value)} /></span></div>;
}

const pillVisibilityOptions = [
  { value: "always", label: "Always on", title: "Keep the resting pill visible" },
  { value: "recording_only", label: "Recording", title: "Show the pill only while recording" },
] as const;

function TitlebarSegmented<T extends string>({ settingId, label, value, options, disabled, onChange }: { settingId: string; label: string; value: T; options: ReadonlyArray<{ value: T; label: string; title?: string }>; disabled?: boolean; onChange: (value: T) => void }) {
  const { groupProps, getOptionProps } = useSegmentedControl<T>({ value, options: options.map((option) => ({ value: option.value })), onChange, label, disabled, mode: "toggle" });
  return <span data-setting-id={settingId} className="flex items-center gap-[6px] text-[length:var(--size-xs)] text-[var(--muted)]">{label}<span {...groupProps} className="inline-grid grid-flow-col gap-[3px] rounded-[var(--radius-sm)] border border-[color-mix(in_srgb,var(--line),transparent_30%)] bg-[var(--surface-2)] p-[3px]">{options.map((option) => <button key={option.value} type="button" disabled={disabled} {...getOptionProps(option.value)} aria-label={`${label} ${option.label}`} title={option.title ?? `${label}: ${option.label}`} className={cx("rounded-[calc(var(--radius-sm)-2px)] px-[9px] py-[5px] text-[length:var(--size-xs)] font-medium", value === option.value ? "bg-[var(--surface-3)] font-semibold text-[var(--paper)]" : "text-[var(--muted)] hover:bg-[var(--surface-hover)] hover:text-[var(--paper)]")} >{option.label}</button>)}</span></span>;
}

function ControlsTitlebarToggles() {
  const { loaded, get, patch } = useSettings();
  const mac = usePlatform() === "macos";
  const pttSetting = mac ? "mac_push_to_talk_key" : "push_to_talk_key";
  const pttOn = get("push_to_talk", true);
  const pttKey = (get(pttSetting, mac ? "ctrl" : "ralt") as string) || (mac ? "ctrl" : "ralt");
  const options = [{ value: "off", label: "Off" }, ...(mac ? MAC_PTT_KEY_OPTIONS : WINDOWS_PTT_KEY_OPTIONS)];
  return <div className="flex h-full shrink-0 items-center gap-[var(--space-3)] pr-[var(--space-4)]"><span className="flex items-center gap-[6px] text-[length:var(--size-xs)] text-[var(--muted)]">Push-to-talk<SegmentedControl label="Push-to-talk" value={pttOn ? pttKey : "off"} disabled={!loaded} options={options} onChange={(value) => value === "off" ? void patch("push_to_talk", false) : void (async () => { await patch(pttSetting, value); if (!pttOn) await patch("push_to_talk", true); })()} /></span><span className="h-[18px] w-px bg-[var(--line)]" aria-hidden="true" /><span className="flex items-center gap-[6px] text-[length:var(--size-xs)] text-[var(--muted)]">Middle-click<Toggle checked={get("middle_mouse_recording", true)} disabled={!loaded} ariaLabel="Middle-click" onChange={(v) => void patch("middle_mouse_recording", v)} /></span></div>;
}

function SettingsTitlebarControls() {
  const { loaded, get, patch } = useSettings();
  const value = get("pill_visibility", "always") as NonNullable<SettingsBlob["pill_visibility"]>;
  const normalized = value === "during_dictation" ? "recording_only" : value;
  return <div className="flex h-full shrink-0 items-center gap-[var(--space-3)] pr-[var(--space-4)]"><TitlebarSegmented settingId="settings.show-pill" label="Pill" value={normalized} options={pillVisibilityOptions} disabled={!loaded} onChange={(next) => void patch("pill_visibility", next)} /></div>;
}

interface TitleBarProps { active: TabKey }

export function TitlebarTools({ active }: TitleBarProps) {
  const platform = usePlatform();
  return active === "history" && platform !== "ios" ? <HistoryTitlebarControls /> : active === "controls" && platform !== "ios" ? <ControlsTitlebarToggles /> : active === "settings" && platform !== "ios" ? <SettingsTitlebarControls /> : active === "home" ? <span className="flex h-full shrink-0 items-center pr-[var(--space-4)]"><HomePrivacyBadge /></span> : null;
}

function TitleBar({ active }: TitleBarProps) {
  return <SharedTitlebar end={<TitlebarTools active={active} />} windowControls={tauriWindowControls} captionButtonClasses={noMaximizeCaptionClasses} />;
}

export default memo(TitleBar);
