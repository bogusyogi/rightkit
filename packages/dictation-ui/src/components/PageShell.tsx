import type { ReactNode } from "react";
import { PageShell as SharedPageShell } from "@rightkit/react-shell/react";
import { getPlatform, isMac } from "../hooks/usePlatform.js";
import type { TabKey } from "./Sidebar.js";
import { cx } from "../lib/ui.js";
import { HomeStatsMeta } from "./TitleBar.js";
import { hostAppName } from "../appName.js";

export const HERO_GLOW = "radial-gradient(120% 160% at 88% -20%, color-mix(in srgb, var(--hero-accent) 16%, var(--hero-surface)) 0%, color-mix(in srgb, var(--hero-accent) 5%, var(--hero-surface)) 42%, var(--hero-surface) 100%)";

export const PAGE_METAS: Record<string, { title: string; subtitle?: string | { mac: string; win: string } }> = {
  home: { title: "Welcome Back" },
  dictation: { title: "Dictation", subtitle: "Spoken formatting, vocabulary, and snippets." },
  controls: { title: "Controls", subtitle: "Recording triggers, hotkeys, and delivery behavior." },
  shortcuts: { title: "Controls", subtitle: "Recording triggers, hotkeys, and delivery behavior." },
  polish: { title: "Polish", subtitle: "Local cleanup first. AI polish only runs when you opt in." },
  settings: { title: "Settings", subtitle: `Configuration for ${hostAppName()}.` },
  about: { title: "About", subtitle: "License, privacy, and app information." },
  history: { title: "History", subtitle: "Local recovery log for text delivered into other apps." },
};

function metaFor(tab: string) {
  return PAGE_METAS[tab === "shortcuts" ? "controls" : tab] ?? PAGE_METAS.home;
}

function subtitleFor(tab: string): string | undefined {
  const { subtitle } = metaFor(tab);
  return typeof subtitle === "object" ? (isMac() ? subtitle.mac : subtitle.win) : subtitle;
}

// 0.1.213 title bar layout (12b7d8ce TitleBar.tsx): the title sits on the bar's bottom baseline
// (items-end, pb-[7px]; History pb-[11px] with a dimmed subtitle on macOS). Home on macOS is the one
// centred row, with its stats pushed to the right edge. The shell's title slot is only as tall as its
// content, so each piece below fills the bar height itself (see `.rk-top__title` in index.css).
const BAR_H = "h-[var(--rk-titlebar-height)]";

/** The page's title, in the title bar (not in the page body). */
export function PageTitleHeading({ tab }: { tab: string }) {
  return <h1 className="m-0 shrink-0 whitespace-nowrap [font-family:var(--font-display)] text-[28px] font-semibold leading-none text-[var(--paper)]">{metaFor(tab).title}</h1>;
}

/**
 * The line beside the title: Home's stats, else the page subtitle. Only macOS hides the subtitle below
 * 900px; the Windows body column always shows it (truncated), as 0.1.213 did.
 */
export function PageTitleMeta({ tab, mac }: { tab: string; mac: boolean }) {
  if (tab === "home") return <HomeStatsMeta />;
  const subtitle = subtitleFor(tab);
  if (!subtitle) return null;
  return <span className={cx("min-w-0 truncate text-[length:var(--size-sm)] font-normal text-[var(--muted)]", mac && "hidden min-[900px]:inline", mac && tab === "history" && "opacity-80")}>{subtitle}</span>;
}

/** macOS title bar: title and subtitle inline, after the traffic-light column. Home's stats go to the right edge. */
export function PageTitle({ tab }: { tab: string }) {
  if (tab === "home") {
    return <div data-tauri-drag-region data-titlebar-fill="" className={cx("flex min-w-0 items-center gap-[var(--space-4)] overflow-hidden pl-[var(--space-2)] pr-[var(--space-4)]", BAR_H)}><PageTitleHeading tab={tab} /><span className="ml-auto min-w-0 truncate"><PageTitleMeta tab={tab} mac /></span></div>;
  }
  return <div data-tauri-drag-region className={cx("flex min-w-0 items-end gap-[var(--space-3)] overflow-hidden pl-[var(--space-2)] pr-[var(--space-3)]", BAR_H, tab === "history" ? "pb-[11px]" : "pb-[7px]")}><PageTitleHeading tab={tab} /><PageTitleMeta tab={tab} mac /></div>;
}

/** Windows title bar, left half: the title sits over the sidebar column, right-aligned, on the baseline. */
export function PageTitleSidebarColumn({ tab }: { tab: string }) {
  return <div data-tauri-drag-region className="flex h-full min-w-0 flex-1 items-end justify-end overflow-hidden pb-[7px] pl-[var(--space-3)] pr-[var(--space-2)] text-right"><PageTitleHeading tab={tab} /></div>;
}

/** Windows title bar, right half: the subtitle (or Home's stats) in the body column, on the baseline. */
export function PageTitleBodyColumn({ tab }: { tab: string }) {
  return <div data-tauri-drag-region className={cx("flex min-w-0 items-end overflow-hidden pb-[7px] pl-[var(--space-2)] pr-[var(--space-3)]", BAR_H)}><PageTitleMeta tab={tab} mac={false} /></div>;
}

/** The title-bar title for the active page, laid out as 0.1.213 did on each platform. */
export function pageTitlebar(tab: string): { start?: ReactNode; title: ReactNode } {
  return getPlatform() === "windows"
    ? { start: <PageTitleSidebarColumn tab={tab} />, title: <PageTitleBodyColumn tab={tab} /> }
    : { title: <PageTitle tab={tab} /> };
}

function readTabFromHash(): string {
  if (typeof window === "undefined") return "home";
  const h = window.location.hash.replace(/^#/, "");
  return h === "shortcuts" ? "controls" : h || "home";
}

export interface PageShellProps {
  tab?: TabKey;
  header?: ReactNode;
  headerInContent?: boolean;
  children: ReactNode;
  fixedHeight?: boolean;
  heroMinHeight?: string;
  className?: string;
}

export default function PageShell({ tab, header, headerInContent = false, children, className }: PageShellProps) {
  const currentTab = tab ?? readTabFromHash();
  const body = headerInContent && header != null ? <><div className="border-b border-[color-mix(in_srgb,var(--line),transparent_40%)] py-[var(--space-2)]">{header}</div>{children}</> : children;
  return <div data-page={currentTab} className="contents"><SharedPageShell header={headerInContent ? undefined : header} maxWidth={1320} className={className}>{body}</SharedPageShell></div>;
}
