import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Element information emitted by the in-page picker. Page-controlled fields
 * are untrusted and are sanitized by buildDesignModePrompt before messaging. */
export interface DesignModeElement {
  tag?: string;
  id?: string;
  classes?: string;
  text?: string;
  testId?: string;
  ariaLabel?: string;
  role?: string;
  component?: string | null;
}

export interface DesignModeElementSelected {
  conversationId: string;
  element?: DesignModeElement;
  screenshot?: string | null;
  rect?: { x: number; y: number; width: number; height: number };
  [key: string]: unknown;
}

export interface DesignModePromptSubmit {
  conversationId: string;
  id: number;
  element?: DesignModeElement;
  prompt?: string;
}

export interface DesignModeSubmitResult {
  ok: boolean;
  message?: string;
}

type Unlisten = () => void;
type ListenerRegistration = Unlisten | Promise<Unlisten>;

export interface DesignModeBridge {
  enable: (conversationId: string, nonce: string) => Promise<unknown>;
  disable: (conversationId: string) => Promise<unknown>;
  signalResult: (
    conversationId: string,
    result: { id: number; ok: boolean; message?: string },
  ) => Promise<unknown>;
  onSelected?: (callback: (payload: DesignModeElementSelected) => void) => ListenerRegistration;
  onSubmit?: (callback: (payload: DesignModePromptSubmit) => void) => ListenerRegistration;
  onDismiss?: (callback: (payload: { conversationId: string }) => void) => ListenerRegistration;
}

/** Tauri IPC adapter. Browser-session injection/capture is intentionally left
 * behind these commands: it belongs to the daemon-owned embedded browser. */
export function createTauriDesignModeBridge(): DesignModeBridge {
  return {
    enable: (conversationId, nonce) =>
      invoke("browser_enable_design_mode", { conversationId, nonce }),
    disable: (conversationId) => invoke("browser_disable_design_mode", { conversationId }),
    signalResult: (conversationId, result) =>
      invoke("browser_signal_design_result", { conversationId, ...result }),
    onSelected: (callback) =>
      listen<DesignModeElementSelected>("browser-element-selected", (event) =>
        callback(event.payload),
      ),
    onSubmit: (callback) =>
      listen<DesignModePromptSubmit>("browser-element-prompt-submit", (event) =>
        callback(event.payload),
      ),
    onDismiss: (callback) =>
      listen<{ conversationId: string }>("browser-element-prompt-dismiss", (event) =>
        callback(event.payload),
      ),
  };
}

const TAURI_BRIDGE = createTauriDesignModeBridge();

export interface UseDesignModeOptions {
  conversationId: string | null;
  bridge?: DesignModeBridge;
  onPromptSubmit?: (
    payload: DesignModePromptSubmit,
  ) => Promise<DesignModeSubmitResult | void> | DesignModeSubmitResult | void;
  onDismiss?: (conversationId: string) => void;
}

export interface UseDesignMode {
  enabled: boolean;
  selected: DesignModeElementSelected | null;
  error: string | null;
  enable: () => Promise<boolean>;
  disable: () => Promise<boolean>;
  toggle: () => Promise<boolean>;
}

/**
 * Owns the renderer half of Omnigent's design-mode lifecycle: one listener set
 * per conversation, enable/disable state, and result acknowledgements. The
 * hook does not send chat messages itself; the shell supplies onPromptSubmit
 * so the normal session message path remains authoritative.
 */
export function useDesignMode({
  conversationId,
  bridge = TAURI_BRIDGE,
  onPromptSubmit,
  onDismiss,
}: UseDesignModeOptions): UseDesignMode {
  const [enabled, setEnabled] = useState(false);
  const [selected, setSelected] = useState<DesignModeElementSelected | null>(null);
  const [error, setError] = useState<string | null>(null);
  const enabledRef = useRef(false);
  const conversationRef = useRef(conversationId);
  const submitRef = useRef(onPromptSubmit);
  const dismissRef = useRef(onDismiss);
  conversationRef.current = conversationId;
  submitRef.current = onPromptSubmit;
  dismissRef.current = onDismiss;

  useEffect(() => {
    if (!conversationId) {
      setEnabled(false);
      enabledRef.current = false;
      setSelected(null);
      return;
    }

    const localConversationId = conversationId;
    let cancelled = false;
    const pending: Promise<Unlisten>[] = [];
    const cleanups: Unlisten[] = [];
    const disposed = new Set<Unlisten>();
    const dispose = (unlisten: Unlisten) => {
      if (disposed.has(unlisten)) return;
      disposed.add(unlisten);
      unlisten();
    };
    const register = (registration: ListenerRegistration) => {
      const pendingRegistration = Promise.resolve(registration);
      pending.push(pendingRegistration);
      void pendingRegistration.then((unlisten) => {
        if (cancelled) dispose(unlisten);
        else cleanups.push(unlisten);
      });
    };

    setEnabled(false);
    enabledRef.current = false;
    setSelected(null);
    register(
      bridge.onSelected?.((payload) => {
        if (payload.conversationId === localConversationId) setSelected(payload);
      }) ?? (() => {}),
    );
    register(
      bridge.onSubmit?.((payload) => {
        if (payload.conversationId !== localConversationId) return;
        const id = typeof payload.id === "number" ? payload.id : 0;
        const signal = (result: DesignModeSubmitResult) =>
          void bridge.signalResult(payload.conversationId, {
            id,
            ok: result.ok,
            message: result.message,
          });
        if (!payload.element || !payload.prompt?.trim()) {
          signal({ ok: false, message: "Missing element or prompt." });
          return;
        }
        const handler = submitRef.current;
        if (!handler) {
          signal({ ok: false, message: "No design-mode submit handler." });
          return;
        }
        void Promise.resolve(handler(payload))
          .then((result) => signal(result ?? { ok: true, message: "Sent to agent." }))
          .catch((reason: unknown) =>
            signal({ ok: false, message: `Send failed: ${String(reason)}` }),
          );
      }) ?? (() => {}),
    );
    register(
      bridge.onDismiss?.((payload) => {
        if (payload.conversationId === localConversationId) {
          dismissRef.current?.(payload.conversationId);
        }
      }) ?? (() => {}),
    );

    return () => {
      cancelled = true;
      for (const cleanup of cleanups) dispose(cleanup);
      void Promise.all(pending).then((unlisteners) => {
        if (cancelled) for (const unlisten of unlisteners) dispose(unlisten);
      });
      if (enabledRef.current) {
        enabledRef.current = false;
        void bridge.disable(localConversationId).catch(() => {});
      }
    };
  }, [bridge, conversationId]);

  const disable = useCallback(async () => {
    if (!conversationId) return false;
    setError(null);
    try {
      await bridge.disable(conversationId);
      enabledRef.current = false;
      setEnabled(false);
      setSelected(null);
      return true;
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(message);
      return false;
    }
  }, [bridge, conversationId]);

  const enable = useCallback(async () => {
    if (!conversationId) return false;
    setError(null);
    const nonce = crypto.randomUUID().replaceAll("-", "");
    try {
      await bridge.enable(conversationId, nonce);
      enabledRef.current = true;
      setEnabled(true);
      return true;
    } catch (reason) {
      const message = reason instanceof Error ? reason.message : String(reason);
      setError(message);
      return false;
    }
  }, [bridge, conversationId]);

  const toggle = useCallback(() => (enabledRef.current ? disable() : enable()), [disable, enable]);

  return { enabled, selected, error, enable, disable, toggle };
}

const FIELD_MAX = 200;
// eslint-disable-next-line no-control-regex
// biome-ignore lint/suspicious/noControlCharactersInRegex: sanitizer intentionally removes control ranges.
const CONTROL_CHARS = /[\u0000-\u001F\u007F-\u009F\u2028\u2029]/g;

/** Sanitize one untrusted page field without allowing it to forge prompt lines. */
function sanitizeField(value: string | null | undefined): string {
  if (typeof value !== "string") return "";
  return value.replace(CONTROL_CHARS, " ").replace(/\s+/g, " ").trim().slice(0, FIELD_MAX);
}

function displayName(element: DesignModeElement): string {
  const component = sanitizeField(element.component);
  if (component) return `<${component}>`;
  return `<${sanitizeField(element.tag) || "element"}>`;
}

function selectorFor(element: DesignModeElement): string {
  const testId = sanitizeField(element.testId);
  if (testId) return `[data-testid="${testId}"]`;
  const id = sanitizeField(element.id);
  if (id) return id;
  return `${sanitizeField(element.tag)}${sanitizeField(element.classes)}`;
}

/** Direct port of Omnigent's stable design-mode context block. */
export function buildDesignModePrompt(element: DesignModeElement, prompt: string): string {
  const text = sanitizeField(element.text);
  const ariaLabel = sanitizeField(element.ariaLabel);
  const role = sanitizeField(element.role);
  const context = [
    "[Design Mode — modify this element in the browser preview]",
    `Element: ${displayName(element)}`,
    `CSS selector: ${selectorFor(element)}`,
    text ? `Text: "${text}"` : "",
    ariaLabel ? `Aria-label: "${ariaLabel}"` : "",
    role ? `Role: ${role}` : "",
  ]
    .filter(Boolean)
    .join("\n");
  return `${prompt}\n\n---\n${context}\n---`;
}

/** Convert the cropped base64 image into the normal chat attachment shape. */
export function dataUrlToFile(
  dataUrl: string | null | undefined,
  filename: string,
): File | null {
  if (typeof dataUrl !== "string") return null;
  const match = /^data:([^;,]+)(;base64)?,(.*)$/s.exec(dataUrl);
  if (!match) return null;
  const mime = match[1] || "image/png";
  const rawData = match[3] ?? "";
  try {
    const raw = match[2] ? atob(rawData) : decodeURIComponent(rawData);
    const buffer = new ArrayBuffer(raw.length);
    const bytes = new Uint8Array(buffer);
    for (let index = 0; index < raw.length; index += 1) bytes[index] = raw.charCodeAt(index);
    return new File([buffer], filename, { type: mime });
  } catch {
    return null;
  }
}
