import { useId, useRef, useState, type CSSProperties, type FormEvent, type ReactNode } from "react";
import { useFocusTrap, useModalDismiss } from "./hooks.js";
import { Button, Input, cx, type ButtonProps } from "./ui.js";
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
  /**
   * Which pointer event on the backdrop dismisses. `"mousedown"` (default) closes as the button goes
   * down. `"click"` closes on click and only when both the press and the release landed on the
   * backdrop itself, so a drag that starts inside the dialog and ends on the backdrop, or starts on
   * the backdrop and ends inside, never closes it.
   */
  dismissOn?: "mousedown" | "click";
  /** `id` of a visible heading inside; replaces `label` for aria-labelledby. */
  labelledBy?: string;
  describedBy?: string;
  /** When given, the surface gets this `aria-label` and no `aria-labelledby`, even if `labelledBy` is set. */
  ariaLabel?: string;
  /** Extra class on the dialog surface. */
  className?: string;
  /** Extra class on the backdrop (same as `slots.overlay`). */
  overlayClassName?: string;
  /** Inline style on the backdrop, e.g. a different scrim colour. */
  overlayStyle?: CSSProperties;
  /** `root`: extra class on the dialog surface (same as `className`); `overlay`: extra class on the backdrop. */
  slots?: SlotClasses<"root" | "overlay">;
  children: ReactNode;
}

/**
 * The one modal surface (ViewRight ModalShell + CodeRight DialogSurface): backdrop, Escape and
 * backdrop dismissal with an exit animation, Tab focus trap, focus restored to the opener, and a
 * width that never exceeds the viewport.
 */
export function ModalShell({ label, onClose, width = 440, align = "center", autoFocus = true, role = "dialog", dismissOnBackdrop = true, dismissOn = "mousedown", labelledBy, describedBy, ariaLabel, className, overlayClassName, overlayStyle, slots, children }: ModalShellProps) {
  const ref = useRef<HTMLDivElement>(null);
  const pressedOnBackdrop = useRef(false);
  const releasedOnBackdrop = useRef(false);
  const { closing, close } = useModalDismiss(onClose, { escape: true });
  useFocusTrap(ref, true, { autoFocus });
  const named = ariaLabel !== undefined;
  const byMouseDown = dismissOn === "mousedown";
  return (
    <div
      className={cx(`rk-overlay rk-overlay--${align}${closing ? " is-closing" : ""}`, overlayClassName, slots?.overlay)}
      style={overlayStyle}
      onMouseDown={(e) => {
        if (byMouseDown) {
          if (e.target === e.currentTarget && dismissOnBackdrop) close();
        } else {
          pressedOnBackdrop.current = e.target === e.currentTarget;
        }
      }}
      onMouseUp={byMouseDown ? undefined : (e) => { releasedOnBackdrop.current = e.target === e.currentTarget; }}
      onClick={
        byMouseDown
          ? undefined
          : (e) => {
              const started = pressedOnBackdrop.current && releasedOnBackdrop.current;
              pressedOnBackdrop.current = false;
              releasedOnBackdrop.current = false;
              if (started && e.target === e.currentTarget && dismissOnBackdrop) close();
            }
      }
    >
      <div
        ref={ref}
        role={role}
        aria-modal="true"
        aria-label={named ? ariaLabel : labelledBy ? undefined : label}
        aria-labelledby={named ? undefined : labelledBy}
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
  overlay?: string;
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
    <ModalShell {...shell} slots={slots?.overlay ? { root: slots.root, overlay: slots.overlay } : { root: slots?.root }} label={title} labelledBy={titleId} describedBy={description ? descId : undefined}>
      <h2 id={titleId} className={cx("rk-modal__title", slots?.header)}>{title}</h2>
      {description ? <p id={descId} className="rk-modal__desc">{description}</p> : null}
      {slots?.body ? <div className={cx("rk-modal__body", slots.body)}>{body}</div> : body}
      {actions ? <div className={cx("rk-modal__actions", slots?.footer)}>{actions}</div> : null}
    </ModalShell>
  );
}

/** One footer button of an `ActionDialog`. */
export interface DialogAction {
  /** Stable key; defaults to the index. */
  id?: string;
  label: ReactNode;
  onClick: () => void;
  /** `Button` variant. Default `secondary`. */
  variant?: ButtonProps["variant"];
  /** Extra class on the button. */
  className?: string;
  /** Focus this button when the dialog opens. */
  autoFocus?: boolean;
  disabled?: boolean;
}

export interface ActionDialogProps extends Omit<DialogProps, "actions" | "role"> {
  /** Footer buttons, in order. Any number. */
  actions: readonly DialogAction[];
  /** Explicit role. Default `dialog`; use `alertdialog` for interruptions that need an answer. */
  role?: "dialog" | "alertdialog";
}

/**
 * Titled dialog with N footer actions (Save / Don't save / Cancel ...). `onClose` runs for Escape and
 * the backdrop. When no action sets `autoFocus`, focus starts on the first focusable element as in `Dialog`.
 */
export function ActionDialog({ actions, role = "dialog", autoFocus, ...dialog }: ActionDialogProps) {
  return (
    <Dialog
      {...dialog}
      role={role}
      autoFocus={autoFocus ?? !actions.some((a) => a.autoFocus)}
      actions={actions.map((a, i) => (
        <Button key={a.id ?? i} variant={a.variant} className={a.className} disabled={a.disabled} autoFocus={a.autoFocus} onClick={a.onClick}>{a.label}</Button>
      ))}
    />
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
  /** Explicit role. Default: `alertdialog` when `danger`, else `dialog`. */
  role?: "dialog" | "alertdialog";
  onConfirm: () => void;
  onCancel: () => void;
}

export function ConfirmDialog({ title, description, confirmLabel = "Confirm", cancelLabel = "Cancel", danger, role, onConfirm, onCancel, className, slots }: ConfirmDialogProps) {
  return (
    <ActionDialog
      className={className}
      slots={slots}
      title={title}
      description={description}
      role={role ?? (danger ? "alertdialog" : "dialog")}
      onClose={onCancel}
      actions={[
        { id: "cancel", label: cancelLabel, onClick: onCancel, autoFocus: danger },
        { id: "confirm", label: confirmLabel, onClick: onConfirm, variant: danger ? "danger" : "primary", autoFocus: !danger },
      ]}
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
