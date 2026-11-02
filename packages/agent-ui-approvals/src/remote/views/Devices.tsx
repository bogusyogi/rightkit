import { useEffect, useRef, useState } from "react";
import { Badge, Button, ConfirmDialog, Select, Toggle } from "@rightkit/react-shell/react";
import {
  DEVICE_GRANT_CLASSES,
  LISTEN_OFF_SENTENCE,
  devicesApi,
  isListenOff,
  setNotchSetting,
  type DeviceGrantClass,
  type DeviceRecord,
  type JoinCreated,
  type OwnerProof,
  type PendingPairing,
} from "@rightkit/system-ui/api";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";
import { SettingRow, SettingsGroup, SettingsSearchSection } from "@rightkit/settings-ui/core";
import { openSectionLabel } from "@rightkit/system-ui/views/settingsKit";
import { PhonePairing } from "./PhonePairing.js";
import { BrowserLogins } from "./BrowserLogins.js";
import { OwnerPinCard, confirmOwnerKey, onMac, ownerProblem, touchIdProof } from "./OwnerPin.js";
import { GuestsGroup } from "./Guests.js";

/** Settings > Devices: paired devices, the pairing approval card, per-device grants and revoke.
 *  Every change goes to the resident engine (`device.*`); with no engine the page says so. */

/** A paired device as the page shows it, read from one `device.list` record. */
interface DeviceView {
  id: string;
  name: string;
  platform: string;
  /** Null until the engine reports the key fingerprint. */
  fingerprint: string | null;
  revoked: boolean;
  /** Null when the engine does not say whether the link is up. */
  linkUp: boolean | null;
  revision: number;
  grants: string[];
  expiresAtMs: number | null;
  /** The computer may add devices for the circle. */
  admit: boolean;
}

const PLATFORM: Record<string, string> = { mac: "Mac", windows: "Windows", linux: "Linux", ios: "iOS" };

const platformName = (p: string | null | undefined): string =>
  p ? (PLATFORM[p] ?? p) : "Platform not reported";

/** What each grant lets a device ask for. Defaults: all off, since pairing grants nothing. */
const GRANT_TEXT: Record<DeviceGrantClass, { title: string; note: string }> = {
  reversible: {
    title: "Reversible actions",
    note: "Can be undone later. Off by default: a new device may request nothing.",
  },
  "irreversible-destruction": {
    title: "Permanent deletion",
    note: "Cannot be recalled. Off by default. Even when on, each act still asks you.",
  },
  spend: {
    title: "Spending money",
    note: "Moves money or a financial asset. Off by default. Even when on, each act still asks you.",
  },
  publication: {
    title: "Publishing or sending out",
    note: "Makes private content public, or sends it outside your accounts. Off by default. Even when on, each act still asks you.",
  },
};

/** Grant expiry choices; `ms` null means the grants do not expire. Default: No expiry. */
const EXPIRY: { label: string; ms: number | null }[] = [
  { label: "No expiry", ms: null },
  { label: "1 hour", ms: 60 * 60 * 1000 },
  { label: "1 day", ms: 24 * 60 * 60 * 1000 },
  { label: "7 days", ms: 7 * 24 * 60 * 60 * 1000 },
];

const POLL_MS = 3000;

function message(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}

/** The eight-digit code as 4+4, for reading across to the other computer. */
export function formatCode(code: string): string {
  return /^\d{8}$/.test(code) ? `${code.slice(0, 4)} ${code.slice(4)}` : code;
}

function toView(raw: DeviceRecord): DeviceView {
  return {
    id: raw.deviceId ?? "unknown",
    name: raw.name?.trim() || raw.deviceId || "Unnamed device",
    platform: raw.platform ?? "",
    fingerprint: raw.keyFingerprint ?? null,
    revoked: raw.state === "revoked",
    linkUp: typeof raw.linkUp === "boolean" ? raw.linkUp : null,
    revision: raw.revision,
    grants: raw.grants ?? [],
    expiresAtMs: raw.grantsExpireAtMs ?? null,
    admit: raw.admit === true,
  };
}

const clock = (ms: number): string => new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

function countdown(ms: number, now: number): string {
  const left = Math.max(0, Math.ceil((ms - now) / 1000));
  if (left === 0) return "expired";
  return `${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}`;
}

function grantsSummary(d: DeviceView): string {
  if (d.grants.length === 0) return "none";
  const names = DEVICE_GRANT_CLASSES.filter((c) => d.grants.includes(c)).map((c) => GRANT_TEXT[c].title.toLowerCase());
  const text = names.join(", ");
  return d.expiresAtMs ? `${text}, until ${clock(d.expiresAtMs)}` : text;
}

export function DevicesSection({ onOpen, browserOn = false }: { onOpen: (id: string) => void; browserOn?: boolean }) {
  const [devices, setDevices] = useState<DeviceView[] | null>(null);
  const [pending, setPending] = useState<PendingPairing | null>(null);
  const [joinCmd, setJoinCmd] = useState<JoinCreated | null>(null);
  const [gate, setGate] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const touchTurn = useRef(0);
  const [problem, setProblem] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [pasted, setPasted] = useState("");
  /** The device ids known when the command was made; a new one means the other computer joined. */
  const baseline = useRef<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [listenOff, setListenOff] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<DeviceView | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [ownerGate, setOwnerGate] = useState<{ title: string; submit: (pin: string) => void } | null>(null);
  const [ownerNote, setOwnerNote] = useState<string | null>(null);
  const [ownerWaiting, setOwnerWaiting] = useState(false);
  const ownerTurn = useRef(0);
  const [ownerKeys, setOwnerKeys] = useState<{ keyId: string; device: string; bound?: boolean }[]>([]);
  const [bindNote, setBindNote] = useState<string | null>(null);
  /** The engine's once-a-day offer to confirm the owner key on its own (a click on the row always works). */
  const [bindDue, setBindDue] = useState(false);
  /** Windows without the elevated the host app service: the owner-check keys sit in a file any process of this account can rewrite. */
  const [ownerWeaker, setOwnerWeaker] = useState(false);

  const load = () => {
    devicesApi.list()
      .then((answer) => { setDevices(answer.devices.map(toView)); setError(null); })
      .catch((e) => { setDevices(null); setError(message(e)); });
    devicesApi.ownerKeys()
      .then((a) => {
        setOwnerKeys((a.keys ?? []).filter((k) => k.device === "mac-owner-check" || k.device === "windows-hello"));
        setOwnerWeaker(a.ownerCheck?.weaker === true);
        setBindDue(a.bindDue === true);
      })
      .catch(() => { setOwnerKeys([]); setOwnerWeaker(false); setBindDue(false); });
    devicesApi.pending()
      .then((answer) => setPending(answer.open ? answer.pending : null))
      .catch(() => setPending(null));
  };

  /** One owner check that binds an owner key enrolled before bindings existed to this computer. */
  const confirmKey = async () => {
    setBusy(true);
    setBindNote(null);
    const problem = await confirmOwnerKey();
    setBindNote(problem);
    setBusy(false);
    load();
  };

  useEffect(() => {
    if (bindDue) void confirmKey();
  }, [bindDue]);

  useEffect(() => {
    load();
    const timer = visibleInterval(load, POLL_MS);
    return () => clearVisibleInterval(timer);
  }, []);

  const expiresAt = pending?.expiresAtMs ?? joinCmd?.expiresAtMs ?? null;
  useEffect(() => {
    if (expiresAt == null) return;
    const timer = visibleInterval(() => setNow(Date.now()), 1000);
    return () => clearVisibleInterval(timer);
  }, [expiresAt]);

  /** Runs one engine action, shows its failure, then re-reads the devices. */
  const run = async (work: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
      load();
    }
  };

  /** "Add a device": the owner check first (the the host app PIN, checked by the engine), then one command. */
  const confirmAdd = (pin?: string, proof?: OwnerProof) => run(async () => {
    setListenOff(false);
    setNotice(null);
    try {
      const made = await devicesApi.joinCreate(pin, proof);
      baseline.current = new Set((devices ?? []).map((d) => d.id));
      setJoinCmd(made);
      setCopied(false);
      setGate(false);
      setProblem(null);
    } catch (e) {
      const owner = ownerProblem(e);
      if (owner) { setProblem(owner); return; }
      setGate(false);
      if (!isListenOff(e)) throw e;
      setListenOff(true);
    }
  });

  /** On a Mac the owner check is Touch ID, asked of the notch; the PIN card is the fallback. */
  const startAdd = () => {
    setProblem(null);
    setGate(true);
    if (!onMac) return;
    const turn = ++touchTurn.current;
    setWaiting(true);
    void touchIdProof("join").then((proof) => {
      if (turn !== touchTurn.current) return;
      setWaiting(false);
      if (typeof proof === "string") setProblem(proof);
      else void confirmAdd(undefined, proof);
    });
  };
  const useTypedPin = () => { touchTurn.current++; setWaiting(false); };

  const cancelAdd = () => run(async () => {
    await devicesApi.joinCancel();
    setJoinCmd(null);
  });

  const copyCommand = () => {
    if (!joinCmd) return;
    void navigator.clipboard.writeText(joinCmd.command).then(() => setCopied(true)).catch(() => setCopied(false));
  };

  /** "Join a device": paste the command the other computer's Add a device shows. Nothing else is asked here. */
  const joinPasted = () => run(async () => {
    const text = pasted.trim().replace(/^pulse device join\s+/, "");
    setNotice(null);
    const joined = await devicesApi.joinWith(text);
    setPasted("");
    setNotice(`Joined ${joined.hostName}. It may request nothing until its owner gives grants.`);
  });

  // The command is done when the other computer appears in the list, or when its ten minutes end.
  useEffect(() => {
    if (!joinCmd || !devices) return;
    const fresh = devices.find((d) => !d.revoked && !baseline.current.has(d.id));
    if (fresh) {
      setNotice(`${fresh.name} joined.`);
      setJoinCmd(null);
    } else if (now >= joinCmd.expiresAtMs) {
      setNotice("The join command expired. Add a device again for a new one.");
      setJoinCmd(null);
    }
  }, [devices, joinCmd, now]);

  /** Turns on the `deviceLinkListen` setting, the same write the Settings page makes. */
  const turnOnListen = () => run(async () => {
    await setNotchSetting("deviceLinkListen", true);
    setListenOff(false);
    setNotice("Accept device links is on. Add a device again.");
  });

  /** The owner check for one engine action: Touch ID on a Mac (the notch signs), Windows Hello on
   *  Windows (the hub signs), the the host app PIN as the fallback. A refused check opens the PIN card. */
  const ownerRun = async (
    title: string,
    op: "join" | "approve",
    pairingId: string | null,
    call: (pin?: string, proof?: OwnerProof) => Promise<unknown>,
    pinOnly = false,
  ) => {
    const attempt = (pin?: string, proof?: OwnerProof) => run(async () => {
      try {
        await call(pin, proof);
        setOwnerGate(null);
        setOwnerNote(null);
      } catch (e) {
        const owner = ownerProblem(e);
        if (owner) {
          setOwnerGate({ title, submit: (typed) => void attempt(typed) });
          setOwnerNote(owner);
          return;
        }
        setOwnerGate(null);
        throw e;
      }
    });
    setOwnerNote(null);
    if (onMac && !pinOnly) {
      setOwnerGate({ title, submit: (typed) => void attempt(typed) });
      const turn = ++ownerTurn.current;
      setOwnerWaiting(true);
      const proof = await touchIdProof(op, pairingId);
      if (turn !== ownerTurn.current) return;
      setOwnerWaiting(false);
      if (typeof proof === "string") setOwnerNote(proof);
      else await attempt(undefined, proof);
    } else if (pinOnly) {
      setOwnerGate({ title, submit: (typed) => void attempt(typed) });
    } else {
      await attempt();
    }
  };
  const ownerPinInstead = () => { ownerTurn.current++; setOwnerWaiting(false); };

  const approve = (p: PendingPairing) =>
    ownerRun("Approve this device", "approve", p.pairingId, (pin, proof) => devicesApi.pairApprove(p.pairingId, pin, proof));

  const setAdmit = (d: DeviceView, allowed: boolean) =>
    ownerRun(allowed ? `Let ${d.name} add devices` : `Stop ${d.name} adding devices`, "join", null,
      (pin, proof) => devicesApi.admitSet(d.id, allowed, pin, proof));

  const reject = (p: PendingPairing) => run(async () => {
    await devicesApi.pairReject(p.pairingId);
  });

  const confirmRevoke = () => {
    const target = revoking;
    setRevoking(null);
    if (!target) return;
    if (editing === target.id) setEditing(null);
    void run(() => devicesApi.revoke(target.id));
  };

  const selected = devices?.find((d) => d.id === editing && !d.revoked) ?? null;

  return (
    <SettingsSearchSection headingLevel={3} passthroughWhenInactive openLabel={openSectionLabel} className="ck-search-section" id="devices" title="Devices" onOpen={onOpen}>
      {error && <div className="error" role="alert">{error}</div>}

      {pending && (
        <PendingCard pending={pending} now={now} busy={busy} onApprove={() => void approve(pending)} onReject={() => void reject(pending)} />
      )}

      {ownerGate && (
        <OwnerPinCard title={ownerGate.title} busy={busy} problem={ownerNote} waiting={ownerWaiting} onSubmit={ownerGate.submit}
          onCancel={() => { if (ownerWaiting) ownerPinInstead(); else { ownerTurn.current++; setOwnerGate(null); setOwnerNote(null); } }} />
      )}

      {gate && <OwnerPinCard title="Add a device" busy={busy} problem={problem} waiting={waiting} onSubmit={(pin) => void confirmAdd(pin)} onCancel={() => (waiting ? useTypedPin() : setGate(false))} />}

      <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Add a device">
        <SettingRow className="ck-set"
          label="Add a device"
          description="Asks for your Pulse PIN here, then shows one command. Run it on the computer you are adding; nothing is asked there and no code is typed. The command works once, for ten minutes. A new device may request nothing until you give it grants."
        >
          <Button size="sm" variant="secondary" disabled={busy || joinCmd !== null} onClick={startAdd}>Add a device</Button>
        </SettingRow>
        {listenOff && (
          <SettingRow className="ck-set" label={LISTEN_OFF_SENTENCE}>
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => void turnOnListen()}>Turn on Accept device links</Button>
          </SettingRow>
        )}
        {notice && <div className="ck-set ck-sub" role="status">{notice}</div>}
        {joinCmd && (
          <div className="ck-set" style={{ alignItems: "flex-start" }}>
            <div className="ck-text">
              <strong>Run this on the computer you are adding</strong>
              <div><code style={{ overflowWrap: "anywhere", userSelect: "all" }}>{joinCmd.command}</code></div>
              <div className="ck-sub">
                {`Works once. Anyone who has it can join until it expires${now >= joinCmd.expiresAtMs ? "" : ` (${countdown(joinCmd.expiresAtMs, now)} left)`}, so share it only with that computer. On a computer with the Pulse hub, choose Join a device and paste it.`}
              </div>
              <div className="ck-line" style={{ marginTop: 8 }}>
                <Button size="sm" variant="primary" disabled={busy} onClick={copyCommand}>{copied ? "Copied" : "Copy"}</Button>
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => void cancelAdd()}>Cancel</Button>
              </div>
            </div>
          </div>
        )}
      </SettingsGroup>

      <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Join a device">
        <SettingRow className="ck-set"
          label="Join a device"
          description="Paste the command another computer's Add a device shows. Nothing else is asked on this computer: the command is the proof."
        >
          <div className="ck-set ck-stack">
            <input className="ck-input" aria-label="Join command" autoComplete="off" spellCheck={false} value={pasted}
              placeholder="pulse-join:1:..." onChange={(e) => setPasted(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter" && pasted.trim() !== "") void joinPasted(); }} />
            <Button size="sm" variant="secondary" disabled={busy || pasted.trim() === ""} onClick={() => void joinPasted()}>Join</Button>
          </div>
        </SettingRow>
      </SettingsGroup>

      <PhonePairing pairingOpen={pending !== null} busy={busy} run={run} />

      <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Paired devices">
        {devices === null && !error && <div className="ck-set ck-sub" role="status">Reading paired devices…</div>}
        {devices?.length === 0 && (
          <SettingRow className="ck-set" label="No paired devices" description="Pair a computer to see it here. Pairing grants nothing by itself.">{null}</SettingRow>
        )}
        {devices?.map((d) => (
          <SettingRow className="ck-set"
            key={d.id}
            label={d.name}
            description={`${platformName(d.platform)} · ${d.id} · key ${d.fingerprint ?? "fingerprint not reported"} · grants: ${grantsSummary(d)}`}
          >
            <div className="ck-line">
              <Badge tone={d.revoked ? "warn" : "ok"}>
                {d.revoked ? "Revoked" : d.linkUp === null ? "Active" : d.linkUp ? "Link up" : "Link down"}
              </Badge>
              {!d.revoked && d.admit && <Badge tone="ok">Can add devices</Badge>}
              {/* Only a Mac or Windows computer can be allowed to add devices (#457); a phone or a server never is. */}
              {!d.revoked && /^(mac|macos|windows)$/.test(d.platform) && (
                <Toggle checked={d.admit} label="Can add devices"
                  onChange={(on: boolean) => void setAdmit(d, on)} />
              )}
              {!d.revoked && (
                <Button size="sm" variant="secondary" disabled={busy} onClick={() => setEditing(editing === d.id ? null : d.id)}>
                  {editing === d.id ? "Close grants" : "Grants"}
                </Button>
              )}
              {!d.revoked && (
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => setRevoking(d)}>Revoke</Button>
              )}
            </div>
          </SettingRow>
        ))}
      </SettingsGroup>

      <GuestsGroup busy={busy} run={run} />

      <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Owner-check keys">
        {ownerKeys.map((k) => (
          <SettingRow className="ck-set" key={k.keyId}
            label={k.device === "windows-hello" ? "Windows Hello key" : "Mac Touch ID key"}
            description={bindNote && k.bound === false ? bindNote : `Confirms you when you add or approve a device. Key ${k.keyId}. Removing it asks for your Pulse PIN; the next check enrols a new key.`}>
            {k.bound === false && (
              <Button size="sm" variant="secondary" disabled={busy} onClick={() => void confirmKey()}>Confirm owner key</Button>
            )}
            <Button size="sm" variant="ghost" disabled={busy}
              onClick={() => void ownerRun("Remove this owner-check key", "join", null, (pin) => devicesApi.ownerKeyRevoke(k.keyId, pin ?? ""), true)}>
              Remove
            </Button>
          </SettingRow>
        ))}
        {ownerWeaker && ownerKeys.length > 0 && (
          <SettingRow className="ck-set" label="Weaker protection"
            description="This computer keeps its owner-check keys in a file that other programs running as your account could rewrite. Other devices trust them for the circle. Install the elevated Pulse service (pulse admin install-service, one Windows confirmation) and they are kept where only administrators can change them.">
            <Badge tone="warn">Weaker</Badge>
          </SettingRow>
        )}
      </SettingsGroup>

      {browserOn && (
        <BrowserLogins devices={(devices ?? []).filter((d) => !d.revoked).map((d) => ({ id: d.id, name: d.name }))} />
      )}

      {selected && (
        <GrantsEditor key={selected.id} device={selected} busy={busy} run={run} onDone={() => setEditing(null)} />
      )}

      {revoking && (
        <ConfirmDialog
          danger
          title={`Revoke ${revoking.name}?`}
          description="This device loses its trust here at once: its requests are refused and its link closes. Pairing it again needs a new code and a new approval."
          confirmLabel="Revoke"
          onConfirm={confirmRevoke}
          onCancel={() => setRevoking(null)}
        />
      )}
    </SettingsSearchSection>
  );
}

/**
 * The pairing card. Stage "code": the code only, to type on the other computer; no details (the
 * engine sends them empty) and no Approve. Stage "confirm": the code, the device's own details, and
 * Approve or Reject.
 */
function PendingCard({ pending, now, busy, onApprove, onReject }: {
  pending: PendingPairing;
  now: number;
  busy: boolean;
  onApprove: () => void;
  onReject: () => void;
}) {
  const left = pending.expiresAtMs ? countdown(pending.expiresAtMs, now) : null;
  const codeStage = pending.stage === "code";
  return (
    <div className="ck-card" role="group" aria-label="Pairing request" style={{ marginBottom: 12 }}>
      <div className="ck-set">
        <div className="ck-text">
          <strong>{codeStage ? "Pairing code" : "Approve this device?"}</strong>
          <div className="ck-sub">
            {codeStage
              ? "Type this code on the other computer."
              : "Approve only if this code and fingerprint match the other computer."}
          </div>
        </div>
      </div>
      <div className="ck-set">
        <div className="ck-text">
          <div className="ck-sub">Code</div>
          <strong style={{ fontSize: 28, letterSpacing: 2, fontVariantNumeric: "tabular-nums" }}>{formatCode(pending.code)}</strong>
        </div>
      </div>
      {!codeStage && (
        <>
          <ApprovalDetail label="Name (chosen by the other computer, not verified)" value={pending.deviceName || "Unnamed device"} />
          <ApprovalDetail label="Platform" value={platformName(pending.platform)} />
          <ApprovalDetail label="Device id" value={pending.deviceId} />
          <ApprovalDetail label="Key fingerprint" value={pending.keyFingerprint} />
        </>
      )}
      <div className="ck-set">
        <div className="ck-line">
          {!codeStage && <Button size="sm" variant="primary" disabled={busy} onClick={onApprove}>Approve</Button>}
          <Button size="sm" variant="secondary" disabled={busy} onClick={onReject}>Reject</Button>
          {left && <span className="ck-sub">{left === "expired" ? "This code has expired." : `Expires in ${left}.`}</span>}
        </div>
      </div>
    </div>
  );
}

function ApprovalDetail({ label, value }: { label: string; value: string }) {
  return (
    <div className="ck-set">
      <div className="ck-text">
        <div className="ck-sub">{label}</div>
        <span style={{ overflowWrap: "anywhere" }}>{value}</span>
      </div>
    </div>
  );
}

/** One device's grants: a switch per effect class, an expiry, and Save, which sends `device.grants.set`. */
function GrantsEditor({ device, busy, run, onDone }: {
  device: DeviceView;
  busy: boolean;
  run: (work: () => Promise<unknown>) => Promise<void>;
  onDone: () => void;
}) {
  const [draft, setDraft] = useState<DeviceGrantClass[]>(() =>
    DEVICE_GRANT_CLASSES.filter((c) => device.grants.includes(c)));
  const [expiry, setExpiry] = useState(0);

  const save = () => run(async () => {
    const ms = EXPIRY[expiry].ms;
    await devicesApi.grantsSet(
      device.id,
      device.revision,
      DEVICE_GRANT_CLASSES.filter((c) => draft.includes(c)),
      ms === null ? null : Date.now() + ms,
    );
    onDone();
  });

  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title={`Grants for ${device.name}`}>
      {DEVICE_GRANT_CLASSES.map((c) => (
        <SettingRow className="ck-set" key={c} label={GRANT_TEXT[c].title} description={GRANT_TEXT[c].note}>
          <Toggle
            checked={draft.includes(c)}
            label={`${GRANT_TEXT[c].title} for ${device.name}`}
            onChange={(on: boolean) => setDraft((cur) => on ? [...cur, c] : cur.filter((x) => x !== c))}
          />
        </SettingRow>
      ))}
      <SettingRow className="ck-set"
        label="Expires"
        description={device.expiresAtMs
          ? `Currently ends ${clock(device.expiresAtMs)}. Save replaces the grants and their expiry. Default: No expiry.`
          : "Default: No expiry. Save replaces the grants and their expiry."}
      >
        <Select label={`Grant expiry for ${device.name}`} value={String(expiry)}
          onChange={(v) => setExpiry(Number(v))}
          options={EXPIRY.map((o, i) => ({ value: String(i), label: o.label }))} />
      </SettingRow>
      <SettingRow className="ck-set" label="Save grants" description="Saved to this device's record; the engine refuses it if the device changed since it was read.">
        <div className="ck-line">
          <Button size="sm" variant="primary" disabled={busy} onClick={() => void save()}>Save grants</Button>
          <Button size="sm" variant="ghost" disabled={busy} onClick={onDone}>Cancel</Button>
        </div>
      </SettingRow>
    </SettingsGroup>
  );
}
