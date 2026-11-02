import type { AuthStatus } from "./ipc.js";

export type AuthBadge = { label: string; level: "pass" | "fail" | "none"; title: string };

/**
 * Roll an AuthStatus up to a single sender-line badge. DMARC is the decisive
 * signal (it binds SPF/DKIM to the From domain); SPF/DKIM are the fallback.
 * Returns null when there's nothing meaningful to show (all none/unknown), so
 * the reader doesn't clutter every message with an inconclusive badge.
 */
export function authBadge(a: AuthStatus | null): AuthBadge | null {
  if (!a) return null;
  const title = `SPF ${a.spf} · DKIM ${a.dkim} · DMARC ${a.dmarc}`;
  const anyFail = [a.spf, a.dkim, a.dmarc].includes("fail");
  const dmarcPass = a.dmarc === "pass";
  const anyPass = [a.spf, a.dkim].includes("pass");
  if (anyFail && !dmarcPass) return { label: "Failed auth", level: "fail", title };
  if (dmarcPass || anyPass) return { label: "Verified", level: "pass", title };
  return null;
}
