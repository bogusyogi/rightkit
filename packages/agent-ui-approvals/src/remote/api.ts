// Pulse's device link, remote approvals, remote unlock, browser logins, secure box and notch settings over the
// Tauri commands of Pulse's hub. Moved here from @rightkit/system-ui 0.1.0, whose system client no longer carries
// them; these screens are their only users.
import { invoke } from "@tauri-apps/api/core";

// ---- devices (device link; the resident engine's `device.*` methods, hub/src-tauri/src/commands.rs) ----

/** Effect classes a paired device may request (`agent/effect.rs`, kebab-case). */
export const DEVICE_GRANT_CLASSES = ["reversible", "irreversible-destruction", "spend", "publication"] as const;
export type DeviceGrantClass = (typeof DEVICE_GRANT_CLASSES)[number];

/** One device from `device.list` (the answer is `{ devices: DeviceRecord[] }`). */
export interface DeviceRecord {
  deviceId: string;
  name: string;
  platform: string;
  /** Hex key fingerprint; null until the engine reports it. */
  keyFingerprint: string | null;
  state: string;
  grants: string[];
  grantsExpireAtMs: number | null;
  revision: number;
  /** Null when the engine does not say whether the link is up. */
  linkUp: boolean | null;
  /** The computer may add devices for the circle. */
  admit?: boolean;
}

/** The answer to `device.list`. */
export interface DeviceListResult {
  devices: DeviceRecord[];
}

/**
 * `code`: only the code is known; the owner types it on the other computer. Name, platform, id and
 * fingerprint are empty and Approve is not offered. `confirm`: the other computer has confirmed and
 * the full card with Approve and Reject is shown.
 */
export type PendingPairingStage = "code" | "confirm";

/** The live pairing awaiting the owner (`device.pair.pending` → `pending`). */
export interface PendingPairing {
  pairingId: string;
  /** Eight digits, shown as 4+4. */
  code: string;
  stage: PendingPairingStage;
  deviceName: string;
  platform: string;
  deviceId: string;
  keyFingerprint: string;
  expiresAtMs: number;
}

/** The answer to `device.pair.pending`: `pending` is set only while `open` is true. */
export interface PendingPairingResult {
  open: boolean;
  pending: PendingPairing | null;
}

/** What `device.pair.start` returns (the code is never in it). */
export interface PairingStarted {
  pairingId: string;
  expiresAtMs: number;
  /** The single-use join secret (64 hex) the QR carries as `s=`; a phone that proves it joins with one scan. */
  secret?: string | null;
  /** This computer's iroh endpoint id (64 hex); a phone's QR carries it. */
  endpointId?: string;
  /** Our relay, when one is set; null otherwise. */
  relayUrl?: string | null;
  /** This computer's private-network addresses (ip:port); the QR carries them for a phone. */
  addrs?: string[];
}

/** What `device.join` returns once this computer joined a host. */
export interface JoinedHost {
  state: string;
  deviceId: string;
  hostName: string;
}

/** What `device.join.create` returns. `token` and `command` are a bearer secret until used or expired: show them to the owner only. */
export interface JoinCreated {
  /** `pulse-join:1:...` */
  token: string;
  /** `pulse device join <token>`, the one line to run on the computer being added. */
  command: string;
  pairingId: string;
  expiresAtMs: number;
  /** Guest links only (#493): per `uname -m` arch, the pinned artifact and the copyable install command (a bearer secret, it carries the pass). Absent on older engines. */
  linux?: Record<string, { url: string; sha256: string; size: number; glibc: string; command: string }>;
  /** Why no Linux command was made (no published Linux build, unreadable manifest). */
  linuxUnavailable?: string;
}

/** The engine's refusal text is `code: message`; these codes mean the owner check wants a (new) PIN. */
export const OWNER_CHECK_CODES = /^(unlock_needs_auth|pin_not_set|pin_wrong|pin_locked|pin_invalid): (.*)$/s;

/** A Touch ID proof of the owner: the engine's single-use challenge and the signature the Mac notch made over it. The engine verifies it. */
export interface OwnerProof {
  challenge: string;
  sig: unknown;
}

export type PhoneReadMode = "allow" | "deny";
export type PhoneSendMode = "allow" | "ask" | "deny";

/** One paired phone from `device.phone.access`; `error` is set when its policy file is unreadable. */
export interface PhoneAccess {
  deviceId: string;
  name: string;
  read?: PhoneReadMode;
  send?: PhoneSendMode;
  admin?: boolean;
  error?: string;
}

export interface PhoneAccessResult {
  enabled: boolean;
  defaults: { read: PhoneReadMode; send: PhoneSendMode };
  phones: PhoneAccess[];
}

/** A phone send held for the owner (`device.phone.asks`); it times out after 60 s. */
export interface PhoneAsk {
  askId: string;
  deviceId: string;
  deviceName: string;
  chat: string;
  chatName: string;
  preview: string;
  createdAtMs: number;
}

/** One guest from `device.guest.list` (#492): a cloud box that redeemed a guest link. */
export interface GuestRecord {
  deviceId: string;
  name: string;
  platform: string;
  status: "pending" | "approved" | "lapsed";
  /** The three digits the box printed when it joined; compare before approving. */
  checkCode: string;
  /** What the box said about itself; nothing verifies it. */
  reportedSession: string | null;
  mintedBy: string;
  approvedAtMs: number | null;
  leaseExpiresAtMs: number;
  lastSeenMs: number | null;
  /** What it may not do on any computer: add or manage devices, the phone admin path. */
  refused: string[];
}

export const devicesApi = {
  list: () => invoke<DeviceListResult>("device_list"),
  pending: () => invoke<PendingPairingResult>("device_pair_pending"),
  /** Opens a phone pairing. The engine checks the owner (`pin`, the the host app PIN) first. */
  pairStart: (pin?: string, proof?: OwnerProof) =>
    invoke<PairingStarted>("device_pair_start", { pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null }),
  /** Joins the computer whose owner made `token` (`pulse-join:1:...`); nothing else is asked. */
  joinWith: (token: string) => invoke<JoinedHost>("device_join", { token }),
  /** The engine checks the owner itself: `pin` is the the host app PIN. Without it the engine refuses `unlock_needs_auth`. */
  pairApprove: (pairingId: string, pin?: string, proof?: OwnerProof) =>
    invoke<unknown>("device_pair_approve", { pairingId, pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null }),
  /** Grants or takes away a computer's `admit` (it may add devices for the circle); owner-checked like Add a device. */
  admitSet: (deviceId: string, allowed: boolean, pin?: string, proof?: OwnerProof) =>
    invoke<unknown>("device_admit_set", { deviceId, allowed, pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null }),
  /** The enrolled owner-check keys (`remote.owner.key.status`; the Mac notch's and Windows Hello's). */
  ownerKeys: () =>
    invoke<{
      keys: { keyId: string; device: string; dev: boolean; bound?: boolean }[];
      /** Where the owner-check keys are kept; `weaker` is Windows without the elevated the host app service (a file any process of this account can rewrite). */
      ownerCheck?: { protection?: "service" | "file" | "keychain"; weaker?: boolean };
      /** True once a day while the owner key needs confirming (the engine remembers the offer), so a recreated web view does not ask again. */
      bindDue?: boolean;
    }>("owner_key_status"),
  /** Confirms this computer's owner-check key (binds it to this device) with one owner check. Windows signs in the hub; the Mac's notch signs (`notch_command`, `signOwnerChallenge`, op `bind`). */
  ownerKeyBind: () => invoke<unknown>("owner_key_bind"),
  /** Removes one owner-check key; the engine checks `pin`. */
  ownerKeyRevoke: (keyId: string, pin: string) => invoke<unknown>("owner_key_revoke", { keyId, pin }),
  /** "Add a device": the engine checks `pin` (the the host app PIN) first, then makes one join command valid for ten minutes. */
  joinCreate: (pin?: string, proof?: OwnerProof) =>
    invoke<JoinCreated>("device_join_create", { pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null }),
  /** "Make a guest link" (#492): the same owner check as Add a device, then one single-use link for a cloud box. */
  guestCreate: (leaseMs: number, pin?: string, proof?: OwnerProof) =>
    invoke<JoinCreated>("device_guest_create", { pin: pin ?? null, challenge: proof?.challenge ?? null, sig: proof?.sig ?? null, leaseMs }),
  guests: () => invoke<{ guests: GuestRecord[] }>("device_guest_list"),
  /** Owner UI only: makes a pending cloud agent one of your machines until its lease ends. */
  guestApprove: (deviceId: string) => invoke<unknown>("device_guest_approve", { deviceId }),
  /** Voids the open join command. */
  joinCancel: () => invoke<unknown>("device_join_cancel"),
  pairReject: (pairingId: string) => invoke<unknown>("device_pair_reject", { pairingId }),
  revoke: (deviceId: string) => invoke<unknown>("device_revoke", { deviceId }),
  phoneAccess: () => invoke<PhoneAccessResult>("device_phone_access"),
  /** `read` allow|deny|default; `send` allow|ask|deny|default; omit one to leave it. */
  phoneAccessSet: (deviceId: string, change: { read?: PhoneReadMode | "default"; send?: PhoneSendMode | "default" }) =>
    invoke<unknown>("device_phone_access_set", { deviceId, ...change }),
  /** Off revokes the phone's approval key first (system prompt); rejects if cancelled, leaving admin on. */
  phoneAdminSet: (deviceId: string, allow: boolean) => invoke<unknown>("device_phone_admin_set", { deviceId, allow }),
  phoneAsks: () => invoke<{ asks: PhoneAsk[] }>("device_phone_asks"),
  phoneAnswer: (askId: string, allow: boolean) => invoke<unknown>("device_phone_answer", { askId, allow }),
  computerName: () => invoke<string | null>("device_computer_name"),
  /** `revision` is the one the device was read at; `expiresAtMs` null means no expiry. */
  grantsSet: (deviceId: string, revision: number, classes: DeviceGrantClass[], expiresAtMs: number | null) =>
    invoke<unknown>("device_grants_set", { deviceId, revision, classes, expiresAtMs }),
};

// ---- remote approvals (Settings > Remote, "Waiting for your approval"; the engine's `remote.approvals.*`) ----

/** One remote request waiting for the owner's answer (core/src/remote/approvals.rs `Item`, state `pending`). */
export interface RemoteApproval {
  id: string;
  /** `act` (one change) or `session` (a family's grant). */
  kind: string;
  /** The tool for an act (e.g. `files.patch`), the family (`terminal`) for a session. */
  scope: string;
  /** The door the call came in by: `local` or `gateway`. */
  origin: string;
  /** What the request is bound to; shown to nobody, sent back with the answer so the engine can check it. */
  bind: string;
  /** What it will do and the folder or file name it touches; never file contents or secrets. */
  summary: string;
  /** The external chat's client name (unlock requests). */
  client?: string;
  state: string;
  created: number;
  decided: number;
  lastUsed: number;
}

/** The engine answers `remote.approvals.pending` with `{ items: [...] }` (engine/src/contract.rs); a bare list or `{ approvals }` is read too. */
export function pendingApprovals(answer: unknown): RemoteApproval[] {
  const wrapped = answer as { items?: unknown; approvals?: unknown } | null;
  const list = Array.isArray(answer) ? answer : (wrapped?.items ?? wrapped?.approvals);
  return Array.isArray(list) ? (list as RemoteApproval[]).filter((a) => a && typeof a.id === "string") : [];
}

export const remoteApprovalsApi = {
  pending: () => invoke<unknown>("remote_approvals_pending"),
  /** Approves or denies one pending request, bound to the item as displayed (the engine compares a digest of its fields). Owner UI only: the engine checks that the caller is the the host app hub. */
  decide: (a: RemoteApproval, approve: boolean, pin?: string) =>
    invoke<unknown>("remote_approval_decide", {
      id: a.id,
      approve,
      pin: approve ? pin ?? "" : null,
      kind: a.kind,
      scope: a.scope,
      origin: a.origin,
      bind: a.bind,
      summary: a.summary,
    }),
};

/** The per-chat unlock: the the host app PIN and the owner's answer to an unlock request (`scope === "unlock"`). Never log or keep a PIN. */
export const remoteUnlockApi = {
  pinStatus: () => invoke<unknown>("remote_unlock_pin_status"),
  /** `current` is needed only when a PIN is already set. */
  pinSet: (pin: string, current?: string) => invoke<unknown>("remote_unlock_pin_set", { pin, current: current || null }),
  /** Denies, or approves with the the host app PIN (the hub cannot show the system prompt). */
  decide: (a: RemoteApproval, approve: boolean, pin?: string) =>
    invoke<unknown>("remote_unlock_decide", {
      args: {
        id: a.id,
        approve,
        kind: a.kind,
        scope: a.scope,
        origin: a.origin,
        bind: a.bind,
        summary: a.summary,
        pin: approve ? pin ?? "" : null,
      },
    }),
};

// ---- browser logins (bogusyogi/pulse#460: the engine's `browser.remote.*`, per paired computer) ----
// Each call goes through a hub command named `browser_remote_*`, which forwards to the engine method of
// the same name and refuses without an attached engine. The engine owns the approval: Approve asks for a
// nonce (`challenge`) and then `approve`, and the engine itself runs the owner check.

/** A sign-in request an agent made on a paired computer and that waits for the owner. */
export interface BrowserRequest {
  id: string;
  sites: string[];
  reason: string;
  chat: string;
  expires_ms?: number;
}

/** A sign-in session on a paired computer. `needs-owner` is the state the live view is for. */
export interface BrowserSession {
  session: string;
  sites: string[];
  chat: string;
  state: string;
  url?: string;
  title?: string;
}

/** One frame of a session: JPEG, at most 1280 wide; `seq` rises with each new frame. */
export interface BrowserFrame {
  seq: number;
  jpeg_b64: string;
  width: number;
  height: number;
  url: string;
  title: string;
}

/** An input for the session, with x and y in frame pixels. `text` is never logged or kept. */
export type BrowserInput =
  | { type: "click"; x: number; y: number }
  | { type: "text"; text: string }
  | { type: "key"; key: string }
  | { type: "scroll"; dy: number };

/** The rows a list call answers: a bare list, or the list under `items` or the given key. */
export function browserRows<T>(answer: unknown, key?: string): T[] {
  if (Array.isArray(answer)) return answer as T[];
  const wrapped = answer as Record<string, unknown> | null;
  const list = wrapped?.items ?? (key ? wrapped?.[key] : undefined);
  return Array.isArray(list) ? (list as T[]) : [];
}

export const browserRemoteApi = {
  pending: (device: string) => invoke<unknown>("browser_remote_pending", { device }),
  /** A single-use nonce for one approval (valid for 60 seconds). */
  challenge: (device: string, id: string) => invoke<{ nonce: string }>("browser_remote_challenge", { device, id }),
  approve: (device: string, id: string, nonce: string) => invoke<unknown>("browser_remote_approve", { device, id, nonce }),
  deny: (device: string, id: string) => invoke<unknown>("browser_remote_deny", { device, id }),
  sessions: (device: string) => invoke<unknown>("browser_remote_sessions", { device }),
  frame: (device: string, session: string, afterSeq: number) =>
    invoke<BrowserFrame>("browser_remote_frame", { device, session, afterSeq }),
  input: (device: string, session: string, events: BrowserInput[]) =>
    invoke<unknown>("browser_remote_input", { device, session, events }),
  /** Hands the session back to the agent. */
  done: (device: string, session: string) => invoke<unknown>("browser_remote_done", { device, session }),
};

/** One saved item of the agent secure box, as `remote.secure.list` answers: labels, hosts, last four and limits, never a value. */
export interface SecureItem {
  id: string;
  kind: "login" | "card";
  label: string;
  /** A login's one site. */
  host?: string;
  /** A login has a one-time code saved (never the secret). */
  totp?: boolean;
  /** A card's shops. */
  hosts?: string[];
  last4?: string;
  maxCents?: number;
  autoCents?: number;
  monthCents?: number;
}
export interface SecureList { items: SecureItem[]; month?: string; spentCents?: Record<string, number> }
/** `cancelled` when the system dialog was closed; otherwise the saved item's metadata. */
export type SecureSaved = SecureItem | { cancelled: true };

/**
 * The agent secure box (#345 logins, #346 virtual cards). The renderer sends only non-secret fields and the PIN;
 * the username, password, card number, name, expiry and CVC are typed into a system dialog opened by the command
 * and go straight to the engine, so no secret ever exists in this page.
 */
export const secureBoxApi = {
  list: () => invoke<SecureList>("remote_secure_list"),
  remove: (id: string) => invoke<unknown>("remote_secure_remove", { id }),
  saveLogin: (label: string, host: string, pin: string, withTotp = false) =>
    invoke<SecureSaved>("remote_secure_save_login", { label, host, pin, withTotp }),
  /** Adds or replaces a saved login's one-time code; the secret is typed into the system dialog. */
  setTotp: (id: string, host: string, pin: string) =>
    invoke<SecureSaved>("remote_secure_set_totp", { id, host, pin }),
  saveCard: (c: { label: string; hosts: string[]; zip: string; maxCents: number; autoCents: number; monthCents: number }, pin: string) =>
    invoke<SecureSaved>("remote_secure_save_card", { ...c, pin }),
};

/** The sentence the Devices page shows when the engine refuses pairing with `listen_off`. */
export const LISTEN_OFF_SENTENCE =
  "Turn on Settings > Devices > Accept device links (LAN only) on this computer first.";

/** The engine's `listen_off` refusal (engine/src/devicelink_host.rs, pair_start). The hub's engine link
 *  passes only the refusal's message, not its code, so the message text identifies it. */
export function isListenOff(e: unknown): boolean {
  const text = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
  return text.startsWith("listen_off:") || text.includes("turn on Accept device links first");
}

/** The owner-only security settings (`PROTECTED_KEYS`, core/src/remote/settings.rs; listed in docs/settings.md).
 *  A `hub-commands` file never changes them: they go through the engine's owner check (`remote_setting_set`). */
const PROTECTED_KEYS = new Set([
  "chatUnlock", "chatUnlockMinutes", "remoteBlockedApps", "agentActions", "agentCliPath", "agentBrowser", "agentBrowserSessionMinutes", "remoteRoots", "remoteGateway",
  "remoteChatRead", "phoneChatReadAll", "remoteDocs", "remoteLaunch", "remoteMedia",
  "remoteReadMaxBytes", "remoteFindMaxResults", "remoteChatReadMaxTurns", "remoteChatReadMaxBytes", "remoteImageMaxBytes", "remoteDocsMaxBytes",
  "remoteFormFill", "remoteFormFillStanding", "remoteFormFillProfile", "remoteScreenObserve", "remoteScreenAct",
  "remoteScreenDisclosure", "remoteTerminals", "remoteTermStanding", "remoteTermAllow", "remoteTermDeny",
  "remoteClipboard", "remoteReadOnly", "ledgerTranscriptSearch", "ledgerTranscriptShare", "remoteFilesWrite", "remoteTools",
]);
/** `PROTECTED_PREFIXES` in core: every key under one of these is owner-only too. */
const PROTECTED_PREFIXES = ["remoteFormFill", "remoteScreen"];
export const isProtectedKey = (key: string): boolean =>
  PROTECTED_KEYS.has(key) || PROTECTED_PREFIXES.some((prefix) => key.startsWith(prefix));

/** A protected change waiting for the the host app PIN (the engine asks for it when a change makes a gate less safe). */
export interface PinAsk { key: string; value: unknown; problem: string | null }
let pinListener: ((ask: PinAsk | null) => void) | null = null;
export function onProtectedPin(fn: ((ask: PinAsk | null) => void) | null): void {
  pinListener = fn;
}

/** Sends one protected change to the engine. Without a PIN a change that loosens a gate is refused with
 *  `unlock_needs_auth`; that opens the PIN prompt (`ProtectedPinPrompt`). Never logs or keeps the PIN. */
export async function setProtectedSetting(key: string, value: unknown, pin?: string): Promise<void> {
  try {
    await invoke<unknown>("remote_setting_set", { key, value, pin: pin ? pin : null });
    pinListener?.(null);
  } catch (e) {
    const text = typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
    const m = /^(unlock_needs_auth|pin_not_set|pin_wrong|pin_locked|pin_invalid): (.*)$/s.exec(text);
    if (m && pinListener) pinListener({ key, value, problem: pin || m[1] !== "unlock_needs_auth" ? m[2] : null });
    else throw e;
  }
}

/** Sets one notch setting the way the Settings page does: `notch_command` with a `set` command
 *  (a protected key goes through the engine instead). */
export function setNotchSetting(key: string, value: unknown): Promise<unknown> {
  if (isProtectedKey(key)) return setProtectedSetting(key, value);
  return invoke<unknown>("notch_command", { command: { command: "set", key, value } });
}
