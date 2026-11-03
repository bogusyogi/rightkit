import {
  forwardRef,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type ButtonHTMLAttributes,
  type CSSProperties,
  type ForwardedRef,
  type InputHTMLAttributes,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactElement,
  type ReactNode,
  type RefAttributes,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
} from "react";
import { useDeferred } from "./deferred.js";

export { Kbd } from "@rightkit/platform-ui/react";

const cx = (...parts: Array<string | false | null | undefined>) => parts.filter(Boolean).join(" ");
export { cx };

/** `data-*` attributes accepted on a component and forwarded to its root element. */
export type DataAttributes = { [key: `data-${string}`]: string | number | boolean | undefined };

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** `plain` is an unboxed row or disclosure button: left-aligned, no fill, shell hover and press washes. */
  variant?: "primary" | "secondary" | "ghost" | "danger" | "plain";
  size?: "md" | "sm";
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button({ variant = "secondary", size = "md", className, type = "button", ...rest }, ref) {
  return <button ref={ref} type={type} className={cx("rk-btn", `rk-btn--${variant}`, size === "sm" && "rk-btn--sm", className)} {...rest} />;
});

export interface IconButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** Required: icon-only controls need an accessible name (also used as the tooltip). */
  label: string;
  pressed?: boolean;
  /** A mouse press keeps focus and the text selection where they are (mousedown is prevented), for editor toolbars. */
  focusSafe?: boolean;
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton({ label, pressed, focusSafe, className, type = "button", onMouseDown, ...rest }, ref) {
  return (
    <button
      ref={ref}
      type={type}
      aria-label={label}
      title={label}
      aria-pressed={pressed}
      className={cx("rk-icon-btn", pressed && "is-open", className)}
      onMouseDown={(event) => {
        if (focusSafe) event.preventDefault();
        onMouseDown?.(event);
      }}
      {...rest}
    />
  );
});

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function Input({ className, ...rest }, ref) {
  return <input ref={ref} className={cx("rk-input", className)} {...rest} />;
});

export interface TextareaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  /** Grows with its content (no inner scrollbar) instead of staying at its `rows` height. */
  autoGrow?: boolean;
}

/** Multi-line field in the `Input` look. Native attributes, `aria-*` and `data-*` pass through; the minimum height is `--rk-textarea-min-height`. */
export const Textarea = forwardRef<HTMLTextAreaElement, TextareaProps>(function Textarea({ className, autoGrow = false, onInput, ...rest }, forwardedRef: ForwardedRef<HTMLTextAreaElement>) {
  const ref = useRef<HTMLTextAreaElement | null>(null);
  const setRef = (node: HTMLTextAreaElement | null) => {
    ref.current = node;
    if (typeof forwardedRef === "function") forwardedRef(node);
    else if (forwardedRef) forwardedRef.current = node;
  };
  const grow = () => {
    const el = ref.current;
    if (!autoGrow || !el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  };
  useLayoutEffect(() => {
    grow();
  });
  return (
    <textarea
      ref={setRef}
      className={cx("rk-input", "rk-textarea", autoGrow && "rk-textarea--grow", className)}
      onInput={(event) => {
        grow();
        onInput?.(event);
      }}
      {...rest}
    />
  );
});

export interface ToggleProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label: string;
  disabled?: boolean;
  /** Added after `rk-toggle` on the root (0.6.4). */
  className?: string;
}

/** Switch (role="switch"); the label is the accessible name. Track and knob sizes are `--rk-toggle-*` custom properties. */
export function Toggle({ checked, onChange, label, disabled, className }: ToggleProps) {
  return (
    <button type="button" role="switch" aria-checked={checked} aria-label={label} disabled={disabled} className={cx("rk-toggle", className)} onClick={() => onChange(!checked)}>
      <span className="rk-toggle__knob" />
    </button>
  );
}

export interface SegmentedOption<T extends string> {
  value: T;
  label: ReactNode;
  title?: string;
}

export function SegmentedControl<T extends string>({ value, options, onChange, label, disabled, className }: { value: T; options: readonly SegmentedOption<T>[]; onChange: (value: T) => void; label: string; disabled?: boolean; /** Added after `rk-seg` on the root (0.6.4). */ className?: string }) {
  return (
    <div className={cx("rk-seg", className)} role="radiogroup" aria-label={label}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={value === option.value}
          title={option.title}
          disabled={disabled}
          className={cx("rk-seg__item", value === option.value && "is-on")}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

export interface CardProps extends DataAttributes {
  className?: string;
  children: ReactNode;
  as?: "div" | "section" | "article";
  /** Makes the card a button (role, Tab stop, Enter and Space). The app's handler runs; nothing else changes. */
  onClick?: () => void;
  /** Accessible name, needed when the card is interactive and its text is not a useful name. */
  label?: string;
  /** Remove the inner padding (media or a full-bleed list inside the card). */
  flush?: boolean;
  /** Highlights the card. With `onClick` it is also exposed as `aria-pressed`. */
  selected?: boolean;
  /** Marks the card as the current item of a list or calendar (`aria-current="true"`), highlighted like `selected`. Navigation state, not a toggle. */
  current?: boolean;
  /** Native tooltip and hover title. */
  title?: string;
  /** Media slot above the body (thumbnail, cover or preview). Full-bleed at the top of the card. */
  media?: ReactNode;
  /** Header strip. With `onHeaderClick` it is one real button and the body stays outside it. */
  header?: ReactNode;
  onHeaderClick?: () => void;
  /** Accessible name of the header button, when its text is not a useful name. */
  headerLabel?: string;
}

export function Card({ className, children, as: Tag = "div", onClick, label, flush, selected, current, title, media, header, onHeaderClick, headerLabel, ...rest }: CardProps) {
  const interactive = onClick !== undefined;
  const slotted = media !== undefined || header !== undefined;
  const onKeyDown = interactive
    ? (event: ReactKeyboardEvent<HTMLElement>) => {
        // Keys from a nested control belong to that control.
        if (event.target !== event.currentTarget) return;
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onClick?.();
        }
      }
    : undefined;
  const headerNode =
    header === undefined ? null : onHeaderClick ? (
      <button type="button" className="rk-card__header rk-card__header--button" aria-label={headerLabel} onClick={onHeaderClick}>
        {header}
      </button>
    ) : (
      <div className="rk-card__header">{header}</div>
    );
  return (
    <Tag
      {...rest}
      className={cx("rk-card", interactive && "rk-card--interactive", flush && "is-flush", slotted && "is-slotted", (selected || current) && "is-selected", className)}
      role={interactive ? "button" : undefined}
      tabIndex={interactive ? 0 : undefined}
      aria-label={label}
      aria-pressed={interactive && selected !== undefined ? selected : undefined}
      aria-current={current ? "true" : undefined}
      title={title}
      onClick={onClick}
      onKeyDown={onKeyDown}
    >
      {headerNode}
      {media !== undefined ? <div className="rk-card__media">{media}</div> : null}
      {slotted ? <div className="rk-card__body">{children}</div> : children}
    </Tag>
  );
}

/** Placeholder block for loading content. Decorative: pair it with a `SkeletonRows` status or a visible "Loading" label. */
export interface SkeletonProps {
  width?: number | string;
  height?: number | string;
  radius?: number | string;
  className?: string;
  style?: CSSProperties;
}

export function Skeleton({ width, height = 12, radius, className, style }: SkeletonProps) {
  return <span aria-hidden="true" className={cx("rk-skeleton", className)} style={{ width, height, borderRadius: radius, ...style }} />;
}

/** A column of skeleton lines announced as a busy status. The last line is shorter, as text usually is. `delayMs` renders nothing until that time has passed, so a fast load never flashes. */
export function SkeletonRows({ rows = 3, label = "Loading", className, delayMs = 0 }: { rows?: number; label?: string; className?: string; delayMs?: number }) {
  const shown = useDeferred(true, delayMs);
  if (delayMs > 0 && !shown) return null;
  return (
    <div role="status" aria-label={label} aria-busy="true" className={cx("rk-skeleton-rows", className)}>
      {Array.from({ length: Math.max(1, rows) }, (_, i) => (
        <Skeleton key={i} width={i === Math.max(1, rows) - 1 ? "60%" : "100%"} />
      ))}
    </div>
  );
}

/** Checkbox with a visible box and a label. `indeterminate` shows the mixed state (set on the input, not an attribute). */
export interface CheckboxProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  label?: ReactNode;
  disabled?: boolean;
  indeterminate?: boolean;
  className?: string;
  /** Set on the input. Use it when the visible label is elsewhere (a grid cell, a status line) or does not name the control. */
  id?: string;
  "aria-label"?: string;
  "aria-labelledby"?: string;
  /** `start` puts the label before the box. The input stays first in the DOM. */
  labelPosition?: "end" | "start";
}

export function Checkbox({ checked, onChange, label, disabled, indeterminate = false, className, id, "aria-label": ariaLabel, "aria-labelledby": ariaLabelledBy, labelPosition = "end" }: CheckboxProps) {
  const labelNode = label !== undefined ? <span className="rk-check__label">{label}</span> : null;
  return (
    <label className={cx("rk-check", disabled && "is-disabled", className)}>
      <input
        ref={(el) => {
          if (el) el.indeterminate = indeterminate;
        }}
        id={id}
        type="checkbox"
        className="rk-check__input"
        aria-label={ariaLabel}
        aria-labelledby={ariaLabelledBy}
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      {labelPosition === "start" ? labelNode : null}
      <span className="rk-check__box" aria-hidden="true">{indeterminate ? "–" : checked ? "✓" : ""}</span>
      {labelPosition === "start" ? null : labelNode}
    </label>
  );
}

/** Radio button with an optional description. The native input is underneath, so arrow keys and group announcement work as usual. */
export interface RadioProps {
  name: string;
  value: string;
  checked: boolean;
  onChange: () => void;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
  className?: string;
}

export function Radio({ name, value, checked, onChange, label, description, disabled, className }: RadioProps) {
  const descriptionId = useId();
  return (
    <label className={cx("rk-radio", disabled && "is-disabled", className)}>
      <input
        type="radio"
        className="rk-radio__input"
        name={name}
        value={value}
        checked={checked}
        disabled={disabled}
        aria-describedby={description ? descriptionId : undefined}
        onChange={() => onChange()}
      />
      <span className="rk-radio__box" aria-hidden="true" />
      <span className="rk-radio__text">
        <span className="rk-radio__label">{label}</span>
        {description ? <span className="rk-radio__desc" id={descriptionId}>{description}</span> : null}
      </span>
    </label>
  );
}

export interface RadioOption<T extends string> {
  value: T;
  label: ReactNode;
  description?: ReactNode;
  disabled?: boolean;
}

export interface RadioGroupProps<T extends string> {
  /** Group name, announced with the fieldset. */
  legend: ReactNode;
  /** The chosen value, or undefined when nothing is chosen. */
  value: T | undefined;
  options: readonly RadioOption<T>[];
  onChange: (value: T) => void;
  /** Shared name for the native radios; generated when omitted. */
  name?: string;
  disabled?: boolean;
  /** Group-level description or error, referenced from the fieldset. */
  "aria-describedby"?: string;
  className?: string;
}

export function RadioGroup<T extends string>({ legend, value, options, onChange, name, disabled, "aria-describedby": describedBy, className }: RadioGroupProps<T>) {
  const generatedName = useId();
  const groupName = name ?? generatedName;
  return (
    <fieldset className={cx("rk-radio-group", className)} disabled={disabled} aria-describedby={describedBy}>
      <legend className="rk-radio-group__legend">{legend}</legend>
      {options.map((option) => (
        <Radio
          key={option.value}
          name={groupName}
          value={option.value}
          checked={value === option.value}
          disabled={option.disabled}
          label={option.label}
          description={option.description}
          onChange={() => onChange(option.value)}
        />
      ))}
    </fieldset>
  );
}

/** Native select in the shell's look. Option labels are strings (the browser renders `option` text only). Options may be grouped with `SelectGroup`. */
export interface SelectOption<T extends string> {
  value: T;
  label: string;
  disabled?: boolean;
}

/** A labelled `optgroup` of options. */
export interface SelectGroup<T extends string> {
  label: string;
  options: readonly SelectOption<T>[];
  disabled?: boolean;
}

export interface SelectProps<T extends string> extends Omit<SelectHTMLAttributes<HTMLSelectElement>, "value" | "defaultValue" | "onChange" | "children">, DataAttributes {
  value: T;
  options: readonly (SelectOption<T> | SelectGroup<T>)[];
  onChange: (value: T) => void;
  label: string;
}

const SelectBase = forwardRef<HTMLSelectElement, SelectProps<string>>(function Select({ value, options, onChange, label, className, ...rest }, ref) {
  return (
    <select {...rest} ref={ref} className={cx("rk-select", className)} aria-label={label} value={value} onChange={(event) => onChange(event.target.value)}>
      {options.map((option) =>
        "options" in option ? (
          <optgroup key={option.label} label={option.label} disabled={option.disabled}>
            {option.options.map((item) => (
              <option key={item.value} value={item.value} disabled={item.disabled}>{item.label}</option>
            ))}
          </optgroup>
        ) : (
          <option key={option.value} value={option.value} disabled={option.disabled}>{option.label}</option>
        ),
      )}
    </select>
  );
});

/** Forwards its ref and all native attributes (`id`, `name`, `tabIndex`, `onBlur`, `aria-*`, `data-*`) to the `select`. */
export const Select = SelectBase as unknown as <T extends string>(props: SelectProps<T> & RefAttributes<HTMLSelectElement>) => ReactElement | null;

/** Native range input in the shell's look. `format` supplies the spoken value (for example "42 percent"). */
export interface SliderProps extends DataAttributes {
  value: number;
  onChange: (value: number) => void;
  label: string;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
  format?: (value: number) => string;
  className?: string;
  id?: string;
  /** A gesture starts on pointer down, or on the first key change. `value` is the value before the gesture. */
  onChangeStart?: (value: number) => void;
  /** A gesture ends on pointer up, or after `settleMs` idle for keys. `value` is the final value. */
  onChangeEnd?: (value: number) => void;
  /** Escape during a gesture. `value` is the start value; the slider also reports it through `onChange` so the control restores. */
  onChangeCancel?: (value: number) => void;
  /** Idle time before a key-driven gesture settles, in milliseconds. Default 400. */
  settleMs?: number;
}

export function Slider({ value, onChange, label, min = 0, max = 100, step = 1, disabled, format, className, id, onChangeStart, onChangeEnd, onChangeCancel, settleMs = 400, ...rest }: SliderProps) {
  const gesture = useRef<{ start: number; latest: number; pointer: boolean } | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearTimer = () => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
      timer.current = null;
    }
  };
  useEffect(() => () => clearTimer(), []);

  const beginGesture = (pointer: boolean) => {
    if (gesture.current === null) {
      gesture.current = { start: value, latest: value, pointer };
      onChangeStart?.(value);
    } else if (pointer) {
      gesture.current.pointer = true;
      clearTimer();
    }
  };
  const endGesture = () => {
    clearTimer();
    const active = gesture.current;
    if (active === null) return;
    gesture.current = null;
    onChangeEnd?.(active.latest);
  };
  const cancelGesture = () => {
    clearTimer();
    const active = gesture.current;
    if (active === null) return;
    gesture.current = null;
    onChange(active.start);
    onChangeCancel?.(active.start);
  };

  return (
    <input
      {...rest}
      id={id}
      type="range"
      className={cx("rk-slider", className)}
      aria-label={label}
      min={min}
      max={max}
      step={step}
      value={value}
      disabled={disabled}
      aria-valuetext={format ? format(value) : undefined}
      onPointerDown={() => beginGesture(true)}
      onPointerUp={() => {
        if (gesture.current?.pointer) endGesture();
      }}
      onPointerCancel={() => {
        if (gesture.current?.pointer) endGesture();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape" && gesture.current !== null) {
          event.preventDefault();
          cancelGesture();
        }
      }}
      onChange={(event) => {
        const next = Number(event.target.value);
        beginGesture(false);
        if (gesture.current !== null) gesture.current.latest = next;
        onChange(next);
        if (gesture.current !== null && !gesture.current.pointer) {
          clearTimer();
          timer.current = setTimeout(endGesture, settleMs);
        }
      }}
    />
  );
}

/** Inline notice. `warn` and `bad` announce as alerts; the others as status. With `onClick` the whole notice is one button; a control in `action` keeps its own events. */
export interface NoticeProps extends DataAttributes {
  tone?: "info" | "ok" | "warn" | "bad";
  title?: ReactNode;
  children?: ReactNode;
  /** A button or link at the end of the notice. */
  action?: ReactNode;
  className?: string;
  /** Makes the whole notice one button (role, Tab stop, Enter and Space). */
  onClick?: () => void;
}

export function Notice({ tone = "info", title, children, action, className, onClick, ...rest }: NoticeProps) {
  const actionRef = useRef<HTMLDivElement | null>(null);
  const interactive = onClick !== undefined;
  const onKeyDown = interactive
    ? (event: ReactKeyboardEvent<HTMLDivElement>) => {
        // Keys from a nested control belong to that control.
        if (event.target !== event.currentTarget) return;
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onClick?.();
        }
      }
    : undefined;
  const onNoticeClick = interactive
    ? (event: ReactMouseEvent<HTMLDivElement>) => {
        // A click inside the action keeps the action's own events.
        if (actionRef.current?.contains(event.target as Node)) return;
        onClick?.();
      }
    : undefined;
  return (
    <div
      {...rest}
      role={interactive ? "button" : tone === "bad" || tone === "warn" ? "alert" : "status"}
      tabIndex={interactive ? 0 : undefined}
      className={cx("rk-notice", `rk-notice--${tone}`, interactive && "rk-notice--interactive", className)}
      onClick={onNoticeClick}
      onKeyDown={onKeyDown}
    >
      <div className="rk-notice__body">
        {title ? <strong className="rk-notice__title">{title}</strong> : null}
        {children ? <span className="rk-notice__text">{children}</span> : null}
      </div>
      {action ? <div className="rk-notice__action" ref={actionRef}>{action}</div> : null}
    </div>
  );
}

/** Progress bar with a tone. `value` is clamped to 0..max. Omit `value` for an indeterminate bar (work is running, no figure yet). */
export interface ProgressBarProps {
  value?: number;
  max?: number;
  tone?: "accent" | "ok" | "warn" | "bad";
  label: string;
  className?: string;
}

export function ProgressBar({ value, max = 100, tone = "accent", label, className }: ProgressBarProps) {
  if (value === undefined) {
    return (
      <div role="progressbar" aria-label={label} aria-busy="true" className={cx("rk-progress", "rk-progress--indeterminate", `rk-progress--${tone}`, className)}>
        <span className="rk-progress__fill" />
      </div>
    );
  }
  const pct = max > 0 ? Math.min(100, Math.max(0, (value / max) * 100)) : 0;
  return (
    <div role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={max} aria-valuenow={Math.round(Math.min(max, Math.max(0, value)))} className={cx("rk-progress", `rk-progress--${tone}`, className)}>
      <span className="rk-progress__fill" style={{ width: `${pct}%` }} />
    </div>
  );
}

export function Badge({ tone = "neutral", children }: { tone?: "neutral" | "accent" | "ok" | "warn" | "bad"; children: ReactNode }) {
  return <span className={cx("rk-badge", `rk-badge--${tone}`)}>{children}</span>;
}

export function EmptyState({ icon, title, children, action }: { icon?: ReactNode; title: ReactNode; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="rk-empty">
      {icon ? <span className="rk-empty__icon" aria-hidden="true">{icon}</span> : null}
      <strong>{title}</strong>
      {children ? <p>{children}</p> : null}
      {action}
    </div>
  );
}

/** Label + control + optional note and error, wired together for screen readers. The error is `role="alert"`. */
export function Field({ label, note, error, children }: { label: ReactNode; note?: ReactNode; error?: ReactNode; children: (props: { id: string; "aria-describedby"?: string; "aria-invalid"?: true }) => ReactNode }) {
  const id = useId();
  const noteId = `${id}-note`;
  const errorId = `${id}-error`;
  const describedBy = [note ? noteId : null, error ? errorId : null].filter(Boolean).join(" ") || undefined;
  return (
    <div className="rk-field">
      <label htmlFor={id}>{label}</label>
      {children({ id, "aria-describedby": describedBy, ...(error ? { "aria-invalid": true as const } : {}) })}
      {note ? <small id={noteId}>{note}</small> : null}
      {error ? <div className="rk-field__error" id={errorId} role="alert">{error}</div> : null}
    </div>
  );
}

/** A row of controls that wraps. */
export function Toolbar({ children, label, className }: { children: ReactNode; label?: string; className?: string }) {
  return <div role={label ? "toolbar" : undefined} aria-label={label} className={cx("rk-toolbar", className)}>{children}</div>;
}

/** A vertical stack of fields and sections with the shell's spacing. */
export function FormStack({ children, className }: { children: ReactNode; className?: string }) {
  return <div className={cx("rk-form-stack", className)}>{children}</div>;
}

/** A bordered group of related controls with a legend. */
export function Fieldset({ legend, children, className, disabled }: { legend: ReactNode; children: ReactNode; className?: string; disabled?: boolean }) {
  return (
    <fieldset className={cx("rk-fieldset", className)} disabled={disabled}>
      <legend className="rk-fieldset__legend">{legend}</legend>
      {children}
    </fieldset>
  );
}

/** Secondary help text under a control or section. */
export function Hint({ children, className }: { children: ReactNode; className?: string }) {
  return <p className={cx("rk-hint", className)}>{children}</p>;
}

/** Section heading inside a page. */
export function SectionTitle({ children, as: Tag = "h2", className }: { children: ReactNode; as?: "h2" | "h3" | "h4"; className?: string }) {
  return <Tag className={cx("rk-section-title", className)}>{children}</Tag>;
}

/** Small uppercase label above a heading or group. */
export function Kicker({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cx("rk-kicker", className)}>{children}</span>;
}

/** Plain-text block: `raw` (preformatted, monospace), `caption` (prose) or `digest` (inline monospace, breaks long values). With `maxHeight` it scrolls; while it overflows it is a labelled, focusable region. */
export interface TextBlockProps extends DataAttributes {
  variant?: "raw" | "caption" | "digest";
  maxHeight?: number | string;
  /** Accessible name of the scroll region. Used only while the block overflows. */
  label?: string;
  className?: string;
  children: ReactNode;
}

export function TextBlock({ variant = "caption", maxHeight, label, className, children, ...rest }: TextBlockProps) {
  const ref = useRef<HTMLElement | null>(null);
  const [overflowing, setOverflowing] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el || maxHeight === undefined) {
      setOverflowing(false);
      return undefined;
    }
    const measure = () => setOverflowing(el.scrollHeight > el.clientHeight);
    measure();
    if (typeof ResizeObserver === "undefined") return undefined;
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  }, [maxHeight, children]);
  const scrollable = maxHeight !== undefined && overflowing;
  const setRef = (node: HTMLElement | null) => {
    ref.current = node;
  };
  const Tag = variant === "raw" ? "pre" : variant === "digest" ? "span" : "div";
  return (
    <Tag
      {...rest}
      ref={setRef}
      className={cx("rk-text", `rk-text--${variant}`, className)}
      style={maxHeight !== undefined ? { maxHeight, overflow: "auto" } : undefined}
      role={scrollable ? "region" : undefined}
      aria-label={scrollable ? label : undefined}
      tabIndex={scrollable ? 0 : undefined}
    >
      {children}
    </Tag>
  );
}

/** One label/value pair per row, rendered as a `dl`. `dense` tightens the rows; `inline` runs each pair on one wrapping line. */
export interface DescriptionItem {
  label: ReactNode;
  value: ReactNode;
}

export interface DescriptionListProps extends DataAttributes {
  items: readonly DescriptionItem[];
  dense?: boolean;
  inline?: boolean;
  className?: string;
}

export function DescriptionList({ items, dense, inline, className, ...rest }: DescriptionListProps) {
  return (
    <dl {...rest} className={cx("rk-dl", dense && "rk-dl--dense", inline && "rk-dl--inline", className)}>
      {items.map((item, index) => (
        <div key={index} className="rk-dl__row">
          <dt>{item.label}</dt>
          <dd>{item.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** A single label/value pair, as its own `dl`. */
export interface KeyValueProps extends DescriptionItem {
  dense?: boolean;
  inline?: boolean;
  className?: string;
}

export function KeyValue({ label, value, dense, inline, className }: KeyValueProps) {
  return <DescriptionList items={[{ label, value }]} dense={dense} inline={inline} className={className} />;
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
