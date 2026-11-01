import { useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, Select } from "@rightkit/react-shell/react";
import {
  devicesApi,
  isListenOff,
  LISTEN_OFF_SENTENCE,
  setNotchSetting,
  type OwnerProof,
  type PairingStarted,
  type PhoneAccess,
  type PhoneAccessResult,
  type PhoneAsk,
  type PhoneReadMode,
  type PhoneSendMode,
} from "@rightkit/system-ui/api";
import { qrMatrix, qrSvgPath } from "@rightkit/system-ui/lib/qr";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui/core";
import { OwnerPinCard, onMac, ownerProblem, touchIdProof } from "./OwnerPin";

/** Devices > Pair a phone: the QR a phone scans, per-phone read and send modes, and held sends.
 *  The QR holds only public data (`docs/mesh-phone.md`); trust comes from the code and the Approve card. */

const ASK_WINDOW_MS = 60_000;
const POLL_MS = 2000;

const message = (e: unknown): string => (typeof e === "string" ? e : e instanceof Error ? e.message : String(e));

/** The pairing URI of docs/mesh-phone.md. */
export function pairingUri(started: PairingStarted, computer: string): string | null {
  if (!started.endpointId) return null;
  const parts = [
    "v=1",
    `e=${started.endpointId}`,
    `p=${started.pairingId}`,
    `x=${started.expiresAtMs}`,
  ];
  if (started.relayUrl) parts.push(`r=${encodeURIComponent(started.relayUrl)}`);
  // Direct addresses on this network (a phone cannot use mDNS without Apple's multicast entitlement).
  if (started.addrs?.length) parts.push(`a=${encodeURIComponent(started.addrs.join(","))}`);
  // The single-use secret: a phone that proves it joins with this one scan and no typed code.
  if (started.secret) parts.push(`s=${started.secret}`);
  parts.push(`n=${encodeURIComponent(computer)}`);
  return `pulse://pair?${parts.join("&")}`;
}

function QrCode({ text }: { text: string }) {
  const drawn = useMemo(() => {
    try { return qrSvgPath(qrMatrix(text)); } catch { return null; }
  }, [text]);
  if (!drawn) return <div className="error" role="alert">The pairing code is too long to draw.</div>;
  return (
    <svg role="img" aria-label="Pairing QR code" viewBox={`0 0 ${drawn.size} ${drawn.size}`} width={224} height={224}
      shapeRendering="crispEdges" style={{ background: "#fff", borderRadius: 8 }}>
      <path d={drawn.path} fill="#000" />
    </svg>
  );
}

export function PhonePairing({ pairingOpen, busy, run }: {
  /** True while the page shows the approval card (a phone has joined). */
  pairingOpen: boolean;
  busy: boolean;
  run: (work: () => Promise<unknown>) => Promise<void>;
}) {
  const [started, setStarted] = useState<PairingStarted | null>(null);
  const [computer, setComputer] = useState("This computer");
  const [listenOff, setListenOff] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [access, setAccess] = useState<PhoneAccessResult | null>(null);
  const [asks, setAsks] = useState<PhoneAsk[]>([]);
  const [now, setNow] = useState(() => Date.now());
  const [revoking, setRevoking] = useState<string | null>(null);
  const [adminNote, setAdminNote] = useState<string | null>(null);
  const [gate, setGate] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const touchTurn = useRef(0);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    void devicesApi.computerName().then((n) => n && setComputer(n)).catch(() => undefined);
  }, []);

  useEffect(() => {
    const load = () => {
      devicesApi.phoneAccess().then(setAccess).catch(() => setAccess(null));
      devicesApi.phoneAsks().then((a) => setAsks(a.asks ?? [])).catch(() => setAsks([]));
      setNow(Date.now());
    };
    load();
    const timer = visibleInterval(load, POLL_MS);
    return () => clearVisibleInterval(timer);
  }, []);

  // The QR goes away once the phone joined (the approval card takes over) or the pairing expired.
  useEffect(() => {
    if (started && (now >= started.expiresAtMs || pairingOpen)) setStarted(null);
  }, [now, started, pairingOpen]);

  const uri = started ? pairingUri(started, computer) : null;

  /** The owner check first (the the host app PIN, checked by the engine), then the QR. */
  const confirm = (pin?: string, proof?: OwnerProof) => run(async () => {
    setError(null);
    setListenOff(false);
    try {
      setStarted(await devicesApi.pairStart(pin, proof));
      setGate(false);
      setProblem(null);
    } catch (e) {
      const owner = ownerProblem(e);
      if (owner) { setProblem(owner); return; }
      setGate(false);
      if (isListenOff(e)) setListenOff(true);
      else setError(message(e));
    }
  });

  /** On a Mac the owner check is Touch ID, asked of the notch; the PIN card is the fallback. */
  const startPair = () => {
    setProblem(null);
    setGate(true);
    if (!onMac) return;
    const turn = ++touchTurn.current;
    setWaiting(true);
    void touchIdProof("join").then((proof) => {
      if (turn !== touchTurn.current) return;
      setWaiting(false);
      if (typeof proof === "string") setProblem(proof);
      else void confirm(undefined, proof);
    });
  };
  const useTypedPin = () => { touchTurn.current++; setWaiting(false); };

  const cancel = () => run(async () => {
    if (!started) return;
    await devicesApi.pairReject(started.pairingId);
    setStarted(null);
  });

  const turnOn = () => run(async () => {
    await setNotchSetting("deviceLinkListen", true);
    setListenOff(false);
  });

  const setMode = (p: PhoneAccess, change: { read?: PhoneReadMode; send?: PhoneSendMode }) =>
    run(async () => {
      await devicesApi.phoneAccessSet(p.deviceId, change);
      setAccess(await devicesApi.phoneAccess());
    });

  const setAdmin = (p: PhoneAccess, allow: boolean) =>
    run(async () => {
      setAdminNote(null);
      try {
        await devicesApi.phoneAdminSet(p.deviceId, allow);
        setRevoking(null);
      } catch (e) {
        // Cancelled or failed: the switch stays on, and approvals still work until revoked.
        setRevoking(null);
        setAdminNote(message(e));
      }
      setAccess(await devicesApi.phoneAccess());
    });

  const answer = (ask: PhoneAsk, allow: boolean) => run(async () => {
    await devicesApi.phoneAnswer(ask.askId, allow);
    setAsks((cur) => cur.filter((a) => a.askId !== ask.askId));
  });

  return (
    <>
      {asks.map((ask) => {
        const left = Math.max(0, Math.ceil((ask.createdAtMs + ASK_WINDOW_MS - now) / 1000));
        return (
          <div className="ck-card" role="group" aria-label="Phone send waiting" key={ask.askId} style={{ marginBottom: 12 }}>
            <div className="ck-set">
              <div className="ck-text">
                <strong>{ask.deviceName} wants to send to {ask.chatName || ask.chat}</strong>
                <div className="ck-sub" style={{ overflowWrap: "anywhere" }}>{ask.preview}</div>
                <div className="ck-sub">{left > 0 ? `Answer within ${left} s, or it is refused.` : "Timed out."}</div>
              </div>
            </div>
            <div className="ck-set">
              <div className="ck-line">
                <Button size="sm" variant="primary" disabled={busy || left === 0} onClick={() => void answer(ask, true)}>Approve</Button>
                <Button size="sm" variant="secondary" disabled={busy} onClick={() => void answer(ask, false)}>Deny</Button>
              </div>
            </div>
          </div>
        );
      })}

      <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Pair a phone">
        {error && <div className="error" role="alert">{error}</div>}
        <SettingRow className="ck-set"
          label="Pair a phone"
          description="Asks for your Pulse PIN here, then shows a QR code for the Pulse app on your iPhone. The QR holds a single-use secret that works for ten minutes: scanning it is the phone's whole proof, with no code to type and nothing asked on the phone. Older app builds still show an 8-digit code for your Approve."
        >
          <Button size="sm" variant="secondary" disabled={busy || started !== null || pairingOpen} onClick={startPair}>Pair a phone</Button>
        </SettingRow>
        {gate && <OwnerPinCard title="Pair a phone" busy={busy} problem={problem} waiting={waiting} onSubmit={(pin) => void confirm(pin)} onCancel={() => (waiting ? useTypedPin() : setGate(false))} />}
        {listenOff && (
          <SettingRow className="ck-set" label={LISTEN_OFF_SENTENCE}>
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => void turnOn()}>Turn on Accept device links</Button>
          </SettingRow>
        )}
        {started && !pairingOpen && (
          <div className="ck-set" style={{ alignItems: "flex-start", gap: 16 }}>
            {uri ? <QrCode text={uri} /> : <div className="error" role="alert">The engine did not report this computer's endpoint, so no QR can be drawn.</div>}
            <div className="ck-text">
              <strong>Scan with the Pulse app</strong>
              <div className="ck-sub">
                {`Open Pulse on your iPhone, choose Add a computer and scan this. The pairing ends at ${new Date(started.expiresAtMs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}. Anyone holding this QR can join until then, so keep the screen to yourself.`}
              </div>
              {!started.relayUrl && <div className="ck-sub">No relay is set, so the phone must be on the same network.</div>}
              <div className="ck-line" style={{ marginTop: 8 }}>
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => void cancel()}>Cancel pairing</Button>
              </div>
            </div>
          </div>
        )}
      </SettingsGroup>

      {access && access.phones.length > 0 && (
        <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Paired phones">
          {!access.enabled && (
            <div className="ck-set ck-sub" role="status">Phones are switched off (Let paired phones read and message my chats): every request is refused.</div>
          )}
          {access.phones.map((p) => (
            <SettingRow className="ck-set" key={p.deviceId} label={p.name}
              description={`Read lets it list and read your chats. Send lets it message them. New phones start at read ${access.defaults.read}, send ${access.defaults.send}.`}>
              {p.error ? (
                <Badge tone="warn">{`Policy unreadable (${p.error})`}</Badge>
              ) : (
                <div className="ck-line">
                  <label className="ck-sub">Read{" "}
                    <Select label={`Read access for ${p.name}`} value={p.read ?? "allow"} disabled={busy}
                      onChange={(v) => void setMode(p, { read: v as PhoneReadMode })}
                      options={[{ value: "allow", label: "Allow" }, { value: "deny", label: "Deny" }]} />
                  </label>
                  <label className="ck-sub">Send{" "}
                    <Select label={`Send access for ${p.name}`} value={p.send ?? "ask"} disabled={busy}
                      onChange={(v) => void setMode(p, { send: v as PhoneSendMode })}
                      options={[{ value: "allow", label: "Allow" }, { value: "ask", label: "Ask me" }, { value: "deny", label: "Deny" }]} />
                  </label>
                  <label className="ck-sub">Admin approvals{" "}
                    <Select label={`Admin approvals for ${p.name}`} value={p.admin ? "on" : "off"} disabled={busy}
                      onChange={(v) => (v === "on" ? void setAdmin(p, true) : setRevoking(p.deviceId))}
                      options={[{ value: "off", label: "Off" }, { value: "on", label: "On" }]} />
                  </label>
                  {revoking === p.deviceId && (
                    <div className="ck-line" role="alertdialog" aria-label="Revoke this phone's approvals?">
                      <strong>Revoke this phone's approvals?</strong>
                      <Button disabled={busy} onClick={() => void setAdmin(p, false)}>Approve</Button>
                      <Button disabled={busy} onClick={() => { setRevoking(null); setAdminNote("Approvals still work until revoked."); }}>Deny</Button>
                    </div>
                  )}
                  {adminNote && <div className="ck-sub" role="status">{adminNote}</div>}
                </div>
              )}
            </SettingRow>
          ))}
        </SettingsGroup>
      )}
    </>
  );
}
