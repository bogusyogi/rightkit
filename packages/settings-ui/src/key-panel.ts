// Disclosure state for a provider-key panel that sits under a feature's settings row: flipping the feature on with no saved
// key opens the panel, and the feature turns on only after that panel saves a key. Opening the panel to manage keys never
// turns the feature on. Imports only `react`.
import { useCallback, useEffect, useRef, useState, type RefObject } from "react";

export interface UseKeyPanelDisclosureOptions {
  /** A usable key is saved (for example the `onLiveChange` value of `useProviderKeys`). */
  hasKey: boolean;
  /** Turns the feature on or off (the host's settings write). */
  setEnabled(next: boolean): void;
  /** Selector of the field that takes the caret when the panel opens. Default `"input"`; `null` skips focusing. */
  focusSelector?: string | null;
}

export interface KeyPanelDisclosure<T extends HTMLElement = HTMLDivElement> {
  /** The panel is open. */
  open: boolean;
  /** Attach to the panel's element: it is scrolled into view (nearest) and its field focused on the next frame after opening. */
  panelRef: RefObject<T | null>;
  /** The feature toggle's handler. On with no saved key opens the panel and defers enabling; otherwise `setEnabled(next)`. */
  onToggle(next: boolean): void;
  /** The "add / manage key" button's handler: flips the panel and cancels any deferred enabling. */
  togglePanel(): void;
  /** Pass to the key form's save callback: enables the feature and closes the panel only when the toggle opened it. */
  onSaved(): void;
  /** Closes the panel and cancels any deferred enabling. */
  close(): void;
}

/** See the module comment. */
export function useKeyPanelDisclosure<T extends HTMLElement = HTMLDivElement>(options: UseKeyPanelDisclosureOptions): KeyPanelDisclosure<T> {
  const { hasKey, setEnabled, focusSelector = "input" } = options;
  const [open, setOpen] = useState(false);
  // Only the toggle-on path enables the feature after a save.
  const enableOnSaveRef = useRef(false);
  const panelRef = useRef<T | null>(null);
  const hasKeyRef = useRef(hasKey);
  hasKeyRef.current = hasKey;
  const setEnabledRef = useRef(setEnabled);
  setEnabledRef.current = setEnabled;

  useEffect(() => {
    if (!open) return;
    const id = window.requestAnimationFrame(() => {
      const panel = panelRef.current;
      if (!panel) return;
      panel.scrollIntoView({ block: "nearest" });
      if (focusSelector) panel.querySelector<HTMLElement>(focusSelector)?.focus();
    });
    return () => window.cancelAnimationFrame(id);
  }, [open, focusSelector]);

  const onToggle = useCallback((next: boolean) => {
    if (next && !hasKeyRef.current) {
      enableOnSaveRef.current = true;
      setOpen(true);
      return;
    }
    setEnabledRef.current(next);
  }, []);
  const togglePanel = useCallback(() => {
    enableOnSaveRef.current = false;
    setOpen((value) => !value);
  }, []);
  const onSaved = useCallback(() => {
    if (!enableOnSaveRef.current) return;
    enableOnSaveRef.current = false;
    setEnabledRef.current(true);
    setOpen(false);
  }, []);
  const close = useCallback(() => {
    enableOnSaveRef.current = false;
    setOpen(false);
  }, []);

  return { open, panelRef, onToggle, togglePanel, onSaved, close };
}
