// Per-sender / per-domain remote-image trust rules. Extracted from App (M5).
// Reads the latest maps via the store at call time, so the callbacks are stable
// (no deps) and the view that uses them stays prop-light.
import { useCallback } from "react";
import { useAppStore } from "../store";
import { setPref } from "../lib/preferences";
import { resolveImageRule, emailDomain, type ImageRule } from "../lib/imageTrust";

function applyRule(rule: ImageRule | null) {
  const s = useAppStore.getState();
  if (rule === "show") s.setShowImages(true);
  else if (rule === "block") s.setShowImages(false);
  else s.setShowImages(s.imagePolicy);
}

export function useImageTrust() {
  /** Set (or clear) a per-sender remote-image rule, persist it, apply it now. */
  const setSenderImageRule = useCallback((addr: string, rule: ImageRule | null) => {
    if (!addr) return;
    const s = useAppStore.getState();
    const key = addr.trim().toLowerCase();
    const next = { ...s.senderImageTrust };
    if (rule) next[key] = rule;
    else delete next[key];
    s.setSenderImageTrust(next);
    setPref("senderImageTrust", next);
    applyRule(resolveImageRule(addr, next, s.domainImageTrust));
  }, []);

  /** Set (or clear) a domain-level rule; sender rules win ties. */
  const setDomainImageRule = useCallback((addr: string, rule: ImageRule | null) => {
    const domain = emailDomain(addr);
    if (!domain) return;
    const s = useAppStore.getState();
    const next = { ...s.domainImageTrust };
    if (rule) next[domain] = rule;
    else delete next[domain];
    s.setDomainImageTrust(next);
    setPref("domainImageTrust", next);
    applyRule(resolveImageRule(addr, s.senderImageTrust, next));
  }, []);

  return { setSenderImageRule, setDomainImageRule };
}
