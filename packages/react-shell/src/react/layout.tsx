import {
  createContext,
  forwardRef,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type CSSProperties,
  type ElementType,
  type HTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactElement,
  type ReactNode,
  type Ref,
  type RefCallback,
} from "react";
import { createPortal } from "react-dom";
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
import { filterNavGroups, firstNavItem, flattenNav, navNeighborIndex, type NavGroup, type NavItem } from "../nav.js";
import { THEME_CHOICES, type ThemeChoice } from "../theme.js";
import { useMediaQuery, usePersistedState } from "./hooks.js";
import { ContextMenu, type MenuItem } from "./contextmenu.js";
import { ShellProvider, useCommands, useShellOptional, useShortcuts, useTheme, type ShellProviderProps } from "./provider.js";
import { IconButton, cx, type IconButtonProps } from "./ui.js";
import type { SlotClasses } from "./slots.js";

export interface ShellLabels {
  jumpTo: string;
  sections: string;
  settings: string;
  appearance: string;
  toggleSidebar: string;
  goTo: string;
  theme: Readonly<Record<ThemeChoice, string>>;
  /** Accessible name of the sidebar resize handle. Default "Resize sidebar". */
  resizeSidebar?: string;
}

export const DEFAULT_SHELL_LABELS: ShellLabels = {
  jumpTo: "Jump to",
  sections: "Sections",
  settings: "Settings",
  appearance: "Appearance",
  toggleSidebar: "Toggle sidebar",
  goTo: "Go to",
  theme: { system: "System", light: "Light", dark: "Dark" },
  resizeSidebar: "Resize sidebar",
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
/**
 * `side="left"` is the mirrored two-corner form: the curves sit on the row's left edge, for a tab or row that fuses
 * into a plane on its left. The default `side="right"` is unchanged.
 */
export function FuseCurves({ fill, size = 12, side = "right" }: { fill?: string; size?: number; side?: "right" | "left" } = {}) {
  const left = side === "left";
  return (
    <>
      {(["top", "bottom"] as const).map((edge) => {
        const transform = [left ? "scaleX(-1)" : null, edge === "bottom" ? "scaleY(-1)" : null].filter(Boolean).join(" ") || undefined;
        return (
          <svg
            key={edge}
            className={cx(`rk-fuse rk-fuse--${edge}`, left && "rk-fuse--left")}
            data-rk-fuse={edge}
            aria-hidden="true"
            focusable="false"
            width={size}
            height={size}
            viewBox="0 0 12 12"
            style={{ position: "absolute", [left ? "left" : "right"]: 0, [edge]: -size, width: size, height: size, pointerEvents: "none", ...(transform ? { transform } : {}), ...(fill ? { fill } : {}) }}
          >
            <path d={FUSE_PATH} />
          </svg>
        );
      })}
    </>
  );
}

/** Blur first so the focus ring does not stick when the window re-activates. */
/** Click handler of a caption button: optionally blur, then run the window action, swallowing its rejection. Exported for tests. */
export const blurThen = (fn: () => Promise<void>, blur = true) => (event: MouseEvent<HTMLButtonElement>) => {
  if (blur) event.currentTarget.blur();
  void fn().catch(() => undefined);
};

/** Replacement glyphs for the caption buttons. A glyph left out keeps its built-in SVG. */
export interface CaptionGlyphs {
  minimize?: ReactNode;
  maximize?: ReactNode;
  /** Shown on the maximize button while `maximized` is true. Falls back to `maximize`, then the built-in square. */
  restore?: ReactNode;
  close?: ReactNode;
}

/** Extra class per caption button, added after the built-in classes. */
export interface CaptionButtonClasses {
  minimize?: string;
  maximize?: string;
  close?: string;
}

/**
 * Tooltip (`title`) per caption button. A string replaces the label as the tooltip, `false` omits the attribute on that
 * button, and a button left out keeps its label. `false` for the whole object omits all three. `aria-label` is not
 * affected: it always carries the label.
 */
export type CaptionTitles = false | { minimize?: string | false; maximize?: string | false; close?: string | false };

export function CaptionControls({
  adapter,
  labels = createWindowControlLabels(),
  className,
  glyphs,
  maximized = false,
  buttonClasses,
  titles,
  blurOnClick = true,
}: {
  adapter: WindowControlAdapter;
  labels?: WindowControlLabels;
  className?: string;
  glyphs?: CaptionGlyphs;
  maximized?: boolean;
  buttonClasses?: CaptionButtonClasses;
  titles?: CaptionTitles;
  blurOnClick?: boolean;
}) {
  const controls = useMemo(() => createWindowControls(adapter), [adapter]);
  const titleOf = (key: "minimize" | "maximize" | "close"): string | undefined => {
    if (titles === false) return undefined;
    const override = titles?.[key];
    if (override === false) return undefined;
    return override ?? labels[key];
  };
  const maximizeGlyph = maximized ? (glyphs?.restore ?? glyphs?.maximize) : glyphs?.maximize;
  return (
    <div className={cx("rk-caption", className)}>
      <button type="button" className={cx("rk-caption__btn", buttonClasses?.minimize)} title={titleOf("minimize")} aria-label={labels.minimize} onClick={blurThen(controls.minimize, blurOnClick)}>
        {glyphs?.minimize ?? (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect x="0" y="4.5" width="10" height="1" fill="currentColor" /></svg>
        )}
      </button>
      <button type="button" className={cx("rk-caption__btn", buttonClasses?.maximize)} title={titleOf("maximize")} aria-label={labels.maximize} onClick={blurThen(controls.toggleMaximize, blurOnClick)}>
        {maximizeGlyph ?? (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><rect x="0.5" y="0.5" width="9" height="9" fill="none" stroke="currentColor" /></svg>
        )}
      </button>
      <button type="button" className={cx("rk-caption__btn rk-caption__close", buttonClasses?.close)} title={titleOf("close")} aria-label={labels.close} onClick={blurThen(controls.close, blurOnClick)}>
        {glyphs?.close ?? (
          <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5 9.5 9.5M9.5 0.5 0.5 9.5" stroke="currentColor" strokeWidth="1" /></svg>
        )}
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
  /**
   * Ref to the title wrapper element: a portal target for product content (`createPortal(node, el)`). The wrapper is
   * rendered, and is itself a drag region, whenever `title`, `titleRef` or `titleFill` is set.
   */
  titleRef?: Ref<HTMLDivElement>;
  /**
   * Make the title wrapper the flexible fill-and-drag element: it takes the free width of the bar (the separate
   * `rk-top__drag` spacer is dropped) and carries `data-tauri-drag-region` itself, so portalled or product content
   * in it needs no drag region of its own. Default false.
   */
  titleFill?: boolean;
  /** Right cluster, left of the caption buttons. */
  end?: ReactNode;
  windowControls?: WindowControlAdapter;
  windowControlLabels?: WindowControlLabels;
  /** Replacement caption glyphs. Each one left out keeps the built-in SVG. */
  captionGlyphs?: CaptionGlyphs;
  /**
   * True while the window is maximized: the maximize button shows `captionGlyphs.restore`. The window adapter does not
   * report window state, so the app passes it. Default false.
   */
  maximized?: boolean;
  /** Extra class per caption button (`minimize`, `maximize`, `close`), after the built-in classes. */
  captionButtonClasses?: CaptionButtonClasses;
  /** Tooltip per caption button, or `false` for none. Default: each button's label, as before. */
  captionTitles?: CaptionTitles;
  /**
   * Blur a caption button after a click so its focus ring does not stick when the window re-activates.
   * Default true. Pass false to keep focus on the button.
   */
  blurCaptionOnClick?: boolean;
  /** Element type of the root. Default `"header"`. The element gets the same classes and `data-tauri-drag-region`. */
  as?: ElementType;
  platform?: Platform;
  className?: string;
  /**
   * Extra classes per region: `root` (header, same as `className`), `side` (start cluster), `lights`
   * (macOS traffic-light space), `title`, `drag` (flexible drag region), `end` and `caption`.
   */
  slots?: SlotClasses<"root" | "side" | "lights" | "title" | "drag" | "end" | "caption">;
}

/**
 * Props that make an element a native window drag region. Tauri starts a drag only when the pressed element itself
 * carries the attribute (an ancestor's attribute does not count), so spread these on every element that fills the
 * bar and should move the window: `<div {...dragRegionProps} />`.
 */
export const dragRegionProps = { "data-tauri-drag-region": true } as const;

export interface DragRegionProps extends HTMLAttributes<HTMLElement> {
  /** Element type. Default `"div"`. */
  as?: ElementType;
  /** Fill the free width and full height of the flex row it sits in (flex 1, stretched, `min-width: 0`). Default false. */
  fill?: boolean;
}

/**
 * An element that drags the window. Use it for product header content outside a `Titlebar` (a mini window's own
 * header) and for spacers inside one. Children that are interactive (buttons, inputs) keep their own behaviour.
 */
export const DragRegion = forwardRef<HTMLElement, DragRegionProps>(function DragRegion({ as: Root = "div", fill = false, className, ...rest }, ref) {
  return <Root ref={ref} className={cx("rk-drag", fill && "rk-drag--fill", className)} {...dragRegionProps} {...rest} />;
});

/**
 * Drag-region titlebar usable in ANY layout. macOS reserves space for the native traffic lights
 * (Overlay title bar); Windows/Linux draw caption buttons from the adapter. Heights, paddings and
 * widths are `--rk-titlebar-*` / `--rk-lights-width` / `--rk-caption-*` tokens (see the README recipe).
 */
export function Titlebar({ start, startWidth, title, titleRef, titleFill, end, windowControls, windowControlLabels, captionGlyphs, maximized, captionButtonClasses, captionTitles, blurCaptionOnClick, as: Root = "header", platform: platformProp, className, slots }: TitlebarProps) {
  const detected = usePlatform();
  // The provider's platform wins over host detection: one injected platform for the whole shell.
  const provided = useShellOptional()?.platform;
  const platform = platformProp ?? provided ?? detected;
  const caps = platformCapabilities(platform);
  const style = startWidth !== undefined ? ({ width: startWidth } as CSSProperties) : undefined;
  return (
    <Root className={cx("rk-top", className, slots?.root)} data-tauri-drag-region>
      <div className={cx("rk-top__side", slots?.side)} style={style} data-tauri-drag-region>
        {caps.nativeTrafficLights ? <span className={cx("rk-top__lights", slots?.lights)} data-tauri-drag-region /> : null}
        {start}
      </div>
      {title || titleRef || titleFill ? <div ref={titleRef} className={cx("rk-top__title", titleFill && "rk-top__title--fill", slots?.title)} data-tauri-drag-region>{title}</div> : null}
      {titleFill ? null : <div className={cx("rk-top__drag", slots?.drag)} data-tauri-drag-region />}
      {end ? <div className={cx("rk-top__end", slots?.end)} data-tauri-drag-region>{end}</div> : null}
      {caps.customCaptionButtons && windowControls ? <CaptionControls adapter={windowControls} labels={windowControlLabels} className={slots?.caption} glyphs={captionGlyphs} maximized={maximized} buttonClasses={captionButtonClasses} titles={captionTitles} blurOnClick={blurCaptionOnClick} /> : null}
    </Root>
  );
}

/**
 * Root frame for any layout: platform attribute, tokens, font and full-height column. Use it
 * directly for a custom layout; `AppShell` composes it for the default fused one.
 */
export function AppFrame({ platform: platformProp, className, style, children }: { platform?: Platform; className?: string; style?: CSSProperties; children: ReactNode }) {
  const detected = usePlatform();
  // The provider's platform wins over host detection: one injected platform for the whole shell.
  const provided = useShellOptional()?.platform;
  const platform = platformProp ?? provided ?? detected;
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

/** A named colour preset (for example an accent family). The app applies it; the menu only reports the choice. */
export interface AppearancePreset {
  id: string;
  label: string;
  /** Up to three colours drawn as a swatch before the label, for example chrome, plane and accent. */
  swatches?: readonly string[];
}

/** A body font choice. Shown in its own face; apply the chosen family with `applyBodyFont` (theme). */
export interface AppearanceFont {
  id: string;
  label: string;
  /** CSS font-family list, for example `"Inter", system-ui, sans-serif`. */
  family: string;
}

export interface AppearanceMenuProps {
  value: ThemeChoice;
  onChange: (choice: ThemeChoice) => void;
  labels?: Pick<ShellLabels, "appearance" | "theme">;
  /** Named colour presets, listed after the light, dark and system choices, with swatches. */
  presets?: readonly AppearancePreset[];
  /** Id of the active preset. */
  preset?: string;
  onPresetChange?: (id: string) => void;
  /** Body font choices, listed last. */
  fonts?: readonly AppearanceFont[];
  /** Id of the active font. */
  font?: string;
  onFontChange?: (id: string) => void;
}

export function AppearanceMenu({ value, onChange, labels = DEFAULT_SHELL_LABELS, presets, preset, onPresetChange, fonts, font, onFontChange }: AppearanceMenuProps) {
  const [anchor, setAnchor] = useState<{ x: number; y: number } | null>(null);
  const swatchRow = (colours: readonly string[] | undefined) =>
    colours && colours.length ? (
      <span className="rk-swatches" aria-hidden="true">
        {colours.slice(0, 3).map((colour, i) => (
          <i key={i} style={{ background: colour }} />
        ))}
      </span>
    ) : null;
  const items: MenuItem[] = [
    ...THEME_CHOICES.map((choice) => ({ id: choice, label: labels.theme[choice], checked: value === choice, run: () => onChange(choice) })),
    ...(presets ?? []).map((p, i) => ({
      id: `preset:${p.id}`,
      label: (
        <span className="rk-appearance__opt">
          {swatchRow(p.swatches)}
          <span>{p.label}</span>
        </span>
      ),
      checked: preset === p.id,
      separatorBefore: i === 0,
      run: () => onPresetChange?.(p.id),
    })),
    ...(fonts ?? []).map((f, i) => ({
      id: `font:${f.id}`,
      label: <span style={{ fontFamily: f.family }}>{f.label}</span>,
      checked: font === f.id,
      separatorBefore: i === 0,
      run: () => onFontChange?.(f.id),
    })),
  ];
  const wide = (presets?.length ?? 0) > 0 || (fonts?.length ?? 0) > 0;
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
      {anchor ? <ContextMenu x={anchor.x} y={anchor.y} width={wide ? 200 : 160} items={items} label={labels.appearance} onClose={() => setAnchor(null)} /> : null}
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
export function JumpTo({ groups, query, onQueryChange, onNavigate, label = DEFAULT_SHELL_LABELS.jumpTo, platform: platformProp }: JumpToProps) {
  const provided = useShellOptional()?.platform;
  const platform = platformProp ?? provided;
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
export function PaletteTrigger({ onOpen, label = DEFAULT_SHELL_LABELS.jumpTo, chord = "mod+k", platform: platformProp, className }: { onOpen: () => void; label?: string; chord?: string | false; platform?: Platform; className?: string }) {
  const provided = useShellOptional()?.platform;
  const platform = platformProp ?? provided;
  return (
    <button type="button" className={cx("rk-search rk-search--trigger", className)} aria-label={`${label} (command palette)`} aria-haspopup="dialog" onClick={onOpen}>
      <SearchIcon />
      <span className="rk-search__text">{label}…</span>
      {chord ? <Kbd chord={chord} platform={platform} gap={3} /> : null}
    </button>
  );
}

/** Extra attributes for a `NavList` row button: drag-and-drop handlers, data attributes and a class (merged). */
export type NavRowProps = Omit<ButtonHTMLAttributes<HTMLButtonElement>, "type" | "children" | "onClick" | "role" | "tabIndex" | "aria-current" | "data-nav-id"> & {
  [key: `data-${string}`]: string | undefined;
};

export interface NavListProps {
  groups: readonly NavGroup[];
  activeId: string;
  onNavigate: (id: string) => void;
  label?: string;
  /** Override a row; call `defaultRow` to keep standard rendering. */
  renderItem?: (item: NavItem, state: { active: boolean }, defaultRow: ReactNode) => ReactNode;
  /**
   * Props for each row button, for example drop-target handlers (`onDragOver`, `onDrop`) or a highlight class. Spread
   * before the shell's own props, so click, keys, role and the active state stay the shell's.
   */
  itemProps?: (item: NavItem, state: { active: boolean }) => NavRowProps | undefined;
  /**
   * When it returns an object, the row shows a native checkbox before its label, labelled with the item's label
   * (for example a calendar list). `onChange` receives the new checked state.
   */
  itemCheck?: (item: NavItem) => { checked: boolean; onChange: (checked: boolean) => void } | undefined;
  /** Titles of the collapsed groups. Only read when `onToggleGroup` is set. */
  collapsedGroups?: readonly string[];
  /** Makes titled groups collapsible: the title becomes a button with `aria-expanded`, and the handler gets the title. */
  onToggleGroup?: (title: string) => void;
  /** Twisty glyph for a collapsible group title; receives whether the group is open. Default ▾ open, ▸ closed. */
  renderTwisty?: (open: boolean) => ReactNode;
}

/** Grouped nav with the fused active row and roving arrow-key focus. Reusable in custom sidebars. */
export function NavList({ groups, activeId, onNavigate, label = DEFAULT_SHELL_LABELS.sections, renderItem, itemProps, itemCheck, collapsedGroups, onToggleGroup, renderTwisty }: NavListProps) {
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
      {groups.map((group, gi) => {
        const title = group.title;
        const collapsible = title !== undefined && onToggleGroup !== undefined;
        const open = !(collapsible && collapsedGroups?.includes(title ?? ""));
        return (
          <div className="rk-group" key={title ?? `g${gi}`}>
            {title ? (
              collapsible ? (
                <h6>
                  <button type="button" className="rk-group__toggle" aria-expanded={open} onClick={() => onToggleGroup?.(title ?? "")}>
                    {renderTwisty ? renderTwisty(open) : <span aria-hidden="true">{open ? "▾" : "▸"}</span>}
                    {title}
                  </button>
                </h6>
              ) : (
                <h6>{title}</h6>
              )
            ) : null}
            {open
              ? group.items.filter((i) => !i.hidden).map((item) => {
                  const active = activeId === item.id;
                  const extra = itemProps?.(item, { active });
                  const check = itemCheck?.(item);
                  const row = (
                    <button
                      key={item.id}
                      {...extra}
                      type="button"
                      data-nav-id={item.id}
                      className={cx(`rk-item${active ? " is-active" : ""}`, extra?.className)}
                      aria-current={active ? "page" : undefined}
                      aria-label={item.ariaLabel}
                      tabIndex={active || (!flat.some((f) => f.id === activeId) && item === flat[0]) ? 0 : -1}
                      onClick={() => onNavigate(item.id)}
                    >
                      {item.icon}
                      <span className="rk-item__label">{item.label}</span>
                      {item.badge ? <span className="rk-item__badge">{item.badge}</span> : null}
                      {active ? <FuseCurves /> : null}
                    </button>
                  );
                  const node = renderItem ? <div key={item.id}>{renderItem(item, { active }, row)}</div> : row;
                  if (!check) return node;
                  return (
                    <div key={item.id} className="rk-item-row">
                      <input
                        type="checkbox"
                        className="rk-item__check"
                        aria-label={item.label}
                        checked={check.checked}
                        onChange={(event) => check.onChange(event.target.checked)}
                      />
                      {node}
                    </div>
                  );
                })
              : null}
          </div>
        );
      })}
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
  /**
   * Side column (inspector) beside the work column, full height under the header band, scrolling on its own. Omit for
   * the single-column page, unchanged.
   */
  side?: ReactNode;
  /** Width of the side column: a number is px, a string any CSS length. Sets `--rk-page-side-width` (default 320px). */
  sideWidth?: number | string;
  /** Accessible name of the side column. Default "Inspector". */
  sideLabel?: string;
  className?: string;
  children: ReactNode;
}

/** Standard page body: optional fixed header band, title block, scroll region, optional side column. */
export function PageShell({ title, subtitle, actions, header, maxWidth = 1100, scroll = true, side, sideWidth, sideLabel = "Inspector", className, children }: PageShellProps) {
  const style = maxWidth ? ({ maxWidth } as CSSProperties) : undefined;
  const hasSide = side != null && side !== false;
  const sideStyle = sideWidth !== undefined ? ({ "--rk-page-side-width": typeof sideWidth === "number" ? `${sideWidth}px` : sideWidth } as CSSProperties) : undefined;
  const work = (
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
  );
  return (
    <div className="rk-page">
      {header != null ? <div className="rk-page__band"><div className="rk-page__container" style={style}>{header}</div></div> : null}
      {hasSide ? (
        <div className="rk-page__split" style={sideStyle}>
          {work}
          <aside className="rk-page__side" aria-label={sideLabel}>{side}</aside>
        </div>
      ) : (
        work
      )}
    </div>
  );
}

export interface AppShellProps {
  groups: readonly NavGroup[];
  activeId: string;
  onNavigate: (id: string) => void;
  /** Titlebar heading, usually the active view title. */
  title?: ReactNode;
  /**
   * Titlebar right cluster (status pills, actions), left of caption controls. The wrapper element is itself a window
   * drag region, so the empty space around product controls moves the window; interactive children keep their own
   * clicks. Put product controls here instead of replacing the whole titlebar.
   */
  titlebarEnd?: ReactNode;
  /**
   * Titlebar cluster over the sidebar column, after the traffic lights. Same drag behaviour as `titlebarEnd`: the
   * wrapper is a drag region, product content needs no `data-tauri-drag-region` of its own.
   */
  titlebarStart?: ReactNode;
  /** Ref to the default titlebar's title wrapper (portal target for product titlebar content). See `TitlebarProps.titleRef`. */
  titleRef?: Ref<HTMLDivElement>;
  /** Make the default titlebar's title wrapper the fill-and-drag element. See `TitlebarProps.titleFill`. */
  titleFill?: boolean;
  /**
   * Bar at the top of the content plane that is a window drag region (the reader-plane drag bar). Its content, if any,
   * renders inside it; `planeBarRef` exposes the element as a portal target for a product toolbar. Omit both for no bar.
   */
  planeBar?: ReactNode;
  planeBarRef?: Ref<HTMLDivElement>;
  /**
   * Rendered in the sidebar column position while the sidebar is collapsed and the window is not narrow (an icon rail).
   * The column takes `sidebarRailWidth`; with `titlebar={false}` it also carries a drag-region band the height of the
   * titlebar. Use `RailButton` for its buttons.
   */
  sidebarRail?: ReactNode;
  /** Width of the collapsed rail: a number is px, a string any CSS length. Default 52. */
  sidebarRailWidth?: number | string;
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
  /** Native bridge (`@rightkit/tauri-shell`); `window` supplies caption buttons. */
  bridge?: AppShellBridge;
  /** @deprecated Use `bridge.window`. */
  windowControls?: WindowControlAdapter;
  windowControlLabels?: WindowControlLabels;
  labels?: Partial<ShellLabels>;
  /** Override detection (tests, browser QA). Defaults to platform-ui. */
  platform?: Platform;
  sidebarWidth?: number;
  /**
   * Drag handle on the sidebar's right edge. Pointer drag and Left/Right arrows (Shift for larger steps; Home and End
   * for the limits) resize it. `min` defaults to 160 and `max` to 420 px. `storageKey` remembers the width across
   * reloads. `maxViewport` is a CSS length (for example `24vw`) the width never exceeds: the sidebar and the titlebar's
   * sidebar column use `min(var(--rk-sidebar-width), maxViewport)`. `true` enables the handle with the defaults.
   */
  sidebarResizable?: boolean | { min?: number; max?: number; storageKey?: string; maxViewport?: string };
  /** Called with the width in px when a resize ends (pointer up or key press). */
  onSidebarWidthChange?: (px: number) => void;
  className?: string;
  /** Options for the implicit `ShellProvider` (ignored when an ancestor provider exists). Includes `keybindings` and `paletteWidth`. */
  shell?: Omit<ShellProviderProps, "children" | "bridge" | "platform">;
  /**
   * Extra classes per region: `root` (frame, same as `className`), `titlebar` (default titlebar),
   * `sidebar`, `sidebarFoot`, `body` (the row under the titlebar), `plane` (the content surface), `planeBar` and `rail`.
   */
  slots?: SlotClasses<"root" | "titlebar" | "sidebar" | "sidebarFoot" | "body" | "plane" | "planeBar" | "rail">;
  children: ReactNode;
}

function normalizeResize(value: true | { min?: number; max?: number; storageKey?: string; maxViewport?: string }): { min: number; max: number; storageKey?: string; maxViewport?: string } {
  const o = value === true ? {} : value;
  return { min: o.min ?? 160, max: o.max ?? 420, storageKey: o.storageKey, maxViewport: o.maxViewport };
}

function clampWidth(px: number, limits: { min: number; max: number }): number {
  return Math.min(limits.max, Math.max(limits.min, Math.round(px)));
}

function ShellLayout(props: AppShellProps) {
  const detected = usePlatform();
  const shell = useShellOptional()!;
  const platform = props.platform ?? shell.platform ?? detected;
  const labels: ShellLabels = { ...DEFAULT_SHELL_LABELS, ...props.labels, theme: { ...DEFAULT_SHELL_LABELS.theme, ...props.labels?.theme } };
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

  const resize = props.sidebarResizable ? normalizeResize(props.sidebarResizable) : null;
  const [storedWidth, setStoredWidth] = usePersistedState<number>(resize?.storageKey ?? null, props.sidebarWidth ?? 220);
  const [dragWidth, setDragWidth] = useState<number | null>(null);
  const dragging = useRef<{ startX: number; startWidth: number; last: number } | null>(null);
  const shownWidth = resize ? clampWidth(dragWidth ?? storedWidth, resize) : null;
  const commitWidth = (px: number) => {
    if (!resize) return;
    const next = clampWidth(px, resize);
    setStoredWidth(next);
    props.onSidebarWidthChange?.(next);
  };
  const style = shownWidth !== null
    ? ({ "--rk-sidebar-width": `${shownWidth}px`, ...(resize?.maxViewport ? { "--rk-sidebar-max-viewport": resize.maxViewport } : {}) } as CSSProperties)
    : props.sidebarWidth ? ({ "--rk-sidebar-width": `${props.sidebarWidth}px` } as CSSProperties) : undefined;
  const sideHidden = collapsed;
  const railWidth = props.sidebarRailWidth ?? 52;
  const railWidthCss = typeof railWidth === "number" ? `${railWidth}px` : railWidth;
  const showRail = props.sidebarRail != null && hasSidebar && sideHidden && !narrow;
  const rail = showRail ? (
    <aside className={cx("rk-rail", props.slots?.rail)} aria-label={labels.sections} style={{ width: railWidthCss }}>
      {props.titlebar === false ? <div className="rk-rail__drag" data-tauri-drag-region /> : null}
      {props.sidebarRail}
    </aside>
  ) : null;
  const planeBar = props.planeBar != null || props.planeBarRef ? (
    <div ref={props.planeBarRef} className={cx("rk-plane__bar", props.slots?.planeBar)} data-rk-plane-bar data-tauri-drag-region>{props.planeBar}</div>
  ) : null;
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
    <AppFrame platform={platform} className={cx("rk-shell--fused", sideHidden && "is-collapsed", showRail && "has-rail", narrow && "is-narrow", !hasSidebar && "no-sidebar", props.className, props.slots?.root)} style={showRail ? ({ ...style, "--rk-rail-width": railWidthCss } as CSSProperties) : style}>
      {props.titlebar === false ? null : props.titlebar !== undefined ? (
        props.titlebar
      ) : (
        <Titlebar
          start={<>{toggle}{props.titlebarStart}</>}
          startWidth={showRail ? railWidthCss : sideHidden || !hasSidebar ? "auto" : undefined}
          title={props.title}
          titleRef={props.titleRef}
          titleFill={props.titleFill}
          end={props.titlebarEnd}
          windowControls={adapter}
          windowControlLabels={props.windowControlLabels}
          platform={platform}
          className={props.slots?.titlebar}
        />
      )}
      <div className={cx("rk-body", props.slots?.body)}>
        {hasSidebar ? props.sidebar ?? defaultSidebar : null}
        {rail}
        {resize && shownWidth !== null && hasSidebar && !sideHidden ? (
          <div
            role="separator"
            aria-orientation="vertical"
            aria-label={labels.resizeSidebar ?? DEFAULT_SHELL_LABELS.resizeSidebar}
            aria-valuemin={resize.min}
            aria-valuemax={resize.max}
            aria-valuenow={shownWidth}
            tabIndex={0}
            className="rk-side-resizer"
            data-rk-resizer
            style={{ left: resize.maxViewport ? `calc(min(var(--rk-sidebar-width), var(--rk-sidebar-max-viewport)) - 3px)` : shownWidth - 3 }}
            onPointerDown={(event) => {
              event.currentTarget.setPointerCapture?.(event.pointerId);
              dragging.current = { startX: event.clientX, startWidth: shownWidth, last: shownWidth };
              setDragWidth(shownWidth);
            }}
            onPointerMove={(event) => {
              const d = dragging.current;
              if (!d || !resize) return;
              d.last = clampWidth(d.startWidth + event.clientX - d.startX, resize);
              setDragWidth(d.last);
            }}
            onPointerUp={(event) => {
              const d = dragging.current;
              dragging.current = null;
              event.currentTarget.releasePointerCapture?.(event.pointerId);
              setDragWidth(null);
              if (d) commitWidth(d.last);
            }}
            onKeyDown={(event) => {
              if (!resize) return;
              const step = event.shiftKey ? 32 : 8;
              let next: number | null = null;
              if (event.key === "ArrowLeft") next = shownWidth - step;
              else if (event.key === "ArrowRight") next = shownWidth + step;
              else if (event.key === "Home") next = resize.min;
              else if (event.key === "End") next = resize.max;
              if (next === null) return;
              event.preventDefault();
              commitWidth(next);
            }}
          />
        ) : null}
        <Plane className={props.slots?.plane}>{planeBar}{props.children}</Plane>
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

export type RailTooltipSide = "right" | "left" | "top" | "bottom";

export interface RailButtonProps extends Omit<IconButtonProps, "pressed"> {
  /** Side the tooltip opens on, exposed as `data-rk-tooltip-side`. Default `"right"` (the rail sits on the left). */
  tooltipSide?: RailTooltipSide;
  /** Sets `aria-expanded`, for a rail button that opens a menu or panel. */
  expanded?: boolean;
  /** Sets `aria-current="page"` and the `is-active` class, for the rail button of the current destination. */
  current?: boolean;
}

/**
 * Icon button for the `AppShell` `sidebarRail` slot: an `IconButton` (label is the accessible name and tooltip) with
 * the tooltip side and the `aria-expanded` / `aria-current` states a rail needs.
 */
export const RailButton = forwardRef<HTMLButtonElement, RailButtonProps>(function RailButton({ tooltipSide = "right", expanded, current, className, ...rest }, ref) {
  return (
    <IconButton
      ref={ref}
      className={cx("rk-rail__btn", current && "is-active", className)}
      aria-expanded={expanded}
      aria-current={current ? "page" : undefined}
      data-rk-tooltip-side={tooltipSide}
      {...rest}
    />
  );
});

export interface ResizeHandleOptions {
  /** Current size in px (width for a vertical handle, height for a horizontal one). Controlled: the handle never stores it. */
  width: number;
  /** Called with the new clamped, rounded size on every pointer move and key press. */
  onWidthChange: (px: number) => void;
  /** Called once with the final size when a pointer drag ends. */
  onWidthCommit?: (px: number) => void;
  min: number;
  max: number;
  /** Arrow-key step in px; Shift multiplies it by 4. Default 8. */
  step?: number;
  /**
   * The edge of the resized panel the handle sits on. `"end"` (right edge of a left sidebar, bottom edge of a top
   * panel) grows the panel when dragged away from it toward the end; `"start"` (left edge of a right dock, top edge of
   * a bottom dock) grows it when dragged toward the start. Default `"end"`.
   */
  edge?: "start" | "end";
  /** `"vertical"` is a vertical bar resizing a width; `"horizontal"` a horizontal bar resizing a height. Default `"vertical"`. */
  orientation?: "vertical" | "horizontal";
  disabled?: boolean;
  /** Accessible name. Default "Resize". */
  label?: string;
}

const clampSize = (px: number, min: number, max: number) => Math.min(max, Math.max(min, Math.round(px)));

/**
 * Props for a separator that resizes a panel by pointer and keyboard (Left/Right or Up/Down by `step`, Shift for
 * larger steps, Home and End for the limits). Fully controlled and storage-free: the app owns the size, so it can be
 * remembered per project or reset when the project changes. Spread the result on an element you place yourself.
 */
export function useResizeHandle({ width, onWidthChange, onWidthCommit, min, max, step = 8, edge = "end", orientation = "vertical", disabled, label = "Resize" }: ResizeHandleOptions) {
  const drag = useRef<{ start: number; startSize: number; last: number } | null>(null);
  const vertical = orientation === "vertical";
  // Sign of the size change per unit of pointer or key movement toward the end of the axis.
  const sign = edge === "end" ? 1 : -1;
  const axis = (event: { clientX: number; clientY: number }) => (vertical ? event.clientX : event.clientY);
  const finish = (event: ReactPointerEvent<HTMLElement>) => {
    const d = drag.current;
    drag.current = null;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
    if (d) onWidthCommit?.(d.last);
  };
  return {
    role: "separator" as const,
    "aria-orientation": vertical ? ("vertical" as const) : ("horizontal" as const),
    "aria-label": label,
    "aria-valuemin": min,
    "aria-valuemax": max,
    "aria-valuenow": clampSize(width, min, max),
    "aria-disabled": disabled || undefined,
    tabIndex: disabled ? -1 : 0,
    "data-rk-resizer": "",
    "data-edge": edge,
    "data-orientation": orientation,
    onPointerDown: (event: ReactPointerEvent<HTMLElement>) => {
      if (disabled || event.button !== 0) return;
      event.preventDefault();
      event.currentTarget.setPointerCapture?.(event.pointerId);
      const startSize = clampSize(width, min, max);
      drag.current = { start: axis(event), startSize, last: startSize };
    },
    onPointerMove: (event: ReactPointerEvent<HTMLElement>) => {
      const d = drag.current;
      if (!d) return;
      const next = clampSize(d.startSize + sign * (axis(event) - d.start), min, max);
      if (next === d.last) return;
      d.last = next;
      onWidthChange(next);
    },
    onPointerUp: finish,
    onPointerCancel: finish,
    onKeyDown: (event: ReactKeyboardEvent<HTMLElement>) => {
      if (disabled) return;
      const amount = event.shiftKey ? step * 4 : step;
      const toward = vertical ? "ArrowRight" : "ArrowDown";
      const away = vertical ? "ArrowLeft" : "ArrowUp";
      let next: number | null = null;
      if (event.key === toward) next = width + sign * amount;
      else if (event.key === away) next = width - sign * amount;
      else if (event.key === "Home") next = min;
      else if (event.key === "End") next = max;
      if (next === null) return;
      event.preventDefault();
      const clamped = clampSize(next, min, max);
      if (clamped !== clampSize(width, min, max)) onWidthChange(clamped);
    },
  };
}

export interface SidebarResizerProps extends ResizeHandleOptions {
  className?: string;
  style?: CSSProperties;
}

/**
 * Controlled resize handle for sidebars and docks (see `useResizeHandle`). Renders a separator you position with
 * `className` or `style`; by default it is a 6px bar with the matching resize cursor.
 */
export function SidebarResizer({ className, style, ...options }: SidebarResizerProps) {
  const props = useResizeHandle(options);
  return <div {...props} className={cx("rk-resizer", className)} style={style} />;
}

type RetainedSnapshot = { scroll: Array<[Element, number, number]>; focus: HTMLElement | null };

type MovableParent = Element & { moveBefore?: (node: Node, child: Node | null) => void };

function captureRetained(node: HTMLElement): RetainedSnapshot {
  const scroll: Array<[Element, number, number]> = [];
  const walker = document.createTreeWalker(node, NodeFilter.SHOW_ELEMENT);
  for (let el = walker.nextNode() as Element | null; el; el = walker.nextNode() as Element | null) {
    if (el.scrollTop !== 0 || el.scrollLeft !== 0) scroll.push([el, el.scrollTop, el.scrollLeft]);
  }
  const active = document.activeElement;
  return { scroll, focus: active instanceof HTMLElement && node.contains(active) ? active : null };
}

function restoreRetained(snapshot: RetainedSnapshot | null) {
  if (!snapshot) return;
  for (const [el, top, left] of snapshot.scroll) {
    el.scrollTop = top;
    el.scrollLeft = left;
  }
  const { focus } = snapshot;
  if (focus && focus.isConnected && document.activeElement !== focus) focus.focus({ preventScroll: true });
}

/** One DOM node per key, owned for the life of the host; the claiming element is where the node currently sits. */
class RetainedRegistry {
  private nodes = new Map<string, HTMLElement>();
  private pending = new Map<string, RetainedSnapshot>();

  node(key: string): HTMLElement {
    let node = this.nodes.get(key);
    if (!node) {
      node = document.createElement("div");
      node.className = "rk-retained";
      node.style.display = "contents";
      node.setAttribute("data-rk-retained", key);
      this.nodes.set(key, node);
    }
    return node;
  }

  claim(key: string, target: HTMLElement) {
    const node = this.node(key);
    if (node.parentNode === target) return;
    const snapshot = this.pending.get(key) ?? (node.isConnected ? captureRetained(node) : null);
    this.pending.delete(key);
    let moved = false;
    const parent = target as MovableParent;
    // moveBefore keeps focus, scroll, iframes and playing media only when both ends are connected.
    if (typeof parent.moveBefore === "function" && node.isConnected && target.isConnected) {
      try {
        parent.moveBefore(node, null);
        moved = true;
      } catch {
        moved = false;
      }
    }
    if (!moved) {
      target.appendChild(node);
      restoreRetained(snapshot);
    }
  }

  /** The claiming element is about to go away: record scroll and focus while the node is still attached. */
  release(key: string, target: HTMLElement | null) {
    const node = this.nodes.get(key);
    if (!node || !target || node.parentNode !== target) return;
    if (node.isConnected) this.pending.set(key, captureRetained(node));
  }

  prune(keys: ReadonlySet<string>) {
    for (const [key, node] of this.nodes) {
      if (keys.has(key)) continue;
      node.remove();
      this.nodes.delete(key);
      this.pending.delete(key);
    }
  }
}

const RetainedContext = createContext<RetainedRegistry | null>(null);

export interface RetainedHostProps {
  /**
   * The retained content by key. Each entry is rendered once, into a node the host owns, and stays mounted (state,
   * scroll position, focus, decoded images) while the geometry around it changes. Render the geometry shells as
   * `children` and claim each node with `useRetainedSlot(key)` or `<RetainedSlot slotKey={key} />`.
   */
  slots: Readonly<Record<string, ReactNode>>;
  children?: ReactNode;
}

/**
 * Keeps stateful subtrees mounted while the layout that holds them changes. The host renders each slot into a
 * persistent DOM node through a React portal; whichever geometry currently renders a claim element for that key
 * receives the node. When the claim moves the node is moved, not re-created: with `Element.moveBefore` where the
 * engine has it (focus and scroll intact), otherwise a guarded `appendChild` that restores scroll offsets and focus.
 * A key with no claim keeps its node, detached, and its children stay mounted.
 */
export function RetainedHost({ slots, children }: RetainedHostProps) {
  const registry = useRef<RetainedRegistry | null>(null);
  if (registry.current === null && typeof document !== "undefined") registry.current = new RetainedRegistry();
  const reg = registry.current;
  const keys = Object.keys(slots);
  const keySignature = keys.join("\u0000");
  useEffect(() => {
    reg?.prune(new Set(keySignature ? keySignature.split("\u0000") : []));
  }, [reg, keySignature]);
  return (
    <RetainedContext.Provider value={reg}>
      {children}
      {reg ? keys.map((key) => createPortal(slots[key], reg.node(key), key)) : null}
    </RetainedContext.Provider>
  );
}

/**
 * Ref callback that claims the retained node for `key` into the element it is attached to. Put it on the element of
 * the current geometry that should contain that content. Must be used under a `RetainedHost`.
 */
export function useRetainedSlot(key: string): RefCallback<HTMLElement> {
  const registry = useContext(RetainedContext);
  const current = useRef<HTMLElement | null>(null);
  if (!registry && typeof document !== "undefined") throw new Error("useRetainedSlot must be used inside a RetainedHost");
  return useCallback(
    (element: HTMLElement | null) => {
      if (element) {
        current.current = element;
        registry?.claim(key, element);
      } else {
        registry?.release(key, current.current);
        current.current = null;
      }
    },
    [registry, key],
  );
}

/** An element that receives the retained content for `slotKey`. Renders a `div` (or `as`) that lays out like any container; the retained node inside it is `display: contents`. */
export function RetainedSlot({ slotKey, as: Root = "div", className, style }: { slotKey: string; as?: ElementType; className?: string; style?: CSSProperties }) {
  const ref = useRetainedSlot(slotKey);
  return <Root ref={ref} className={className} style={style} data-rk-retained-slot={slotKey} />;
}
