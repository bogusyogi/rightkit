import { useId, useRef, useState, type FormEvent, type ReactNode } from "react";
import { useFocusTrap, useModalDismiss } from "./hooks.js";
import { Button, Input } from "./ui.js";

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
  children: ReactNode;
}

/**
 * The one modal surface (ViewRight ModalShell + CodeRight DialogSurface): backdrop, Escape and
 * backdrop dismissal with an exit animation, Tab focus trap, focus restored to the opener, and a
 * width that never exceeds the viewport.
 */
export function ModalShell({ label, onClose, width = 440, align = "center", autoFocus = true, role = "dialog", dismissOnBackdrop = true, labelledBy, describedBy, children }: ModalShellProps) {
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
        className={`rk-modal${closing ? " is-closing" : ""}`}
      >
        {children}
      </div>
    </div>
  );
}

export interface DialogProps extends Omit<ModalShellProps, "label" | "labelledBy" | "describedBy" | "children"> {
  title: string;
  description?: ReactNode;
  children?: ReactNode;
  /** Footer buttons; right-aligned. */
  actions?: ReactNode;
}

/** Titled dialog: heading, optional description, body, action row. */
export function Dialog({ title, description, children, actions, ...shell }: DialogProps) {
  const titleId = useId();
  const descId = useId();
  return (
    <ModalShell {...shell} label={title} labelledBy={titleId} describedBy={description ? descId : undefined}>
      <h2 id={titleId} className="rk-modal__title">{title}</h2>
      {description ? <p id={descId} className="rk-modal__desc">{description}</p> : null}
      {children}
      {actions ? <div className="rk-modal__actions">{actions}</div> : null}
    </ModalShell>
  );
}

export interface ConfirmDialogProps {
  title: string;
  description?: ReactNode;
  confirmLabel?: string;
  cancelLabel?: string;
  /** Destructive styling; focus starts on Cancel. */
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({ title, description, confirmLabel = "Confirm", cancelLabel = "Cancel", danger, onConfirm, onCancel }: ConfirmDialogProps) {
  return (
    <Dialog
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
export function PromptDialog({ title, label, initialValue = "", placeholder, confirmLabel = "OK", cancelLabel = "Cancel", validate, onSubmit, onCancel }: PromptDialogProps) {
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
    <Dialog title={title} onClose={onCancel} autoFocus={false}>
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
