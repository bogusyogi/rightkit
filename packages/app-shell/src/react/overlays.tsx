import { useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { useFocusTrap, useModalDismiss } from "./hooks.js";
import { Button, Input, cx } from "./ui.js";
import type { SlotClasses } from "./slots.js";

export interface ModalShellProps {
  /** Accessible name when there is no visible title. */
  label: string;
  onClose: () => void;
  width?: number;
  align?: "center" | "top";
  /** Pass false when the dialog sets its own initial focus. */
  autoFocus?: boolean;
  role?: "dialog" | "alertdialog";
  /** Close when the backdrop is clicked. Default true. */
  dismissOnBackdrop?: boolean;
  /** `id` of a visible heading inside; replaces `label` for aria-labelledby. */
  labelledBy?: string;
  describedBy?: string;
  /** Extra class on the dialog surface. */
  className?: string;
  /** `root`: extra class on the dialog surface (same as `className`). */
  slots?: SlotClasses<"root">;
  children: ReactNode;
}

/**
 * The one modal surface (ViewRight ModalShell + CodeRight DialogSurface): backdrop, Escape and
 * backdrop dismissal with an exit animation, Tab focus trap, focus restored to the opener, and a
 * width that never exceeds the viewport.
 */
export function ModalShell({ label, onClose, width = 440, align = "center", autoFocus = true, role = "dialog", dismissOnBackdrop = true, labelledBy, describedBy, className, slots, children }: ModalShellProps) {
  const ref = useRef<HTMLDivElement>(null);
  const { closing, close } = useModalDismiss(onClose, { escape: true });
  useFocusTrap(ref, true, { autoFocus });
  return (
    <div className={`rk-overlay rk-overlay--${align}${closing ? " is-closing" : ""}`} onMouseDown={(e) => e.target === e.currentTarget && dismissOnBackdrop && close()}>
      <div
        ref={ref}
        role={role}
        aria-modal="true"
        aria-label={labelledBy ? undefined : label}
        aria-labelledby={labelledBy}
        aria-describedby={describedBy}
        tabIndex={-1}
        style={{ width }}
        className={cx("rk-modal", closing && "is-closing", className, slots?.root)}
      >
        {children}
      </div>
    </div>
  );
}

export interface DialogSlots {
  root?: string;
  header?: string;
  body?: string;
  footer?: string;
}

export interface DialogProps extends Omit<ModalShellProps, "label" | "labelledBy" | "describedBy" | "children" | "slots"> {
  title: string;
  description?: ReactNode;
  children?: ReactNode;
  /** Footer buttons; right-aligned. */
  actions?: ReactNode;
  /** Extra classes per region: `root` (surface), `header` (title), `body` (wraps children), `footer` (actions). */
  slots?: SlotClasses<keyof DialogSlots>;
}

/** Titled dialog: heading, optional description, body, action row. */
export function Dialog({ title, description, children, actions, slots, ...shell }: DialogProps) {
  const titleId = useId();
  const descId = useId();
  const body = children;
  return (
    <ModalShell {...shell} slots={{ root: slots?.root }} label={title} labelledBy={titleId} describedBy={description ? descId : undefined}>
      <h2 id={titleId} className={cx("rk-modal__title", slots?.header)}>{title}</h2>
      {description ? <p id={descId} className="rk-modal__desc">{description}</p> : null}
      {slots?.body ? <div className={cx("rk-modal__body", slots.body)}>{body}</div> : body}
      {actions ? <div className={cx("rk-modal__actions", slots?.footer)}>{actions}</div> : null}
    </ModalShell>
  );
}

export interface ConfirmDialogProps {
  className?: string;
  slots?: SlotClasses<keyof DialogSlots>;
  title: string;
  description?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  /** Destructive styling; focus starts on Cancel. */
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({ title, description, confirmLabel = "Confirm", cancelLabel = "Cancel", danger, onConfirm, onCancel, className, slots }: ConfirmDialogProps) {
  return (
    <Dialog
      className={className}
      slots={slots}
      title={title}
      description={description}
      role={danger ? "alertdialog" : "dialog"}
      onClose={onCancel}
      actions={
        <>
          <Button onClick={onCancel} autoFocus={danger}>{cancelLabel}</Button>
          <Button variant={danger ? "danger" : "primary"} onClick={onConfirm} autoFocus={!danger}>{confirmLabel}</Button>
        </>
      }
      autoFocus={false}
    />
  );
}

export interface PromptDialogProps {
  className?: string;
  slots?: SlotClasses<keyof DialogSlots>;
  title: string;
  label: string;
  initialValue?: string;
  placeholder?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  validate?: (value: string) => string | null;
  onSubmit: (value: string) => void;
  onCancel: () => void;
}

/** Single-field text prompt (rename, new folder). */
export function PromptDialog({ title, label, initialValue = "", placeholder, confirmLabel = "OK", cancelLabel = "Cancel", validate, onSubmit, onCancel, className, slots }: PromptDialogProps) {
  const [value, setValue] = useState(initialValue);
  const [error, setError] = useState<string | null>(null);
  const inputId = useId();
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const problem = validate?.(value) ?? null;
    setError(problem);
    if (!problem) onSubmit(value);
  };
  return (
    <Dialog title={title} onClose={onCancel} autoFocus={false} className={className} slots={slots}>
      <form onSubmit={submit} className="rk-modal__form">
        <label htmlFor={inputId} className="rk-modal__label">{label}</label>
        <Input id={inputId} autoFocus value={value} placeholder={placeholder} aria-invalid={error ? true : undefined} onChange={(e) => setValue(e.target.value)} onFocus={(e) => e.currentTarget.select()} />
        {error ? <small className="rk-modal__error" role="alert">{error}</small> : null}
        <div className="rk-modal__actions">
          <Button onClick={onCancel}>{cancelLabel}</Button>
          <Button variant="primary" type="submit">{confirmLabel}</Button>
        </div>
      </form>
    </Dialog>
  );
}
