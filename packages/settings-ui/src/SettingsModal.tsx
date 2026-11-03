// @rightkit/settings-ui SettingsModal: the settings panel as a modal dialog with a dirty banner, Save / Discard / Reload footer
// and a close guard. The app owns the draft (its context writes the draft and reports `dirty`); this component is the chrome.
import { useEffect, useId, useState, type ReactNode } from "react";
import { Button, ModalShell } from "@rightkit/react-shell/react";
import type { SettingsBackend, SettingsSection, SettingsSnapshot } from "./types.js";
import { SettingsPanel } from "./SettingsPanel.js";

export interface SettingsModalConfirmLabels {
  /** Title of the confirmation. Default "Discard unsaved changes?". */
  title?: string;
  /** Text under the title. */
  description?: ReactNode;
  /** Default "Discard". */
  discard?: string;
  /** Default "Keep editing". */
  keep?: string;
}

export interface SettingsModalProps {
  open: boolean;
  /** Called when the dialog closes (after a confirmed discard, or a close that the guard allowed). */
  onClose: () => void;
  /** Dialog heading. Default "Settings". */
  title?: ReactNode;
  /** Sections as for `SettingsPanel` (the standard set merged with the app's own). */
  sections: readonly SettingsSection[];
  /** Persistence port for the panel; without it the panel keeps an in-memory store, as `SettingsPanel` does. */
  backend?: SettingsBackend;
  /** A snapshot already known to the caller, so the first render shows the sections. */
  initial?: SettingsSnapshot;
  /** Controlled active section id. */
  activeSection?: string;
  onActiveSectionChange?: (id: string) => void;
  /** Uncontrolled starting section. */
  defaultActiveSection?: string;
  /**
   * The draft has changes. Shows the dirty banner, enables Save and Discard, and makes a close open the discard confirmation
   * instead of closing.
   */
  dirty?: boolean;
  /** A save or reload is running: Save, Discard and Reload are disabled and the dialog cannot be closed. */
  pending?: boolean;
  /** Persist the draft. The app sets `dirty` back to false when it succeeds. */
  onSave?: () => void | Promise<void>;
  /** Drop the draft. Called by the Discard button and by a confirmed discard on close. */
  onDiscard?: () => void;
  /** Reload the stored settings over the draft. Shown only when given; disabled while `pending`. */
  onReload?: () => void | Promise<void>;
  /** Replaces the default dirty banner text. */
  banner?: ReactNode;
  /** Replaces the default footer (Reload, Discard, Save). */
  footer?: ReactNode;
  /** Wording of the discard confirmation. */
  confirmDiscard?: SettingsModalConfirmLabels;
  /**
   * Deep link: on open, `#settings=<section id>` in the URL selects that section, and each section change rewrites the hash
   * (replaceState, no navigation). Default false.
   */
  deepLinkHash?: boolean;
  /** Dialog width in pixels. Default 880. */
  width?: number;
}

const HASH_KEY = "settings";

function hashSection(sections: readonly SettingsSection[]): string | null {
  const m = /(?:^|[#&])settings=([^&]+)/.exec(window.location.hash);
  if (!m) return null;
  let id: string;
  try {
    id = decodeURIComponent(m[1]!);
  } catch {
    return null;
  }
  return sections.some((s) => s.id === id) ? id : null;
}

function writeHash(id: string): void {
  try {
    window.history.replaceState(window.history.state, "", `#${HASH_KEY}=${encodeURIComponent(id)}`);
  } catch {
    // A host that forbids history changes keeps working without the deep link.
  }
}

/**
 * The settings dialog. Built on `ModalShell` (focus trap, Escape, backdrop, focus restored to the opener) and `SettingsPanel`
 * (search, Cmd/Ctrl-F). While `dirty`, a close attempt (Escape, backdrop, the close button) asks before discarding; while
 * `pending`, the dialog cannot be closed.
 */
export function SettingsModal({
  open,
  onClose,
  title = "Settings",
  sections,
  backend,
  initial,
  activeSection,
  onActiveSectionChange,
  defaultActiveSection,
  dirty = false,
  pending = false,
  onSave,
  onDiscard,
  onReload,
  banner,
  footer,
  confirmDiscard,
  deepLinkHash = false,
  width = 880,
}: SettingsModalProps) {
  const titleId = useId();
  const confirmId = useId();
  const [inner, setInner] = useState<string | undefined>(undefined);
  const [confirming, setConfirming] = useState(false);
  const current = activeSection ?? inner ?? defaultActiveSection;

  const select = (id: string) => {
    if (activeSection === undefined) setInner(id);
    onActiveSectionChange?.(id);
    if (deepLinkHash) writeHash(id);
  };

  // Deep link: read `#settings=<id>` each time the dialog opens.
  useEffect(() => {
    if (!open) {
      setConfirming(false);
      return;
    }
    if (!deepLinkHash) return;
    const id = hashSection(sections);
    if (id !== null) select(id);
    // Only on open: later selections write the hash themselves.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // The guard is asked on every close attempt. It refuses while pending, and while dirty it opens the confirmation instead.
  const guard = (): boolean => {
    if (pending) return false;
    if (dirty) {
      setConfirming(true);
      return false;
    }
    return true;
  };

  if (!open) return null;

  const discard = () => {
    setConfirming(false);
    onDiscard?.();
    onClose();
  };

  return (
    <ModalShell label="Settings" labelledBy={titleId} onClose={onClose} width={width} dismissible={guard} className="rk-settings-modal">
      <div className="rk-settings-modal__head">
        <h2 id={titleId} className="rk-settings-modal__title">{title}</h2>
        <Button size="sm" variant="ghost" disabled={pending} onClick={() => guard() && onClose()}>Close</Button>
      </div>
      {dirty ? (
        <div className="rk-settings-modal__banner" role="status">{banner ?? "You have unsaved changes."}</div>
      ) : null}
      {confirming ? (
        <div className="rk-settings-modal__confirm" role="alertdialog" aria-labelledby={confirmId}>
          <p id={confirmId} className="rk-settings-modal__confirm-title"><strong>{confirmDiscard?.title ?? "Discard unsaved changes?"}</strong></p>
          {confirmDiscard?.description ? <p className="rk-settings-modal__confirm-text">{confirmDiscard.description}</p> : null}
          <div className="rk-sr__buttons">
            <Button size="sm" variant="secondary" onClick={() => setConfirming(false)}>{confirmDiscard?.keep ?? "Keep editing"}</Button>
            <Button size="sm" variant="danger" disabled={pending} onClick={discard}>{confirmDiscard?.discard ?? "Discard"}</Button>
          </div>
        </div>
      ) : null}
      <div className="rk-settings-modal__body">
        <SettingsPanel
          sections={sections}
          backend={backend}
          initial={initial}
          activeSection={current}
          onActiveSectionChange={select}
          navHeading={false}
          search
          findShortcut
          className="rk-settings-modal__panel"
        />
      </div>
      <div className="rk-settings-modal__foot">
        {footer ?? (
          <>
            {onReload ? (
              <Button size="sm" variant="ghost" disabled={pending} onClick={() => void onReload()}>Reload</Button>
            ) : null}
            <span className="rk-settings-modal__spacer" />
            <Button size="sm" variant="secondary" disabled={pending || !dirty} onClick={() => onDiscard?.()}>Discard</Button>
            <Button size="sm" variant="primary" disabled={pending || !dirty} onClick={() => void onSave?.()}>Save</Button>
          </>
        )}
      </div>
    </ModalShell>
  );
}
