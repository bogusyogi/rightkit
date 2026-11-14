import { type Dispatch, type SetStateAction, useEffect, useState } from "react";
import { sectionClient } from "../settings/client.js";
import type { OS } from "./types.js";

// UX-4: the input is `type=email`, but JS-driven submit (the email button
// here) bypasses HTML validation. Validate shape before the network call so
// the user sees an honest "looks like a typo" instead of the server's
// generic "couldn't reach / invalid" reply. Permissive on purpose — the
// authoritative check is on the server.
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

type Args = {
  setError: Dispatch<SetStateAction<string>>;
  /** Kept for callers: the trial length depends on the platform, which native code adds itself. */
  platform: OS;
};

export function useOnboardingTrial({ setError }: Args) {
  const [trialEmail, setTrialEmail] = useState("");
  const [trialKey, setTrialKey] = useState("");
  const [trialPhase, setTrialPhase] = useState<"idle" | "sent" | "active">("idle");
  const [trialBusy, setTrialBusy] = useState(false);
  const [trialMsg, setTrialMsg] = useState("");

  // The step remounts on every visit (Back/forward, a relaunch that resumes here), so an already
  // activated trial is read back instead of offering a second key request.
  useEffect(() => {
    let live = true;
    const license = sectionClient("license");
    license
      .loadLicense()
      .then((view) => {
        if (live && license.viewShowsPro(view)) setTrialPhase((phase) => (phase === "idle" ? "active" : phase));
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, []);

  async function requestTrial() {
    const email = trialEmail.trim();
    if (!email) return;
    // UX-4: browser type=email is not enough when JS handles submit.
    if (!EMAIL_RE.test(email)) {
      setError("");
      setTrialMsg("That doesn't look like an email address. Check for typos?");
      return;
    }
    setError("");
    setTrialMsg("");
    setTrialBusy(true);
    try {
      const ok = await sectionClient("license").requestTrialKey(email);
      if (ok) {
        setTrialPhase("sent");
        setTrialMsg("Key sent. Check your email, then paste it below.");
      } else {
        setTrialMsg("Could not send the key. Check the address and try again.");
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
    const license = sectionClient("license");
    try {
      const lic = await license.activateLicense(key);
      if (lic.status === "active") {
        setTrialPhase("active");
        setTrialMsg("");
        await license.refreshEntitlements?.();
      } else {
        // Surface the actual reason native code captured (device limit reached /
        // couldn't reach the server) instead of one opaque string — otherwise a
        // seat-limit or network failure looks identical to a mistyped key and
        // there's nothing to act on.
        setTrialMsg(
          license.licenseText(lic.message).trim() || "That key didn't activate. Double-check it from the email.",
        );
      }
    } catch (e) {
      setTrialMsg((license.words ?? ((text: string) => text))(e instanceof Error ? e.message : String(e)));
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
