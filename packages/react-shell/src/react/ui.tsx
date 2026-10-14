import { forwardRef, useId, type ButtonHTMLAttributes, type InputHTMLAttributes, type ReactNode } from "react";

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

export function Card({ className, children, as: Tag = "div" }: { className?: string; children: ReactNode; as?: "div" | "section" | "article" }) {
  return <Tag className={cx("rk-card", className)}>{children}</Tag>;
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
