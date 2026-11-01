import { useCallback, useEffect, useRef, useState } from "react";

import {
  remoteClient,
  remoteRelativeTime,
  shortRemoteFingerprint,
  type PairingBegin,
  type RemoteClient,
  type RemoteDevice,
  type RemoteStatus,
} from "@rightkit/agent-ui-core/lib/remote/remoteClient";

export interface RemoteDevicesPanelProps {
  /** The settings tab controls polling by passing its visibility. */
  open?: boolean;
  client?: RemoteClient;
}

function PairingCode({ pairing }: { pairing: PairingBegin }) {
  const [secondsLeft, setSecondsLeft] = useState(() =>
    Math.max(0, Math.ceil((pairing.expiresAt - Date.now()) / 1000)),
  );

  useEffect(() => {
    const timer = window.setInterval(() => {
      setSecondsLeft(Math.max(0, Math.ceil((pairing.expiresAt - Date.now()) / 1000)));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [pairing.expiresAt]);

  return (
    <div className="grid gap-2 rounded-lg border border-accent/40 bg-[color-mix(in_srgb,var(--color-accent)_6%,transparent)] p-3" data-testid="remote-pairing">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-wide text-text-3">
          Pairing code
        </span>
        <span className="font-mono text-lg font-bold tracking-[0.2em] text-text" data-testid="remote-pairing-code">
          {pairing.code}
        </span>
      </div>
      <p className="text-[11px] text-text-2">
        Enter this code in the companion app. It expires in {secondsLeft}s and is single-use.
      </p>
      <details className="rounded border border-line bg-bg px-2 py-1">
        <summary className="cursor-pointer text-[11px] text-text-3">Show pairing payload</summary>
        <code className="mt-2 block max-h-24 overflow-auto break-all whitespace-pre-wrap font-mono text-[10px] text-text-3">
          {pairing.qrPayload}
        </code>
      </details>
    </div>
  );
}

function useRemoteDevices(client: RemoteClient, open: boolean) {
  const [status, setStatus] = useState<RemoteStatus | null>(null);
  const [devices, setDevices] = useState<RemoteDevice[]>([]);
  const [pairing, setPairing] = useState<PairingBegin | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const pairingBaseline = useRef<Set<string> | null>(null);
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    try {
      const [nextStatus, nextDevices] = await Promise.all([
        client.remoteStatus(),
        client.remoteDevicesList(),
      ]);
      if (!alive.current) return;
      const list = Array.isArray(nextDevices) ? nextDevices : [];
      setStatus(nextStatus);
      setDevices(list);
      setError(null);
      const baseline = pairingBaseline.current;
      if (baseline && list.some((device) => !baseline.has(device.id))) {
        pairingBaseline.current = null;
        setPairing(null);
      }
    } catch (reason) {
      if (alive.current) setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [client]);

  useEffect(() => {
    if (!open) return;
    void refresh();
    const timer = window.setInterval(() => void refresh(), pairing ? 1500 : 5000);
    return () => window.clearInterval(timer);
  }, [open, pairing, refresh]);

  useEffect(() => {
    if (!pairing) return;
    const timer = window.setTimeout(() => {
      pairingBaseline.current = null;
      if (alive.current) setPairing(null);
    }, Math.max(0, pairing.expiresAt - Date.now()));
    return () => window.clearTimeout(timer);
  }, [pairing]);

  const beginPairing = useCallback(async () => {
    setBusy(true);
    try {
      const begun = await client.remotePairingBegin();
      pairingBaseline.current = new Set(devices.map((device) => device.id));
      setPairing(begun);
      setError(null);
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  }, [client, devices, refresh]);

  const cancelPairing = useCallback(async () => {
    pairingBaseline.current = null;
    setPairing(null);
    try {
      await client.remotePairingCancel();
      await refresh();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    }
  }, [client, refresh]);

  const revoke = useCallback(
    async (id: string) => {
      try {
        await client.remoteDeviceRevoke(id);
        await refresh();
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    },
    [client, refresh],
  );

  const rename = useCallback(
    async (id: string) => {
      const trimmed = name.trim();
      if (!trimmed) return;
      try {
        await client.remoteDeviceRename(id, trimmed);
        setEditingId(null);
        setName("");
        await refresh();
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
      }
    },
    [client, name, refresh],
  );

  return {
    status,
    devices,
    pairing,
    error,
    busy,
    editingId,
    name,
    setEditingId,
    setName,
    refresh,
    beginPairing,
    cancelPairing,
    revoke,
    rename,
  };
}

export default function RemoteDevicesPanel({
  open = true,
  client = remoteClient,
}: RemoteDevicesPanelProps) {
  const remote = useRemoteDevices(client, open);
  const canPair = remote.status?.running === true && remote.status.tlsActive === true;
  const gatewayLabel =
    remote.status === null
      ? "Not loaded"
      : !remote.status.running
        ? "Server stopped"
        : remote.status.tlsActive
          ? `Secure on port ${remote.status.port ?? "?"}`
          : `Insecure (dev) on port ${remote.status.port ?? "?"}`;

  return (
    <section
      aria-label="Remote devices"
      className="grid gap-3 rounded-xl border border-line bg-panel p-4"
      data-testid="remote-devices-panel"
    >
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold text-text">Remote devices</h2>
          <p className="mt-0.5 text-[12px] text-text-3">
            Pair a phone or companion client with this host. Tokens stay native and are never
            rendered here.
          </p>
        </div>
        <button
          type="button"
          aria-label="Refresh remote devices"
          onClick={() => void remote.refresh()}
          className="h-8 rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text"
        >
          Refresh
        </button>
      </header>

      {remote.error ? (
        <p role="alert" className="rounded-lg border border-danger/40 bg-danger/10 p-2 text-[12px] text-danger">
          Remote device service unavailable: {remote.error}
        </p>
      ) : null}

      <div className="flex flex-wrap items-center gap-2 text-[12px]">
        <span className="text-text-3">Gateway</span>
        <span data-testid="remote-gateway-status" className="font-medium text-text-2">
          {gatewayLabel}
        </span>
        {remote.status?.fingerprint ? (
          <code className="rounded border border-line bg-bg px-2 py-1 font-mono text-[10px] text-text-3" title={remote.status.fingerprint}>
            cert {shortRemoteFingerprint(remote.status.fingerprint)}
          </code>
        ) : null}
      </div>

      {!canPair ? (
        <p className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text-3">
          {remote.status && !remote.status.running
            ? "Start the server before pairing a device."
            : "Pairing is unavailable until this host has a TLS identity."}
        </p>
      ) : remote.pairing ? (
        <>
          <PairingCode pairing={remote.pairing} />
          <button
            type="button"
            onClick={() => void remote.cancelPairing()}
            className="h-8 justify-self-start rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text"
          >
            Cancel pairing
          </button>
        </>
      ) : (
        <button
          type="button"
          disabled={remote.busy}
          onClick={() => void remote.beginPairing()}
          className="h-8 justify-self-start rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
        >
          {remote.busy ? "Starting pairing…" : "Pair a device"}
        </button>
      )}

      <div className="grid gap-2" data-testid="remote-device-list">
        {remote.devices.length === 0 ? (
          <p className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text-3">
            No paired devices.
          </p>
        ) : (
          <ul className="grid gap-2">
            {remote.devices.map((device) => (
              <li
                key={device.id}
                data-testid={`remote-device-${device.id}`}
                className="grid gap-2 rounded-lg border border-line bg-bg p-3"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-semibold text-[12px] text-text">{device.name}</span>
                  <span className={device.connected ? "text-accent-2" : "text-text-3"}>
                    {device.connected ? "connected" : "not connected"}
                  </span>
                  <span className="text-[11px] text-text-3">
                    {device.platform} · seen {remoteRelativeTime(device.lastSeenAt)}
                  </span>
                  {device.revoked ? <span className="text-[11px] text-danger">revoked</span> : null}
                </div>
                {remote.editingId === device.id ? (
                  <div className="flex flex-wrap gap-2">
                    <input
                      type="text"
                      aria-label={`New name for ${device.name}`}
                      value={remote.name}
                      onChange={(event) => remote.setName(event.target.value)}
                      className="h-8 min-w-[16ch] flex-1 rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
                    />
                    <button
                      type="button"
                      disabled={!remote.name.trim()}
                      onClick={() => void remote.rename(device.id)}
                      className="h-8 rounded-lg border border-accent px-3 text-[12px] text-text disabled:opacity-40"
                    >
                      Save name
                    </button>
                    <button
                      type="button"
                      onClick={() => remote.setEditingId(null)}
                      className="h-8 rounded-lg border border-line px-3 text-[12px] text-text-2"
                    >
                      Cancel
                    </button>
                  </div>
                ) : (
                  <div className="flex flex-wrap gap-2">
                    {!device.revoked ? (
                      <>
                        <button
                          type="button"
                          onClick={() => {
                            remote.setEditingId(device.id);
                            remote.setName(device.name);
                          }}
                          className="h-7 rounded-md border border-line bg-panel px-2 text-[11px] text-text-2 hover:text-text"
                        >
                          Rename
                        </button>
                        <button
                          type="button"
                          onClick={() => void remote.revoke(device.id)}
                          className="h-7 rounded-md border border-danger/40 bg-panel px-2 text-[11px] text-danger hover:border-danger"
                        >
                          Revoke
                        </button>
                      </>
                    ) : null}
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>
    </section>
  );
}
