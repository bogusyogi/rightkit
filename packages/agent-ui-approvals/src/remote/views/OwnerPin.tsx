import { useState } from "react";
import { Button, Input } from "@rightkit/react-shell/react";
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui/core";
import { invoke } from "@tauri-apps/api/core";
import { OWNER_CHECK_CODES, type OwnerProof } from "../api.js";
import { getPlatform } from "@rightkit/platform-ui";

/** The words of an owner-check refusal (wrong, missing or locked PIN), or null for any other error. */
export function ownerProblem(e: unknown): string | null {
  const text = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
  const m = OWNER_CHECK_CODES.exec(text);
  return m ? m[2] : null;
}

/** Whether this hub runs on a Mac, whose notch can confirm the owner with Touch ID. */
export const onMac = getPlatform() === "macos";

/**
 * Asks this Mac's notch to confirm the owner with Touch ID ("Add a device to the host app"): the engine
 * issues a single-use challenge for `op`, the notch signs it with its enrolled key, and the
 * signature comes back here only to be handed to the engine, which verifies it. Returns the proof,
 * or a sentence saying why not (cancelled, no enrolled key, no Touch ID); the page then asks for
 * the PIN instead. Nothing here decides anything.
 */
export async function touchIdProof(op: "join" | "approve", pairingId: string | null = null, scope: { guest?: boolean; leaseMs?: number } = {}): Promise<OwnerProof | string> {
  const failed = "Touch ID did not confirm this. Enter your Pulse PIN instead.";
  try {
    const issued = await invoke<{ challenge: string; client: string; admit?: unknown }>("device_owner_challenge", { op, pairingId, ...scope });
    // The notch names an approval `approve:<pairing id>`; an admission carries the engine's `admit`.
    const answer = await invoke<{ status?: string; reason?: string; sig?: string }>("notch_command", {
      command: {
        command: "signOwnerChallenge", op: pairingId ? `approve:${pairingId}` : op,
        challenge: issued.challenge, client: issued.client, ...(issued.admit ? { admit: issued.admit } : {}),
      },
    });
    if (answer.status === "signed" && typeof answer.sig === "string") {
      return { challenge: issued.challenge, sig: JSON.parse(answer.sig) };
    }
    if (answer.reason === "needs_pin") {
      return "Pulse could not enrol this Mac's Touch ID key. Enter your Pulse PIN instead.";
    }
    return failed;
  } catch {
    return failed;
  }
}

/**
 * Confirms this computer's owner-check key (gives a key enrolled before bindings existed its binding
 * to this device) with one owner check and no operation: the Mac's notch asks for Touch ID, Windows
 * Hello signs in the hub. Returns null when done, or a sentence saying why not. The engine verifies
 * the binding; nothing here decides anything.
 */
export async function confirmOwnerKey(): Promise<string | null> {
  const failed = "The owner key was not confirmed. Try again from Devices.";
  try {
    if (onMac) {
      const answer = await invoke<{ status?: string }>("notch_command", {
        command: { command: "signOwnerChallenge", op: "bind", challenge: "", client: "" },
      });
      return answer.status === "bound" ? null : failed;
    }
    await invoke("owner_key_bind");
    return null;
  } catch {
    return failed;
  }
}

/**
 * The owner check for adding a device, on the computer that admits it: the the host app PIN, which the
 * engine verifies itself. The device being added is asked for nothing. The PIN lives in this field
 * only until it is sent, then is cleared; it is never logged or kept.
 */
export function OwnerPinCard({ title, busy, problem, waiting, via = "Touch ID", note, onSubmit, onCancel }: {
  title: string;
  /** Which owner check is open while `waiting`: "Touch ID" (default) or "Windows Hello". */
  via?: string;
  /** Extra words under the PIN prompt, such as what a PIN-confirmed result cannot do. */
  note?: string;
  busy: boolean;
  /** True while this Mac's Touch ID prompt is open. */
  waiting?: boolean;
  problem: string | null;
  onSubmit: (pin: string) => void;
  onCancel: () => void;
}) {
  const [pin, setPin] = useState("");
  const submit = () => {
    const typed = pin;
    setPin("");
    if (typed !== "") onSubmit(typed);
  };
  if (waiting) {
    return (
      <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title={`Confirm with ${via}`}>
        <SettingRow className="ck-set" label={title} description={`Confirm with ${via} on this computer…`}>
          <Button size="sm" variant="ghost" onClick={onCancel}>Use PIN instead</Button>
        </SettingRow>
      </SettingsGroup>
    );
  }
  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Confirm with your Pulse PIN">
      <SettingRow className="ck-set" label={title}
        description={problem ?? `Adding a device is a sensitive step, so Pulse asks for your PIN here. The device you add is asked for nothing. Set a PIN under Remote access first if you have none.${note ? ` ${note}` : ""}`}>
        <div className="ck-set ck-stack">
          <Input type="password" className="ck-input" aria-label="Pulse PIN" autoComplete="off" value={pin} maxLength={64}
            onChange={(e) => setPin(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") submit(); }} />
          <Button size="sm" variant="primary" disabled={busy || pin === ""} onClick={submit}>Confirm</Button>
          <Button size="sm" variant="ghost" onClick={() => { setPin(""); onCancel(); }}>Cancel</Button>
        </div>
      </SettingRow>
    </SettingsGroup>
  );
}
