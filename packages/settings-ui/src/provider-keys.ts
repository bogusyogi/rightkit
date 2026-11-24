// Provider API keys: read which keys are saved, validate a key before saving it, save, clear, and the short
// confirmation notice. The host supplies the store as a port and every message; the key boxes are the host's.
// Imports only `react`.
import { useCallback, useEffect, useRef, useState } from "react";

/** Result of checking a key with its provider. */
export interface ProviderKeyCheck {
  /** `Rejected` blocks the save; `Unverified` (offline) saves anyway. */
  kind: "Valid" | "Rejected" | "Unverified";
}

export interface ProviderKeysPort<P extends string, S> {
  /** Which keys are saved. */
  status(): Promise<S>;
  validate(provider: P, key: string): Promise<ProviderKeyCheck>;
  /** Stores the key and returns the new status. */
  save(provider: P, key: string): Promise<S>;
  /** Removes the stored key and returns the new status. */
  clear(provider: P): Promise<S>;
}

export interface ProviderKeysMessages<P extends string> {
  /** After the provider rejected the key. */
  rejected(provider: P): string;
  /** After a save; `verified` is false when the check could not reach the provider. */
  saved(provider: P, verified: boolean): string;
  cleared(provider: P): string;
}

export interface UseProviderKeysOptions<P extends string, S> {
  providers: readonly P[];
  /** True when any key is saved, from a status. */
  isLive(status: S): boolean;
  messages: ProviderKeysMessages<P>;
  /** Reports whether any key is saved, including after the first read. */
  onLiveChange?: (live: boolean) => void;
  /** Runs on every status change (a host event for other screens, say). */
  onStatus?: (status: S) => void;
  /** Fires only after the user saved a key. */
  onSaved?: () => void;
  /** Receives every error text, and "" when it clears. */
  onKeyErrorChange?: (error: string) => void;
  /** Bump to clear a shown error. */
  clearErrorSignal?: number;
  /** How long the notice stays. Default 1800. */
  noticeMs?: number;
}

export function useProviderKeys<P extends string, S>(port: ProviderKeysPort<P, S>, options: UseProviderKeysOptions<P, S>) {
  const [status, setStatus] = useState<S | null>(null);
  const [drafts, setDrafts] = useState<Partial<Record<P, string>>>({});
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [saving, setSaving] = useState<P | null>(null);
  const portRef = useRef(port);
  portRef.current = port;
  const optionsRef = useRef(options);
  optionsRef.current = options;

  const updateError = useCallback((next: string) => {
    setError(next);
    optionsRef.current.onKeyErrorChange?.(next);
  }, []);

  const updateStatus = useCallback((next: S) => {
    setStatus(next);
    optionsRef.current.onLiveChange?.(optionsRef.current.isLive(next));
    optionsRef.current.onStatus?.(next);
  }, []);

  useEffect(() => {
    void portRef.current
      .status()
      .then(updateStatus)
      .catch((e) => updateError(String(e)));
  }, [updateStatus, updateError]);

  useEffect(() => {
    if (!notice) return;
    const timeout = window.setTimeout(() => setNotice(""), options.noticeMs ?? 1800);
    return () => window.clearTimeout(timeout);
  }, [notice, options.noticeMs]);

  const clearErrorSignal = options.clearErrorSignal;
  const errorRef = useRef(error);
  errorRef.current = error;
  useEffect(() => {
    if (!errorRef.current) return;
    updateError("");
  }, [clearErrorSignal, updateError]);

  const setDraft = useCallback((provider: P, value: string) => {
    setDrafts((current) => ({ ...current, [provider]: value }));
  }, []);

  const save = useCallback(
    async (provider: P, value: string) => {
      if (!value) return;
      updateError("");
      setNotice("");
      setSaving(provider);
      try {
        const check = await portRef.current.validate(provider, value);
        if (check.kind === "Rejected") {
          updateError(optionsRef.current.messages.rejected(provider));
          return;
        }
        const next = await portRef.current.save(provider, value);
        updateStatus(next);
        optionsRef.current.onSaved?.();
        setDrafts((current) => ({ ...current, [provider]: "" }));
        setNotice(optionsRef.current.messages.saved(provider, check.kind !== "Unverified"));
      } catch (e) {
        updateError(String(e));
      } finally {
        setSaving(null);
      }
    },
    [updateError, updateStatus],
  );

  const clear = useCallback(
    async (provider: P) => {
      updateError("");
      setNotice("");
      try {
        const next = await portRef.current.clear(provider);
        updateStatus(next);
        setNotice(optionsRef.current.messages.cleared(provider));
      } catch (e) {
        updateError(String(e));
      }
    },
    [updateError, updateStatus],
  );

  return { status, drafts, setDraft, error, notice, saving, save, clear };
}
