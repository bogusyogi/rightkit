import {
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
  type MouseEventHandler,
} from "react";
import { createPortal } from "react-dom";
import { createShortcutRegistry } from "@rightkit/react-shell";
import { isMac } from "../platform.js";
import {
  floatingPanelStyle,
  moveIndex,
  typeaheadMatch,
  useDismiss,
} from "@rightkit/platform-ui/headless";

// Native the host app toolbar/menu primitives — square, copper-on-theme, compact app UI.
// Zero eigenpal UI. Every interactive element uses onMouseDown + preventDefault so clicking
// a control never steals the document selection before the command bridge formats it.

const focusSafe =
  (fn: () => void | Promise<void>): MouseEventHandler<HTMLElement> =>
  (e) => {
    e.preventDefault();
    e.stopPropagation();
    void fn();
  };

/** Square icon/label button. Active = copper outline + tint. */
export function TBtn({
  label,
  title,
  ariaLabel,
  onRun,
  active,
  disabled,
  width,
  bold,
  italic,
  underline,
  strike,
  round,
}: {
  label: ReactNode;
  title: string;
  /** Accessible name, decoupled from `title`. Defaults to `title` for existing callers. */
  ariaLabel?: string;
  onRun: () => void | Promise<void>;
  active?: boolean;
  disabled?: boolean;
  width?: number;
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
  /** Floating-bar icon button — 34px circle (Wave 2 capsule chrome). Square 28px stays the default. */
  round?: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={ariaLabel ?? title}
      aria-pressed={active}
      title={title}
      disabled={disabled}
      onMouseDown={
        disabled
          ? undefined
          : (e) => {
              e.preventDefault();
              e.stopPropagation();
              void onRun();
            }
      }
      onClick={
        disabled
          ? undefined
          : (e) => {
              // A pointer-generated click (mouse/touch) reports detail > 0 and always
              // follows the mousedown above that already ran onRun — skip it here.
              // Keyboard (Enter/Space) and programmatic/assistive .click() report
              // detail === 0 and have no preceding mousedown, so they must run onRun.
              // Reading this off the event itself (rather than state left over from
              // mousedown) can't go stale if mousedown fires without a matching click.
              if (e.detail > 0) return;
              void onRun();
            }
      }
      style={{ width: width ?? (round ? 34 : 28) }}
      className={[
        `grid flex-shrink-0 place-items-center border text-center text-chrome-sm leading-none transition-colors ${
          round ? "h-[34px] rounded-full" : "h-7 rounded-md"
        }`,
        active
          ? "border-copper bg-copper/15 text-copper-hi"
          : "border-transparent text-muted hover:border-line hover:text-paper",
        disabled ? "cursor-not-allowed opacity-35 hover:border-transparent hover:text-muted" : "",
        bold ? "font-bold" : "",
        italic ? "italic" : "",
        underline ? "underline underline-offset-2" : "",
        strike ? "line-through" : "",
      ].join(" ")}
    >
      {label}
    </button>
  );
}

/** Vertical separator between toolbar groups. */
export function Sep() {
  return <span className="mx-1 h-5 w-px flex-shrink-0 bg-line" />;
}

/** Group wrapper. flex-shrink-0 so a scrolling toolbar (variant="scroll") scrolls the row
 *  instead of squeezing this group's controls into their neighbours. */
export function Group({ children }: { children: ReactNode }) {
  return <div className="flex flex-shrink-0 items-center gap-0.5">{children}</div>;
}

/** Focus-safe dropdown: trigger shows current value; panel renders children.
 *  Closes on outside mousedown, Esc, or when a child calls the provided close(). */
export function Dropdown({
  label,
  title,
  minWidth = 96,
  panelWidth,
  children,
  disabled,
  variant = "control",
  accelerator,
}: {
  label: ReactNode;
  title: string;
  minWidth?: number;
  panelWidth?: number;
  children: (close: () => void) => ReactNode;
  disabled?: boolean;
  /** "control" = bordered toolbar control with caret; "menu" = borderless text menu (File/Format…). */
  variant?: "control" | "menu";
  /** Single letter — Alt+<letter> opens this menu (Word-style KeyTip). */
  accelerator?: string;
}) {
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState<{ left: number; top: number; minW: number } | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const panelId = useId();
  // Mouse-opened menus leave focus (and the text selection) in the editor; only keyboard-opened
  // menus take focus, so formatting applies to what the user selected.
  const keyboardRef = useRef(false);

  const openMenu = useCallback(() => {
    const r = triggerRef.current?.getBoundingClientRect();
    if (r) setPos({ left: r.left, top: r.bottom + 2, minW: r.width });
    setOpen(true);
  }, []);
  const toggle = useCallback(() => (open ? setOpen(false) : openMenu()), [open, openMenu]);

  useLayoutEffect(() => {
    if (!open || !pos || !panelRef.current) return;
    const rect = panelRef.current.getBoundingClientRect();
    const fitted = floatingPanelStyle({
      x: pos.left,
      y: pos.top,
      width: rect.width,
      estimatedHeight: rect.height,
      viewportWidth: window.innerWidth,
      viewportHeight: window.innerHeight,
    });
    if (fitted.left !== pos.left || fitted.top !== pos.top)
      setPos({ ...pos, left: Number(fitted.left), top: Number(fitted.top) });
  }, [open, pos]);

  // Word-style menu accelerator: Alt+<letter> opens this menu (no Ctrl/Meta).
  // Disabled on macOS — Option+<letter> is used for typing special characters, not menus.
  useEffect(() => {
    if (!accelerator || disabled || isMac()) return;
    // Alt+<letter> and Alt+Shift+<letter> (the Shift variant matched before), never with Ctrl or
    // Meta: a registry scoped to this menu, bubble phase on the window. Defaults already claimed by
    // another handler still open the menu, as before.
    const letter = accelerator.toLowerCase();
    const registry = createShortcutRegistry();
    registry.register({
      id: `toolbar-menu.open.${letter}`,
      label: "Open menu",
      chord: [`alt+${letter}`, `alt+shift+${letter}`],
      hidden: true,
      allowDefaultPrevented: true,
      allowComposing: true,
      run: () => {
        keyboardRef.current = true;
        openMenu();
      },
    });
    return registry.attach(window);
  }, [accelerator, disabled, openMenu]);

  // Escape (captured on the window before a parent toolbar treats it as session cancel), outside
  // press (trigger and the portalled panel both count as inside), resize and outside scroll close the
  // menu. Escape also lands focus back on the trigger.
  useDismiss(
    (reason) => {
      setOpen(false);
      if (reason === "escape") triggerRef.current?.focus();
    },
    {
      enabled: open,
      outside: "mousedown",
      container: panelRef,
      ignore: [triggerRef],
      onResize: true,
      onScroll: true,
    },
  );

  useEffect(() => {
    if (!open) return;
    // Keyboard nav inside the open panel: arrows move, Enter/letter activate (items use
    // onMouseDown for focus-safety, so we activate by dispatching a real mousedown).
    const items = () =>
      Array.from(panelRef.current?.querySelectorAll<HTMLButtonElement>("button") ?? []);
    const activate = (el: HTMLElement) => {
      el.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, cancelable: true }));
      // Keyboard activation closes the menu and unmounts the focused item — land focus back on
      // the trigger instead of dropping it to <body>.
      if (keyboardRef.current) triggerRef.current?.focus();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Tab") {
        // The panel is portalled to <body>, so Tab order can't leave it sensibly —
        // close and let the natural focus move happen (ARIA menu pattern).
        setOpen(false);
        return;
      }
      const list = items();
      if (list.length === 0) return;
      const i = list.indexOf(document.activeElement as HTMLButtonElement);
      const isDisabled = (n: number) => !!list[n]?.disabled;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        const down = e.key === "ArrowDown";
        const next =
          i < 0
            ? down
              ? 0
              : list.length - 1
            : moveIndex(i, list.length, down ? "next" : "prev", true, isDisabled);
        list[next]?.focus();
      } else if (e.key === "Enter") {
        const el = document.activeElement as HTMLElement | null;
        if (el && panelRef.current?.contains(el)) {
          e.preventDefault();
          activate(el);
        }
      } else if (/^[a-z0-9]$/i.test(e.key) && !e.ctrlKey && !e.altKey && !e.metaKey) {
        const labels = list.map((b) => (b.textContent ?? "").trim().replace(/^✓\s*/, ""));
        const m = typeaheadMatch(labels, e.key, -1, isDisabled);
        if (m >= 0) {
          e.preventDefault();
          activate(list[m]);
        }
      }
    };
    // Menu pattern: opening moves focus to the first enabled item (matches the comment in
    // Item about item 0 being auto-focused) so arrow/type-ahead nav starts in the menu.
    if (keyboardRef.current)
      items()
        .find((b) => !b.disabled)
        ?.focus();
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [open]);

  const menu = variant === "menu";
  return (
    <div className="flex-shrink-0">
      <button
        ref={triggerRef}
        type="button"
        aria-label={title}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? panelId : undefined}
        title={title}
        disabled={disabled}
        onMouseDown={
          disabled
            ? undefined
            : focusSafe(() => {
                keyboardRef.current = false;
                toggle();
              })
        }
        onClick={
          disabled
            ? undefined
            : (e) => {
                // Same stateless contract as TBtn: a pointer follow-up click (detail > 0)
                // already ran through the mousedown above, but a detail-0 click is
                // keyboard/programmatic/native activation with no preceding mousedown and
                // must still toggle.
                if (e.detail > 0) return;
                keyboardRef.current = true;
                void toggle();
              }
        }
        style={menu ? undefined : { minWidth }}
        className={
          menu
            ? [
                "px-2 py-0.5 transition-colors",
                open ? "text-copper-hi" : "text-muted hover:text-paper",
                disabled ? "cursor-not-allowed opacity-35" : "",
              ].join(" ")
            : [
                "flex h-7 items-center justify-between gap-1.5 rounded-md border px-2 text-chrome-sm leading-none transition-colors",
                open ? "border-copper text-paper" : "border-line text-muted hover:text-paper",
                disabled ? "cursor-not-allowed opacity-35" : "",
              ].join(" ")
        }
      >
        {menu ? (
          label
        ) : (
          <>
            <span className="truncate">{label}</span>
            <span className="text-chrome-xs text-muted-2">▾</span>
          </>
        )}
      </button>
      {open &&
        pos &&
        createPortal(
          <div
            ref={panelRef}
            id={panelId}
            role="menu"
            aria-label={title}
            style={{
              position: "fixed",
              left: pos.left,
              top: pos.top,
              minWidth: Math.min(
                panelWidth ?? Math.max(minWidth, pos.minW),
                window.innerWidth - 16,
              ),
              maxWidth: "calc(100vw - 16px)",
              maxHeight: "min(18rem, calc(100vh - 16px))",
            }}
            className="no-print vr-glass-panel z-[100] max-h-72 w-max overflow-y-auto rounded-lg py-1 font-sans text-chrome-xs"
          >
            {children(() => setOpen(false))}
          </div>,
          document.body,
        )}
    </div>
  );
}

/** A selectable row inside a Dropdown panel. `hint` shows the keyboard shortcut, right-aligned. */
export function Item({
  onRun,
  active,
  children,
  style,
  hint,
  disabled,
  title,
}: {
  onRun: () => void | Promise<void>;
  active?: boolean;
  children: ReactNode;
  style?: React.CSSProperties;
  hint?: string;
  disabled?: boolean;
  /** Tooltip only — does not affect the item's accessible name (its text content). */
  title?: string;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      title={title}
      disabled={disabled}
      onMouseDown={disabled ? undefined : focusSafe(onRun)}
      onClick={
        disabled
          ? undefined
          : (e) => {
              // Same stateless contract as TBtn: a pointer follow-up click (detail > 0)
              // already ran through the mousedown above, but a detail-0 click is
              // keyboard/programmatic/native activation with no preceding mousedown and
              // must still run.
              if (e.detail > 0) return;
              void onRun();
            }
      }
      style={style}
      className={[
        // outline-none + focus-visible (not the default outline): the panel auto-focuses item 0 on
        // open, and a clipped default outline showed as a stray line. hover and keyboard focus now use
        // the SAME subtle accent wash, consistent with the active-item accent.
        "flex w-full items-center justify-between gap-4 whitespace-nowrap px-3 py-1 text-left text-chrome-sm leading-tight outline-none transition-colors",
        disabled
          ? "cursor-not-allowed opacity-40"
          : "hover:bg-copper/15 hover:text-copper-hi focus-visible:bg-copper/15 focus-visible:text-copper-hi",
        active ? "text-copper-hi" : "text-muted",
      ].join(" ")}
    >
      <span className="flex min-w-0 items-center gap-2">
        {active ? (
          <span className="shrink-0 text-chrome-xs">✓</span>
        ) : (
          <span className="w-[10px] shrink-0" />
        )}
        <span className="whitespace-nowrap">{children}</span>
      </span>
      {hint && <span className="shrink-0 whitespace-nowrap text-chrome-xs text-muted-2">{hint}</span>}
    </button>
  );
}

/** Swatch grid for text color / highlight. Colors are RRGGBB (no #). */
export function ColorGrid({
  colors,
  onPick,
  onClear,
  clearLabel = "Automatic",
}: {
  colors: { name: string; hex: string }[];
  onPick: (hex: string) => void;
  onClear?: () => void;
  clearLabel?: string;
}) {
  return (
    <div role="group" className="px-2 py-1.5">
      <div className="grid grid-cols-8 gap-1">
        {colors.map((c) => (
          <button
            key={c.hex}
            type="button"
            title={c.name}
            aria-label={c.name}
            onMouseDown={focusSafe(() => onPick(c.hex))}
            className="h-6 w-6 border border-line transition-transform hover:scale-110"
            style={{ backgroundColor: `#${c.hex}` }}
          />
        ))}
      </div>
      {onClear && (
        <button
          type="button"
          onMouseDown={focusSafe(onClear)}
          className="mt-1.5 w-full border-t border-line pt-1 text-left text-chrome-xs text-muted hover:text-paper"
        >
          {clearLabel}
        </button>
      )}
    </div>
  );
}
