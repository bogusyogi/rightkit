import { useEffect, useRef, useState } from "react";
import { Badge, Button, ConfirmDialog, Select } from "@rightkit/react-shell/react";
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui/core";
import { devicesApi, type GuestRecord, type JoinCreated, type OwnerProof } from "@rightkit/system-ui/api";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";
import { OwnerPinCard, onMac, ownerProblem, touchIdProof } from "./OwnerPin.js";

/** Settings > Devices > Guests (#492): one-time links for cloud boxes, the owner's approval of each
 *  box that joined with one, and revoke. An approved cloud agent works like the owner's other machines
 *  on every computer of the circle, but never adds or manages devices and has no phone admin path. */

/** Guest lease choices; the engine caps the lease at seven days. */
const LEASE: { label: string; ms: number }[] = [
  { label: "8 hours", ms: 8 * 60 * 60 * 1000 },
  { label: "1 day", ms: 24 * 60 * 60 * 1000 },
  { label: "7 days", ms: 7 * 24 * 60 * 60 * 1000 },
];

const POLL_MS = 3000;

function message(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : String(e);
}

const when = (ms: number): string =>
  new Date(ms).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

function countdown(ms: number, now: number): string {
  const s = Math.max(0, Math.round((ms - now) / 1000));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function describe(g: GuestRecord): string {
  // A guest another computer minted and approved is known here by introduction: it has no check
  // code on this computer, and the time is when this computer learned of it, not the approval.
  const introduced = g.checkCode === "";
  const parts = introduced ? ["minted and approved on another of your computers"] : [`code ${g.checkCode}`];
  if (g.reportedSession) parts.push(`reports itself as ${g.reportedSession}`);
  if (g.approvedAtMs != null) parts.push(`${introduced ? "learned here" : "approved"} ${when(g.approvedAtMs)}`);
  parts.push(`lease ends ${when(g.leaseExpiresAtMs)}`);
  parts.push(g.lastSeenMs != null ? `last seen ${when(g.lastSeenMs)}` : "not seen yet");
  return parts.join(" · ");
}

export function GuestsGroup({ busy, run }: { busy: boolean; run: (work: () => Promise<unknown>) => Promise<void> }) {
  const [guests, setGuests] = useState<GuestRecord[] | null>(null);
  const [link, setLink] = useState<JoinCreated | null>(null);
  const [lease, setLease] = useState(LEASE[1].ms);
  const [gate, setGate] = useState(false);
  const [waiting, setWaiting] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [linuxArch, setLinuxArch] = useState<string | null>(null);
  const [copiedLinux, setCopiedLinux] = useState(false);
  const [revoking, setRevoking] = useState<GuestRecord | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const touchTurn = useRef(0);

  const load = () => {
    devicesApi.guests()
      .then((answer) => setGuests(answer.guests))
      .catch(() => setGuests(null));
  };

  useEffect(() => {
    load();
    const timer = visibleInterval(load, POLL_MS);
    return () => clearVisibleInterval(timer);
  }, []);

  useEffect(() => {
    if (!link) return;
    const timer = visibleInterval(() => setNow(Date.now()), 1000);
    return () => clearVisibleInterval(timer);
  }, [link]);

  /** The owner check first (Touch ID on a Mac, else the the host app PIN), then one guest link. */
  const confirmMake = (pin?: string, proof?: OwnerProof) => run(async () => {
    try {
      const made = await devicesApi.guestCreate(lease, pin, proof);
      setLink(made);
      setCopied(false);
      setCopiedLinux(false);
      setLinuxArch(Object.keys(made.linux ?? {})[0] ?? null);
      setGate(false);
      setProblem(null);
    } catch (e) {
      const owner = ownerProblem(e);
      if (owner) { setProblem(owner); return; }
      setGate(false);
      throw e;
    }
  }).finally(load);

  const startMake = () => {
    setProblem(null);
    setGate(true);
    if (!onMac) {
      // Windows: the hub brings its window forward and Windows Hello signs inside the command
      // (`owner_checked_with`), so the guest link carries a signed grant every computer accepts.
      // Without Hello (or when it is not confirmed) the engine asks for the PIN, which stays open.
      const turn = ++touchTurn.current;
      setWaiting(true);
      void confirmMake().finally(() => {
        if (turn === touchTurn.current) setWaiting(false);
      });
      return;
    }
    const turn = ++touchTurn.current;
    setWaiting(true);
    void touchIdProof("join", null, { guest: true, leaseMs: lease }).then((proof) => {
      if (turn !== touchTurn.current) return;
      setWaiting(false);
      if (typeof proof === "string") setProblem(proof);
      else void confirmMake(undefined, proof);
    });
  };

  const cancelMake = () => run(async () => {
    await devicesApi.joinCancel();
    setLink(null);
  }).finally(load);

  const copyLink = () => {
    if (!link) return;
    void navigator.clipboard.writeText(link.command).then(() => setCopied(true)).catch(() => setCopied(false));
  };

  const copyLinux = () => {
    const entry = link?.linux && linuxArch ? link.linux[linuxArch] : null;
    if (!entry) return;
    void navigator.clipboard.writeText(entry.command).then(() => setCopiedLinux(true)).catch(() => setCopiedLinux(false));
  };

  // The link is done once a box redeemed it (a guest appears) or its ten minutes end.
  useEffect(() => {
    if (!link) return;
    if (now >= link.expiresAtMs) setLink(null);
  }, [link, now]);

  const approve = (g: GuestRecord) => run(() => devicesApi.guestApprove(g.deviceId)).finally(load);

  const confirmRevoke = () => {
    const g = revoking;
    setRevoking(null);
    if (g) void run(() => devicesApi.revoke(g.deviceId)).finally(load);
  };

  return (
    <>
      {gate && (
        <OwnerPinCard title="Make a guest link" busy={busy} problem={problem} waiting={waiting}
          via={onMac ? "Touch ID" : "Windows Hello"}
          note="A guest made with the PIN reaches only this computer, not your other computers. Use Touch ID or Windows Hello for one that reaches all of them."
          onSubmit={(pin) => void confirmMake(pin)}
          onCancel={() => { if (waiting) { touchTurn.current++; setWaiting(false); } else setGate(false); }} />
      )}

      <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Guests">
        <SettingRow className="ck-set"
          label="Make a guest link"
          description="For a cloud agent working for you: once approved it chats with every chat on your computers and sends and receives like them, until its lease ends. It never adds or manages devices. Asks for Touch ID or your Pulse PIN, then shows one command. It works once, for ten minutes. The box waits here until you approve it."
        >
          <div className="ck-line">
            <Select label="Guest lease" value={String(lease)} disabled={busy || link !== null}
              onChange={(v) => setLease(Number(v))}
              options={LEASE.map((l) => ({ value: String(l.ms), label: l.label }))} />
            <Button size="sm" variant="secondary" disabled={busy || link !== null} onClick={startMake}>Make a guest link</Button>
          </div>
        </SettingRow>
        {link && (
          <div className="ck-set" style={{ alignItems: "flex-start" }}>
            <div className="ck-text">
              <strong>Run this on the cloud box</strong>
              <div><code style={{ overflowWrap: "anywhere", userSelect: "all" }}>{link.command}</code></div>
              <div className="ck-sub">
                {`Works once. Anyone who has it can join as a guest until it expires (${countdown(link.expiresAtMs, now)} left), and can do nothing until you approve it below. The box prints a check code: approve only the guest showing the same code.`}
              </div>
              <div className="ck-sub" style={{ marginTop: 8 }}>
                {link.linux && linuxArch && link.linux[linuxArch] ? (
                  <>
                    <strong>Linux box (no Pulse installed yet)</strong>
                    <div className="ck-line">
                      <Select label="Linux architecture" value={linuxArch} onChange={(v) => { setLinuxArch(v); setCopiedLinux(false); }}
                        options={Object.keys(link.linux).map((a) => ({ value: a, label: a }))} />
                      <Button size="sm" variant="secondary" disabled={busy} onClick={copyLinux}>{copiedLinux ? "Copied" : "Copy Linux command"}</Button>
                    </div>
                    <div>{`Choose the architecture the box's uname -m prints, then paste in a shell on the box. It downloads over https, refuses unless the size (${link.linux[linuxArch].size} bytes) and sha256 (${link.linux[linuxArch].sha256}) match, checks for glibc ${link.linux[linuxArch].glibc} or newer, then installs and joins. The command contains the guest pass: treat it like the pass.`}</div>
                  </>
                ) : (
                  <div>{`No Linux install is offered: ${link.linuxUnavailable ?? "this computer's Pulse has no Linux build to offer"}. A box that already has Pulse uses the command above.`}</div>
                )}
              </div>
              <div className="ck-line" style={{ marginTop: 8 }}>
                <Button size="sm" variant="primary" disabled={busy} onClick={copyLink}>{copied ? "Copied" : "Copy"}</Button>
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => void cancelMake()}>Cancel</Button>
              </div>
            </div>
          </div>
        )}
        {guests?.map((g) => (
          <SettingRow className="ck-set" key={g.deviceId} label={g.name} description={describe(g)}>
            <div className="ck-line">
              <Badge tone={g.status === "approved" ? "ok" : "warn"}>
                {g.status === "approved" ? "Active" : g.status === "pending" ? "Waiting for you" : "Lapsed"}
              </Badge>
              {g.status === "pending" && (
                <Button size="sm" variant="primary" disabled={busy} onClick={() => void approve(g)}>{`Approve (code ${g.checkCode})`}</Button>
              )}
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => setRevoking(g)}>Revoke</Button>
            </div>
          </SettingRow>
        ))}
      </SettingsGroup>

      {revoking && (
        <ConfirmDialog danger title={`Revoke ${revoking.name}?`}
          description="The guest is cut off at once here and on every one of your computers that is online; one that is offline cuts it off when it next connects. A new guest link is needed to add it again."
          confirmLabel="Revoke" onConfirm={confirmRevoke} onCancel={() => setRevoking(null)} />
      )}
    </>
  );
}
