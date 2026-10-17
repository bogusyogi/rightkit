import { forwardRef, useId, type ButtonHTMLAttributes, type CSSProperties, type InputHTMLAttributes, type KeyboardEvent as ReactKeyboardEvent, type ReactNode } from "react";

export { Kbd } from "@rightkit/platform-ui/react";

const cx = (...parts: Array<string | false | null | undefined>) => parts.filter(Boolean).join(" ");
export { cx };

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: "primary" | "secondary" | "ghost" | "danger";
  size?: "md" | "sm";
}

export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button({ variant = "secondary", size = "md", className, type = "button", ...rest }, ref) {
  return <button ref={ref} type={type} className={cx("rk-btn", `rk-btn--${variant}`, size === "sm" && "rk-btn--sm", className)} {...rest} />;
});

export interface IconButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  /** Required: icon-only controls need an accessible name (also used as the tooltip). */
  label: string;
  pressed?: boolean;
}

export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton({ label, pressed, className, type = "button", ...rest }, ref) {
  return <button ref={ref} type={type} aria-label={label} title={label} aria-pressed={pressed} className={cx("rk-icon-btn", pressed && "is-open", className)} {...rest} />;
});

export const Input = forwardRef<HTMLInputElement, InputHTMLAttributes<HTMLInputElement>>(function Input({ className, ...rest }, ref) {
  return <input ref={ref} className={cx("rk-input", className)} {...rest} />;
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

export interface CardProps {
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
}

export function Card({ className, children, as: Tag = "div", onClick, label, flush, selected }: CardProps) {
  const interactive = onClick !== undefined;
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
  return (
    <Tag
      className={cx("rk-card", interactive && "rk-card--interactive", flush && "is-flush", selected && "is-selected", className)}
      role={interactive ? "button" : undefined}
      tabIndex={interactive ? 0 : undefined}
      aria-label={label}
      aria-pressed={interactive && selected !== undefined ? selected : undefined}
      onClick={onClick}
      onKeyDown={onKeyDown}
    >
      {children}
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

/** A column of skeleton lines announced as a busy status. The last line is shorter, as text usually is. */
export function SkeletonRows({ rows = 3, label = "Loading", className }: { rows?: number; label?: string; className?: string }) {
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
  label: ReactNode;
  disabled?: boolean;
  indeterminate?: boolean;
  className?: string;
}

export function Checkbox({ checked, onChange, label, disabled, indeterminate = false, className }: CheckboxProps) {
  return (
    <label className={cx("rk-check", disabled && "is-disabled", className)}>
      <input
        ref={(el) => {
          if (el) el.indeterminate = indeterminate;
        }}
        type="checkbox"
        className="rk-check__input"
        checked={checked}
        disabled={disabled}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span className="rk-check__box" aria-hidden="true">{indeterminate ? "–" : checked ? "✓" : ""}</span>
      <span className="rk-check__label">{label}</span>
    </label>
  );
}

/** Native select in the shell's look. Option labels are strings (the browser renders `option` text only). */
export interface SelectOption<T extends string> {
  value: T;
  label: string;
  disabled?: boolean;
}

export function Select<T extends string>({ value, options, onChange, label, disabled, className }: { value: T; options: readonly SelectOption<T>[]; onChange: (value: T) => void; label: string; disabled?: boolean; className?: string }) {
  return (
    <select className={cx("rk-select", className)} aria-label={label} value={value} disabled={disabled} onChange={(event) => onChange(event.target.value as T)}>
      {options.map((option) => (
        <option key={option.value} value={option.value} disabled={option.disabled}>{option.label}</option>
      ))}
    </select>
  );
}

/** Native range input in the shell's look. `format` supplies the spoken value (for example "42 percent"). */
export interface SliderProps {
  value: number;
  onChange: (value: number) => void;
  label: string;
  min?: number;
  max?: number;
  step?: number;
  disabled?: boolean;
  format?: (value: number) => string;
  className?: string;
}

export function Slider({ value, onChange, label, min = 0, max = 100, step = 1, disabled, format, className }: SliderProps) {
  return (
    <input
      type="range"
      className={cx("rk-slider", className)}
      aria-label={label}
      min={min}
      max={max}
      step={step}
      value={value}
      disabled={disabled}
      aria-valuetext={format ? format(value) : undefined}
      onChange={(event) => onChange(Number(event.target.value))}
    />
  );
}

/** Inline notice. `warn` and `bad` announce as alerts; the others as status. */
export interface NoticeProps {
  tone?: "info" | "ok" | "warn" | "bad";
  title?: ReactNode;
  children?: ReactNode;
  /** A button or link at the end of the notice. */
  action?: ReactNode;
  className?: string;
}

export function Notice({ tone = "info", title, children, action, className }: NoticeProps) {
  return (
    <div role={tone === "bad" || tone === "warn" ? "alert" : "status"} className={cx("rk-notice", `rk-notice--${tone}`, className)}>
      <div className="rk-notice__body">
        {title ? <strong className="rk-notice__title">{title}</strong> : null}
        {children ? <span className="rk-notice__text">{children}</span> : null}
      </div>
      {action ? <div className="rk-notice__action">{action}</div> : null}
    </div>
  );
}

/** Determinate progress bar with a tone. `value` is clamped to 0..max. */
export interface ProgressBarProps {
  value: number;
  max?: number;
  tone?: "accent" | "ok" | "warn" | "bad";
  label: string;
  className?: string;
}

export function ProgressBar({ value, max = 100, tone = "accent", label, className }: ProgressBarProps) {
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

/** Label + control + optional note, wired together for screen readers. */
export function Field({ label, note, children }: { label: ReactNode; note?: ReactNode; children: (props: { id: string; "aria-describedby"?: string }) => ReactNode }) {
  const id = useId();
  const noteId = `${id}-note`;
  return (
    <div className="rk-field">
      <label htmlFor={id}>{label}</label>
      {children({ id, "aria-describedby": note ? noteId : undefined })}
      {note ? <small id={noteId}>{note}</small> : null}
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
