import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
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
import {
  THEME_CHOICES,
  filterNavGroups,
  firstNavItem,
  type NavGroup,
  type ThemeChoice,
} from "./index.js";

export * from "./index.js";

export interface ShellLabels {
  jumpTo: string;
  sections: string;
  settings: string;
  appearance: string;
  theme: Readonly<Record<ThemeChoice, string>>;
}

export const DEFAULT_SHELL_LABELS: ShellLabels = {
  jumpTo: "Jump to",
  sections: "Sections",
  settings: "Settings",
  appearance: "Appearance",
  theme: { system: "System", light: "Light", dark: "Dark" },
};

const icon = (children: ReactElement) => (
  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
    {children}
  </svg>
);
const SearchIcon = () => icon(<><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></>);
const GearIcon = () => icon(<><circle cx="8" cy="8" r="2.2" /><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M12.6 3.4l-1.4 1.4M4.8 11.2l-1.4 1.4" /></>);
const ContrastIcon = () => icon(<><circle cx="8" cy="8" r="6" /><path d="M8 2v12" fill="currentColor" /><path d="M8 2a6 6 0 0 1 0 12z" fill="currentColor" /></>);

const FUSE_PATH = "M12 0V12H0A12 12 0 0 0 12 0Z";

/** Antialiased 12px concave curves bridging the active row into the plane (SVG, not hard-stop masks). */
function FuseCurves() {
  return (
    <>
      {(["top", "bottom"] as const).map((edge) => (
        <svg key={edge} className={`rk-fuse rk-fuse--${edge}`} data-rk-fuse={edge} aria-hidden="true" focusable="false" width="12" height="12" viewBox="0 0 12 12">
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

export function CaptionControls({
  adapter,
  labels = createWindowControlLabels(),
}: {
  adapter: WindowControlAdapter;
  labels?: WindowControlLabels;
}) {
  const controls = useMemo(() => createWindowControls(adapter), [adapter]);
  return (
    <div className="rk-caption">
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

export interface AppearanceMenuProps {
  value: ThemeChoice;
  onChange: (choice: ThemeChoice) => void;
  labels?: Pick<ShellLabels, "appearance" | "theme">;
}

export function AppearanceMenu({ value, onChange, labels = DEFAULT_SHELL_LABELS }: AppearanceMenuProps) {
  return (
    <details className="rk-pop">
      <summary className="rk-icon-btn" aria-label={labels.appearance} title={labels.appearance}>
        <ContrastIcon />
      </summary>
      <div className="rk-popup" role="menu">
        <span className="rk-popup__label">{labels.appearance}</span>
        {THEME_CHOICES.map((choice) => (
          <button
            type="button"
            role="menuitemradio"
            aria-checked={value === choice}
            key={choice}
            onClick={(event) => {
              onChange(choice);
              const menu = event.currentTarget.closest("details");
              if (menu) menu.open = false;
            }}
          >
            {labels.theme[choice]}
            {value === choice ? <span aria-hidden="true">✓</span> : null}
          </button>
        ))}
      </div>
    </details>
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

/** Sidebar search. Cmd/Ctrl+K focuses it; Enter opens the first match; Esc clears. */
export function JumpTo({ groups, query, onQueryChange, onNavigate, label = DEFAULT_SHELL_LABELS.jumpTo, platform }: JumpToProps) {
  const ref = useRef<HTMLInputElement | null>(null);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        ref.current?.focus();
        ref.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
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
      <Kbd chord="mod+k" platform={platform} gap={1} />
    </label>
  );
}

export interface AppShellProps {
  groups: readonly NavGroup[];
  activeId: string;
  onNavigate: (id: string) => void;
  /** Titlebar heading, usually the active view title. */
  title?: ReactNode;
  /** Right side of the titlebar (status pills, actions), left of caption controls. */
  titlebarEnd?: ReactNode;
  /** Bottom-left brand lockup. Colour the brand half with `var(--accent)`. */
  wordmark: ReactNode;
  theme: ThemeChoice;
  onThemeChange: (choice: ThemeChoice) => void;
  /** Settings is an app-owned destination; omit to hide the button. */
  onOpenSettings?: () => void;
  settingsActive?: boolean;
  /** Health / queue rows above the wordmark footer. */
  statusFooter?: ReactNode;
  /** Required on Windows/Linux to render caption buttons; macOS uses native overlay lights. */
  windowControls?: WindowControlAdapter;
  windowControlLabels?: WindowControlLabels;
  labels?: Partial<ShellLabels>;
  /** Override detection (tests, browser QA). Defaults to platform-ui. */
  platform?: Platform;
  sidebarWidth?: number;
  className?: string;
  children: ReactNode;
}

export function AppShell(props: AppShellProps) {
  const detected = usePlatform();
  const platform = props.platform ?? detected;
  const caps = platformCapabilities(platform);
  const labels: ShellLabels = { ...DEFAULT_SHELL_LABELS, ...props.labels, theme: { ...DEFAULT_SHELL_LABELS.theme, ...props.labels?.theme } };
  const [query, setQuery] = useState("");
  const visible = useMemo(() => filterNavGroups(props.groups, query), [props.groups, query]);
  const style = props.sidebarWidth ? ({ "--rk-sidebar-width": `${props.sidebarWidth}px` } as CSSProperties) : undefined;

  return (
    <div className={`rk-shell${props.className ? ` ${props.className}` : ""}`} data-platform={platform} style={style}>
      <header className="rk-top" data-tauri-drag-region>
        <div className="rk-top__side" data-tauri-drag-region>
          {/* macOS draws native traffic lights over this space (Overlay title bar). */}
          {caps.nativeTrafficLights ? <span className="rk-top__lights" data-tauri-drag-region /> : null}
        </div>
        <div className="rk-top__title" data-tauri-drag-region>{props.title}</div>
        <div className="rk-top__drag" data-tauri-drag-region />
        {props.titlebarEnd}
        {caps.customCaptionButtons && props.windowControls ? (
          <CaptionControls adapter={props.windowControls} labels={props.windowControlLabels} />
        ) : null}
      </header>

      <div className="rk-body">
        <aside className="rk-side">
          <JumpTo groups={props.groups} query={query} onQueryChange={setQuery} onNavigate={props.onNavigate} label={labels.jumpTo} platform={platform} />
          <nav className="rk-nav" aria-label={labels.sections}>
            {visible.map((group) => (
              <div className="rk-group" key={group.title}>
                <h6>{group.title}</h6>
                {group.items.map((item) => (
                  <button
                    key={item.id}
                    type="button"
                    className={`rk-item${props.activeId === item.id ? " is-active" : ""}`}
                    aria-current={props.activeId === item.id ? "page" : undefined}
                    onClick={() => props.onNavigate(item.id)}
                  >
                    {item.icon}
                    <span className="rk-item__label">{item.label}</span>
                    {item.badge ? <span className="rk-item__badge">{item.badge}</span> : null}
                    {props.activeId === item.id ? <FuseCurves /> : null}
                  </button>
                ))}
              </div>
            ))}
          </nav>
          <div className="rk-side__foot">
            {props.statusFooter ? <div className="rk-status">{props.statusFooter}</div> : null}
            <footer className="rk-brand-foot">
              <span className="rk-wordmark">{props.wordmark}</span>
              {props.onOpenSettings ? (
                <button
                  type="button"
                  className={`rk-icon-btn${props.settingsActive ? " is-open" : ""}`}
                  aria-label={labels.settings}
                  title={labels.settings}
                  onClick={props.onOpenSettings}
                >
                  <GearIcon />
                </button>
              ) : null}
              <AppearanceMenu value={props.theme} onChange={props.onThemeChange} labels={labels} />
            </footer>
          </div>
        </aside>
        <section className="rk-plane">{props.children}</section>
      </div>
    </div>
  );
}

/** Status row for `statusFooter`: a tone dot, label, and value. */
export function StatusRow({ tone, label, value }: { tone: "ok" | "warn" | "bad" | "idle"; label: string; value: string }) {
  return (
    <div className="rk-status__row">
      <span><i className={`rk-dot rk-dot--${tone}`} />{label}</span>
      <span>{value}</span>
    </div>
  );
}

/** Titlebar pill for `titlebarEnd`. */
export function Pill({ tone, children, onClick }: { tone?: "warn"; children: ReactNode; onClick?: () => void }) {
  const className = `rk-pill${tone ? ` rk-pill--${tone}` : ""}`;
  return onClick ? (
    <button type="button" className={className} onClick={onClick}>{children}</button>
  ) : (
    <span className={className}>{children}</span>
  );
}
