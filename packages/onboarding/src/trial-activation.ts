import { type Dispatch, type SetStateAction, useEffect, useState } from "react";

/**
 * Trial request and key activation as one hook: the email shape check, the request/activate calls
 * with their busy and message state, and the read-back of an already active trial when the step
 * remounts. The licence client is a port the app passes in (structural; no import of
 * `@rightkit/license`); every user-facing sentence is in `messages`.
 */

/** The three calls and two text helpers of the app's licence client the hook needs. */
export interface TrialActivationPort<View, Message> {
  /** Read the current licence view (the step remounts on every visit). */
  loadLicense(): Promise<View>;
  /** Whether that view already grants Pro. */
  viewShowsPro(view: View): boolean;
  /** Ask the server to email a trial key. `false` = not accepted. */
  requestTrialKey(email: string): Promise<boolean>;
  /** Activate with a key. `status === "active"` is success; otherwise `message` explains why. */
  activateLicense(key: string): Promise<{ status: string; message?: Message }>;
  /** Refresh entitlements after a successful activation. */
  refreshEntitlements(): Promise<unknown>;
  /** The words for a failed activation's message code. Empty = use `messages.activateFailed`. */
  messageText(message: Message | undefined): string;
  /** Optional spelling/wording pass for a thrown error's text. */
  formatError?(text: string): string;
}

/** The sentences the hook shows. */
export interface TrialActivationMessages {
  /** The entered email does not look like an address (checked before any network call). */
  invalidEmail: string;
  /** The key was sent. */
  keySent: string;
  /** The server did not accept the address. */
  sendFailed: string;
  /** The key did not activate and the server gave no reason. */
  activateFailed: string;
}

export type TrialPhase = "idle" | "sent" | "active";

export interface UseTrialActivationOptions<View, Message> {
  port: TrialActivationPort<View, Message>;
  messages: TrialActivationMessages;
  /** The step's shared error line; cleared at the start of each action. */
  setError: Dispatch<SetStateAction<string>>;
}

/** Permissive on purpose: the browser's `type=email` is bypassed by a JS-driven submit, and the
 * authoritative check is on the server. */
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export function useTrialActivation<View, Message>({
  port,
  messages,
  setError,
}: UseTrialActivationOptions<View, Message>) {
  const [trialEmail, setTrialEmail] = useState("");
  const [trialKey, setTrialKey] = useState("");
  const [trialPhase, setTrialPhase] = useState<TrialPhase>("idle");
  const [trialBusy, setTrialBusy] = useState(false);
  const [trialMsg, setTrialMsg] = useState("");

  // The step remounts on every visit (Back/forward, a relaunch that resumes here), so an already
  // activated trial is read back instead of offering a second key request.
  useEffect(() => {
    let live = true;
    port
      .loadLicense()
      .then((view) => {
        if (live && port.viewShowsPro(view)) setTrialPhase((phase) => (phase === "idle" ? "active" : phase));
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- read once per mount, as before
  }, []);

  async function requestTrial() {
    const email = trialEmail.trim();
    if (!email) return;
    if (!EMAIL_RE.test(email)) {
      setError("");
      setTrialMsg(messages.invalidEmail);
      return;
    }
    setError("");
    setTrialMsg("");
    setTrialBusy(true);
    try {
      const ok = await port.requestTrialKey(email);
      if (ok) {
        setTrialPhase("sent");
        setTrialMsg(messages.keySent);
      } else {
        setTrialMsg(messages.sendFailed);
      }
    } finally {
      setTrialBusy(false);
    }
  }

  async function activateTrial() {
    const key = trialKey.trim();
    if (!key) return;
    setError("");
    setTrialMsg("");
    setTrialBusy(true);
    try {
      const lic = await port.activateLicense(key);
      if (lic.status === "active") {
        setTrialPhase("active");
        setTrialMsg("");
        await port.refreshEntitlements();
      } else {
        // Surface the actual reason native code captured (device limit reached / couldn't reach
        // the server) instead of one opaque string, so a seat-limit or network failure does not
        // look identical to a mistyped key.
        setTrialMsg(port.messageText(lic.message).trim() || messages.activateFailed);
      }
    } catch (e) {
      const text = e instanceof Error ? e.message : String(e);
      setTrialMsg(port.formatError ? port.formatError(text) : text);
    } finally {
      setTrialBusy(false);
    }
  }

  return {
    activateTrial,
    requestTrial,
    setTrialEmail,
    setTrialKey,
    trialBusy,
    trialEmail,
    trialKey,
    trialMsg,
    trialPhase,
  };
}
