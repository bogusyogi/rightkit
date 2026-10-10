import {
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent,
  type ReactElement,
  type ReactNode,
} from "react";
import {
  createWindowControlLabels,
  createWindowControls,
  platformCapabilities,
  type Platform,
  type WindowControlAdapter,
  type WindowControlLabels,
} from "@rightkit/platform-ui";
import { Kbd, usePlatform } from "@rightkit/platform-ui/react";
import type { AppShellBridge } from "../bridge.js";
import type { Command } from "../commands.js";
import { filterNavGroups, firstNavItem, flattenNav, navNeighborIndex, type NavGroup } from "../nav.js";
import { THEME_CHOICES, type ThemeChoice } from "../theme.js";
import { useMediaQuery } from "./hooks.js";
import { ContextMenu, type MenuItem } from "./contextmenu.js";
import { ShellProvider, useCommands, useShellOptional, useShortcuts, useTheme, type ShellProviderProps } from "./provider.js";
import { IconButton, cx } from "./ui.js";
import type { SlotClasses } from "./slots.js";

export interface ShellLabels {
  jumpTo: string;
  sections: string;
  settings: string;
  appearance: string;
  toggleSidebar: string;
  goTo: string;
  theme: Readonly<Record<ThemeChoice, string>>;
}

export const DEFAULT_SHELL_LABELS: ShellLabels = {
  jumpTo: "Jump to",
  sections: "Sections",
  settings: "Settings",
  appearance: "Appearance",
  toggleSidebar: "Toggle sidebar",
  goTo: "Go to",
  theme: { system: "System", light: "Light", dark: "Dark" },
};

const icon = (children: ReactElement) => (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {children}
  </svg>
);
export const SearchIcon = () => icon(<><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></>);
export const GearIcon = () => icon(<><circle cx="8" cy="8" r="2.2" /><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4" /></>);
export const ContrastIcon = () => icon(<><circle cx="8" cy="8" r="6" /><path d="M8 2a6 6 0 0 1 0 12z" fill="currentColor" /></>);
export const SidebarIcon = () => icon(<><rect x="2" y="3" width="12" height="10" rx="2" /><path d="M6 3v10" /></>);

const FUSE_PATH = "M12 0V12H0A12 12 0 0 0 12 0Z";

/**
 * Antialiased 12px concave curves bridging the active row into the plane (SVG, not hard-stop
 * masks; ViewRight's `SidebarTabFuseCurves`). Put them inside any `position: relative` row that should fuse.
 * Positioning is inline, so they work without shell.css; pass `fill` (the joined surface colour) when shell.css is absent.
 */
export function FuseCurves({ fill, size = 12 }: { fill?: string; size?: number } = {}) {
  return (
    <>
      {(["top", "bottom"] as const).map((edge) => (
        <svg
          key={edge}
          className={`rk-fuse rk-fuse--${edge}`}
          data-rk-fuse={edge}
          aria-hidden="true"
          focusable="false"
          width={size}
          height={size}
          viewBox="0 0 12 12"
          style={{ position: "absolute", right: 0, [edge]: -size, width: size, height: size, pointerEvents: "none", ...(edge === "bottom" ? { transform: "scaleY(-1)" } : {}), ...(fill ? { fill } : {}) }}
        >
          <path d={FUSE_PATH} />
        </svg>
      ))}
    </>
  );
}

/** Blur first so the focus ring does not stick when the window re-activates. */
const blurThen = (fn: () => Promise<void>) => (event: MouseEvent<HTMLButtonElement>) => {
  event.currentTarget.blur();
  void fn().catch(() => undefined);
};

export function CaptionControls({ adapter, labels = createWindowControlLabels(), className }: { adapter: WindowControlAdapter; labels?: WindowControlLabels; className?: string }) {
  const controls = useMemo(() => createWindowControls(adapter), [adapter]);
  return (
    <div className={cx("rk-caption", className)}>
      <button type="button" className="rk-caption__btn" title={labels.minimize} aria-label={labels.minimize} onClick={blurThen(controls.minimize)}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect x="0" y="4.5" width="10" height="1" fill="currentColor" /></svg>
      </button>
      <button type="button" className="rk-caption__btn" title={labels.maximize} aria-label={labels.maximize} onClick={blurThen(controls.toggleMaximize)}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect x="0.5" y="0.5" width="9" height="9" fill="none" stroke="currentColor" /></svg>
      </button>
      <button type="button" className="rk-caption__btn rk-caption__close" title={labels.close} aria-label={labels.close} onClick={blurThen(controls.close)}>
        <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5 9.5 9.5M9.5 0.5 0.5 9.5" stroke="currentColor" strokeWidth="1" /></svg>
      </button>
    </div>
  );
}

export interface TitlebarProps {
  /** Left cluster, aligned over the sidebar column in the fused layout (sidebar toggle, back/forward). */
  start?: ReactNode;
  /** Width of the start cluster. Defaults to the sidebar width in the fused layout, auto otherwise. */
  startWidth?: number | string;
  title?: ReactNode;
  /** Right cluster, left of the caption buttons. */
  end?: ReactNode;
  windowControls?: WindowControlAdapter;
  windowControlLabels?: WindowControlLabels;
  platform?: Platform;
  className?: string;
  /**
   * Extra classes per region: `root` (header, same as `className`), `side` (start cluster), `lights`
   * (macOS traffic-light space), `title`, `drag` (flexible drag region), `end` and `caption`.
   */
  slots?: SlotClasses<"root" | "side" | "lights" | "title" | "drag" | "end" | "caption">;
}

/**
 * Drag-region titlebar usable in ANY layout. macOS reserves space for the native traffic lights
 * (Overlay title bar); Windows/Linux draw caption buttons from the adapter. Heights, paddings and
 * widths are `--rk-titlebar-*` / `--rk-lights-width` / `--rk-caption-*` tokens (see the README recipe).
 */
export function Titlebar({ start, startWidth, title, end, windowControls, windowControlLabels, platform: platformProp, className, slots }: TitlebarProps) {
  const detected = usePlatform();
  const platform = platformProp ?? detected;
  const caps = platformCapabilities(platform);
  const style = startWidth !== undefined ? ({ width: startWidth } as CSSProperties) : undefined;
  return (
    <header className={cx("rk-top", className, slots?.root)} data-tauri-drag-region>
      <div className={cx("rk-top__side", slots?.side)} style={style} data-tauri-drag-region>
        {caps.nativeTrafficLights ? <span className={cx("rk-top__lights", slots?.lights)} data-tauri-drag-region /> : null}
        {start}
      </div>
      {title ? <div className={cx("rk-top__title", slots?.title)} data-tauri-drag-region>{title}</div> : null}
      <div className={cx("rk-top__drag", slots?.drag)} data-tauri-drag-region />
      {end}
      {caps.customCaptionButtons && windowControls ? <CaptionControls adapter={windowControls} labels={windowControlLabels} className={slots?.caption} /> : null}
    </header>
  );
}

/**
 * Root frame for any layout: platform attribute, tokens, font and full-height column. Use it
 * directly for a custom layout; `AppShell` composes it for the default fused one.
 */
export function AppFrame({ platform: platformProp, className, style, children }: { platform?: Platform; className?: string; style?: CSSProperties; children: ReactNode }) {
  const detected = usePlatform();
  const platform = platformProp ?? detected;
  return (
    <div className={`rk-shell${className ? ` ${className}` : ""}`} data-platform={platform} style={style}>
      {children}
    </div>
  );
}

/** The content surface the sidebar fuses into. */
export function Plane({ children, className }: { children: ReactNode; className?: string }) {
  return <section className={`rk-plane${className ? ` ${className}` : ""}`} data-rk-plane>{children}</section>;
}

export interface AppearanceMenuProps {
  value: ThemeChoice;
  onChange: (choice: ThemeChoice) => void;
  labels?: Pick<ShellLabels, "appearance" | "theme">;
}

export function AppearanceMenu({ value, onChange, labels = DEFAULT_SHELL_LABELS }: AppearanceMenuProps) {
  const [anchor, setAnchor] = useState<{ x: number; y: number } | null>(null);
  const items: MenuItem[] = THEME_CHOICES.map((choice) => ({ id: choice, label: labels.theme[choice], checked: value === choice, run: () => onChange(choice) }));
  return (
    <>
      <IconButton
        label={labels.appearance}
        pressed={anchor !== null}
        aria-haspopup="menu"
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect();
          setAnchor({ x: r.right - 160, y: r.top - items.length * 28 - 16 });
        }}
      >
        <ContrastIcon />
      </IconButton>
      {anchor ? <ContextMenu x={anchor.x} y={anchor.y} width={160} items={items} label={labels.appearance} onClose={() => setAnchor(null)} /> : null}
    </>
  );
}

export interface JumpToProps {
  groups: readonly NavGroup[];
  query: string;
  onQueryChange: (query: string) => void;
  onNavigate: (id: string) => void;
  label?: string;
  platform?: Platform;
}

/** Inline sidebar filter (`searchMode="filter"`). Enter opens the first match; Esc clears. */
export function JumpTo({ groups, query, onQueryChange, onNavigate, label = DEFAULT_SHELL_LABELS.jumpTo, platform }: JumpToProps) {
  const ref = useRef<HTMLInputElement | null>(null);
  return (
    <label className="rk-search">
      <SearchIcon />
      <input
        ref={ref}
        aria-label={label}
        placeholder={`${label}…`}
        value={query}
        onChange={(event) => onQueryChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter") {
            const match = firstNavItem(filterNavGroups(groups, query));
            if (match) {
              onNavigate(match.id);
              onQueryChange("");
            }
          }
          if (event.key === "Escape") onQueryChange("");
        }}
      />
      <Kbd chord="mod+f" platform={platform} gap={3} />
    </label>
  );
}

/** Sidebar search that opens the command palette (HeardRight / CodeRight pattern). */
/** `chord={false}` hides the shortcut hint (the binding is disabled in `keybindings`). */
export function PaletteTrigger({ onOpen, label = DEFAULT_SHELL_LABELS.jumpTo, chord = "mod+k", platform, className }: { onOpen: () => void; label?: string; chord?: string | false; platform?: Platform; className?: string }) {
  return (
    <button type="button" className={cx("rk-search rk-search--trigger", className)} aria-label={`${label} (command palette)`} aria-haspopup="dialog" onClick={onOpen}>
      <SearchIcon />
      <span className="rk-search__text">{label}…</span>
      {chord ? <Kbd chord={chord} platform={platform} gap={3} /> : null}
    </button>
  );
}

export interface NavListProps {
  groups: readonly NavGroup[];
  activeId: string;
  onNavigate: (id: string) => void;
  label?: string;
  /** Override a row; call `defaultRow` to keep standard rendering. */
  renderItem?: (item: NavGroup["items"][number], state: { active: boolean }, defaultRow: ReactNode) => ReactNode;
}

/** Grouped nav with the fused active row and roving arrow-key focus. Reusable in custom sidebars. */
export function NavList({ groups, activeId, onNavigate, label = DEFAULT_SHELL_LABELS.sections, renderItem }: NavListProps) {
  const ref = useRef<HTMLElement | null>(null);
  const flat = flattenNav(groups);
  const onKeyDown = (event: ReactKeyboardEvent) => {
    const buttons = Array.from(ref.current?.querySelectorAll<HTMLButtonElement>("button[data-nav-id]") ?? []);
    const current = buttons.indexOf(document.activeElement as HTMLButtonElement);
    const next = navNeighborIndex("vertical", event.key, current < 0 ? 0 : current, buttons.length);
    if (next !== null) {
      event.preventDefault();
      buttons[next]?.focus();
    }
  };
  return (
    <nav className="rk-nav" aria-label={label} ref={ref} onKeyDown={onKeyDown}>
      {groups.map((group, gi) => (
        <div className="rk-group" key={group.title ?? `g${gi}`}>
          {group.title ? <h6>{group.title}</h6> : null}
          {group.items.filter((i) => !i.hidden).map((item) => {
            const active = activeId === item.id;
            const row = (
              <button
                key={item.id}
                type="button"
                data-nav-id={item.id}
                className={`rk-item${active ? " is-active" : ""}`}
                aria-current={active ? "page" : undefined}
                tabIndex={active || (!flat.some((f) => f.id === activeId) && item === flat[0]) ? 0 : -1}
                onClick={() => onNavigate(item.id)}
              >
                {item.icon}
                <span className="rk-item__label">{item.label}</span>
                {item.badge ? <span className="rk-item__badge">{item.badge}</span> : null}
                {active ? <FuseCurves /> : null}
              </button>
            );
            return renderItem ? <div key={item.id}>{renderItem(item, { active }, row)}</div> : row;
          })}
        </div>
      ))}
    </nav>
  );
}

export interface PageShellProps {
  title?: ReactNode;
  subtitle?: ReactNode;
  /** Right of the title (buttons, filters). */
  actions?: ReactNode;
  /** Fixed band above the scroll region (HeardRight hero band); does not scroll. */
  header?: ReactNode;
  /** Cap on content width in px; 0 for full width. Default 1100. */
  maxWidth?: number;
  /** Set false when the page manages its own scrolling (editors, canvases). */
  scroll?: boolean;
  className?: string;
  children: ReactNode;
}

/** Standard page body: optional fixed header band, title block, scroll region. */
export function PageShell({ title, subtitle, actions, header, maxWidth = 1100, scroll = true, className, children }: PageShellProps) {
  const style = maxWidth ? ({ maxWidth } as CSSProperties) : undefined;
  return (
    <div className="rk-page">
      {header != null ? <div className="rk-page__band"><div className="rk-page__container" style={style}>{header}</div></div> : null}
      <div className={`rk-page__scroll${scroll ? "" : " is-fixed"}`}>
        <div className={`rk-page__container${className ? ` ${className}` : ""}`} style={style}>
          {title || subtitle || actions ? (
            <header className="rk-page__head">
              <div>
                {title ? <h1>{title}</h1> : null}
                {subtitle ? <p>{subtitle}</p> : null}
              </div>
              {actions ? <div className="rk-page__actions">{actions}</div> : null}
            </header>
          ) : null}
          {children}
        </div>
      </div>
    </div>
  );
}

export interface AppShellProps {
  groups: readonly NavGroup[];
  activeId: string;
  onNavigate: (id: string) => void;
  /** Titlebar heading, usually the active view title. */
  title?: ReactNode;
  /** Titlebar right cluster (status pills, actions), left of caption controls. */
  titlebarEnd?: ReactNode;
  /** Titlebar cluster over the sidebar column, after the traffic lights. */
  titlebarStart?: ReactNode;
  /** Replace the whole titlebar; `false` removes it (the app draws its own). */
  titlebar?: ReactNode | false;
  /** Replace the whole sidebar column; `false` removes it (sidebar-less layout inside the frame). */
  sidebar?: ReactNode | false;
  /** Above the nav list, below the search. */
  sidebarTop?: ReactNode;
  /** Bottom-left brand lockup. Colour the brand half with `var(--rk-accent)`. */
  wordmark?: ReactNode;
  /** Controlled theme; omit to use the provider's theme store. */
  theme?: ThemeChoice;
  onThemeChange?: (choice: ThemeChoice) => void;
  /** Settings is an app-owned destination; omit to hide the button. */
  onOpenSettings?: () => void;
  settingsActive?: boolean;
  /** Health / queue rows above the wordmark footer. */
  statusFooter?: ReactNode;
  /** Sidebar search: palette trigger (default), inline filter, or none. */
  searchMode?: "palette" | "filter" | "none";
  /** Extra palette commands (the shell already adds "Go to" for every destination). */
  commands?: readonly Command[];
  sidebarCollapsed?: boolean;
  onSidebarCollapsedChange?: (collapsed: boolean) => void;
  /** Show the titlebar sidebar toggle and bind the sidebar chord (`keybindings.sidebar`, default `mod+\`). Default true. */
  sidebarToggle?: boolean;
  /** Native bridge (`@rightkit/shell`); `window` supplies caption buttons. */
  bridge?: AppShellBridge;
  /** @deprecated Use `bridge.window`. */
  windowControls?: WindowControlAdapter;
  windowControlLabels?: WindowControlLabels;
  labels?: Partial<ShellLabels>;
  /** Override detection (tests, browser QA). Defaults to platform-ui. */
  platform?: Platform;
  sidebarWidth?: number;
  className?: string;
  /** Options for the implicit `ShellProvider` (ignored when an ancestor provider exists). Includes `keybindings` and `paletteWidth`. */
  shell?: Omit<ShellProviderProps, "children" | "bridge" | "platform">;
  /**
   * Extra classes per region: `root` (frame, same as `className`), `titlebar` (default titlebar),
   * `sidebar`, `sidebarFoot`, `body` (the row under the titlebar) and `plane` (the content surface).
   */
  slots?: SlotClasses<"root" | "titlebar" | "sidebar" | "sidebarFoot" | "body" | "plane">;
  children: ReactNode;
}

function ShellLayout(props: AppShellProps) {
  const detected = usePlatform();
  const platform = props.platform ?? detected;
  const labels: ShellLabels = { ...DEFAULT_SHELL_LABELS, ...props.labels, theme: { ...DEFAULT_SHELL_LABELS.theme, ...props.labels?.theme } };
  const shell = useShellOptional()!;
  const providerTheme = useTheme();
  const theme = props.theme ?? providerTheme.choice;
  const setTheme = props.onThemeChange ?? providerTheme.setChoice;
  const narrow = useMediaQuery("(max-width: 719px)");
  const [collapsedState, setCollapsedState] = useState(false);
  const collapsed = props.sidebarCollapsed ?? collapsedState;
  const setCollapsed = (value: boolean) => {
    setCollapsedState(value);
    props.onSidebarCollapsedChange?.(value);
  };
  const hasSidebar = props.sidebar !== false;
  const [query, setQuery] = useState("");
  const visible = useMemo(() => filterNavGroups(props.groups, query), [props.groups, query]);
  const adapter = props.bridge?.window ?? props.windowControls;
  const searchMode = props.searchMode ?? "palette";
  const sidebarToggle = props.sidebarToggle ?? true;

  const navCommands = useMemo<Command[]>(
    () => [
      ...flattenNav(props.groups).map((item) => ({ id: `nav:${item.id}`, label: item.label, group: labels.goTo, icon: item.icon, keywords: item.keywords, run: () => props.onNavigate(item.id) })),
      ...(props.onOpenSettings ? [{ id: "nav:settings", label: labels.settings, group: labels.goTo, keywords: ["preferences"], run: () => props.onOpenSettings?.() }] : []),
      ...THEME_CHOICES.map((choice) => ({ id: `theme:${choice}`, label: `${labels.appearance}: ${labels.theme[choice]}`, group: labels.appearance, keywords: ["theme", "dark", "light"], run: () => setTheme(choice) })),
      ...(hasSidebar && sidebarToggle ? [{ id: "shell.sidebar.toggle", label: labels.toggleSidebar, group: "View", shortcut: "shell.sidebar", run: () => setCollapsed(!collapsed) }] : []),
      ...(props.commands ?? []),
    ],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [props.groups, props.commands, props.onOpenSettings, labels.goTo, labels.settings, labels.appearance, labels.toggleSidebar, hasSidebar, sidebarToggle, collapsed],
  );
  useCommands(navCommands);
  const sidebarChord = shell.keybindings.sidebar;
  useShortcuts(
    hasSidebar && sidebarToggle && sidebarChord
      ? [{ id: "shell.sidebar", chord: sidebarChord, label: labels.toggleSidebar, group: "View", rebindable: true, allowInInput: true, run: () => setCollapsed(!collapsed) }]
      : [],
  );

  const style = props.sidebarWidth ? ({ "--rk-sidebar-width": `${props.sidebarWidth}px` } as CSSProperties) : undefined;
  const sideHidden = collapsed;
  const toggle = hasSidebar && sidebarToggle ? (
    <IconButton label={labels.toggleSidebar} pressed={!sideHidden} onClick={() => setCollapsed(!collapsed)} className="rk-top__toggle">
      <SidebarIcon />
    </IconButton>
  ) : null;

  const defaultSidebar = (
    <aside className={cx("rk-side", props.slots?.sidebar)} aria-label={labels.sections} hidden={sideHidden}>
      {searchMode === "palette" ? <PaletteTrigger onOpen={shell.palette.show} label={labels.jumpTo} platform={platform} chord={shell.keybindings.palette} /> : null}
      {searchMode === "filter" ? <JumpTo groups={props.groups} query={query} onQueryChange={setQuery} onNavigate={props.onNavigate} label={labels.jumpTo} platform={platform} /> : null}
      {props.sidebarTop}
      <NavList groups={visible} activeId={props.activeId} onNavigate={(id) => { props.onNavigate(id); if (narrow) setCollapsed(true); }} label={labels.sections} />
      <div className={cx("rk-side__foot", props.slots?.sidebarFoot)}>
        {props.statusFooter ? <div className="rk-status">{props.statusFooter}</div> : null}
        <footer className="rk-brand-foot">
          <span className="rk-wordmark">{props.wordmark}</span>
          {props.onOpenSettings ? (
            <IconButton label={labels.settings} pressed={props.settingsActive} onClick={props.onOpenSettings}>
              <GearIcon />
            </IconButton>
          ) : null}
          <AppearanceMenu value={theme} onChange={setTheme} labels={labels} />
        </footer>
      </div>
    </aside>
  );

  return (
    <AppFrame platform={platform} className={cx("rk-shell--fused", sideHidden && "is-collapsed", narrow && "is-narrow", !hasSidebar && "no-sidebar", props.className, props.slots?.root)} style={style}>
      {props.titlebar === false ? null : props.titlebar !== undefined ? (
        props.titlebar
      ) : (
        <Titlebar
          start={<>{toggle}{props.titlebarStart}</>}
          startWidth={sideHidden || !hasSidebar ? "auto" : undefined}
          title={props.title}
          end={props.titlebarEnd}
          windowControls={adapter}
          windowControlLabels={props.windowControlLabels}
          platform={platform}
          className={props.slots?.titlebar}
        />
      )}
      <div className={cx("rk-body", props.slots?.body)}>
        {hasSidebar ? props.sidebar ?? defaultSidebar : null}
        <Plane className={props.slots?.plane}>{props.children}</Plane>
      </div>
    </AppFrame>
  );
}

/**
 * Default fused layout: sidebar + titlebar on one chrome surface joined to the page plane. Every
 * region has a slot or an override; for a different layout skip `AppShell` and compose
 * `ShellProvider` + `AppFrame` + `Titlebar` + `Plane` + `NavList` yourself (or pass `sidebar={false}`).
 */
export function AppShell(props: AppShellProps) {
  const existing = useShellOptional();
  const detected = usePlatform();
  const layout = <ShellLayout {...props} />;
  if (existing) return layout;
  return (
    <ShellProvider {...props.shell} bridge={props.bridge} platform={props.platform ?? detected}>
      {layout}
    </ShellProvider>
  );
}
