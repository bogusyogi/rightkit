import { useCallback, useEffect, useRef, useState } from "react";

import {
  parseRemoteCandidates,
  remoteClient,
  remoteRelativeTime,
  shortRemoteFingerprint,
  type RemoteClient,
  type RemoteHost,
} from "@rightkit/agent-ui-core/lib/remote/remoteClient";
import { hostAppName } from "../../appName.js";

export interface RemoteHostsPanelProps {
  open?: boolean;
  client?: RemoteClient;
}

type Reachability = { state: "reachable" | "unreachable"; detail?: string };

function describeProjects(projects: unknown): string {
  if (Array.isArray(projects)) return `${projects.length} project${projects.length === 1 ? "" : "s"}`;
  if (projects && typeof projects === "object") return `${Object.keys(projects).length} project fields`;
  return "project state returned";
}

export default function RemoteHostsPanel({
  open = true,
  client = remoteClient,
}: RemoteHostsPanelProps) {
  const [hosts, setHosts] = useState<RemoteHost[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [actionId, setActionId] = useState<string | null>(null);
  const [reachability, setReachability] = useState<Record<string, Reachability>>({});
  const [editingId, setEditingId] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [host, setHost] = useState("");
  const [fingerprint, setFingerprint] = useState("");
  const [code, setCode] = useState("");
  const [candidates, setCandidates] = useState("");
  const alive = useRef(true);

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      const nextHosts = await client.remoteHostsList();
      if (!alive.current) return;
      setHosts(Array.isArray(nextHosts) ? nextHosts : []);
      setError(null);
    } catch (reason) {
      if (alive.current) setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      if (alive.current) setLoading(false);
    }
  }, [client]);

  useEffect(() => {
    if (!open) return;
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5000);
    return () => window.clearInterval(timer);
  }, [open, refresh]);

  const runAction = useCallback(
    async (id: string, operation: () => Promise<void>) => {
      setActionId(id);
      try {
        await operation();
        await refresh();
      } catch (reason) {
        setError(reason instanceof Error ? reason.message : String(reason));
      } finally {
        if (alive.current) setActionId(null);
      }
    },
    [refresh],
  );

  const pairHost = useCallback(async () => {
    const trimmedHost = host.trim();
    const trimmedFingerprint = fingerprint.trim();
    const trimmedCode = code.trim();
    if (!trimmedHost || !trimmedFingerprint || !trimmedCode) {
      setError("Host, fingerprint, and pairing code are required.");
      return;
    }
    await runAction("pair", async () => {
      await client.remoteHostPair({
        host: trimmedHost,
        fingerprint: trimmedFingerprint,
        code: trimmedCode,
        candidates: parseRemoteCandidates(candidates),
      });
      setHost("");
      setFingerprint("");
      setCode("");
      setCandidates("");
    });
  }, [candidates, client, code, fingerprint, host, runAction]);

  const renameHost = useCallback(
    async (id: string) => {
      const trimmed = name.trim();
      if (!trimmed) return;
      await runAction(`rename:${id}`, async () => {
        await client.remoteHostRename(id, trimmed);
        setEditingId(null);
        setName("");
      });
    },
    [client, name, runAction],
  );

  const checkReachability = useCallback(
    async (id: string) => {
      setActionId(`check:${id}`);
      try {
        const snapshot = await client.remoteHostProjectSnapshot(id);
        if (alive.current) {
          setReachability((current) => ({
            ...current,
            [id]: { state: "reachable", detail: describeProjects(snapshot.projects) },
          }));
        }
      } catch (reason) {
        if (alive.current) {
          setReachability((current) => ({
            ...current,
            [id]: {
              state: "unreachable",
              detail: reason instanceof Error ? reason.message : String(reason),
            },
          }));
        }
      } finally {
        if (alive.current) setActionId(null);
      }
    },
    [client],
  );

  return (
    <section
      aria-label="Remote hosts"
      className="grid gap-3 rounded-xl border border-line bg-panel p-4"
      data-testid="remote-hosts-panel"
    >
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold text-text">Remote hosts</h2>
          <p className="mt-0.5 text-[12px] text-text-3">
            Pair another {hostAppName()} desktop and inspect its project reachability. Host tokens remain
            in the native store.
          </p>
        </div>
        <button
          type="button"
          aria-label="Refresh remote hosts"
          disabled={loading}
          onClick={() => void refresh()}
          className="h-8 rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
        >
          {loading ? "Refreshing…" : "Refresh"}
        </button>
      </header>

      {error ? (
        <p role="alert" className="rounded-lg border border-danger/40 bg-danger/10 p-2 text-[12px] text-danger">
          Remote host service unavailable: {error}
        </p>
      ) : null}

      <form
        className="grid gap-2 rounded-lg border border-line bg-bg p-3"
        onSubmit={(event) => {
          event.preventDefault();
          void pairHost();
        }}
        data-testid="remote-host-pair-form"
      >
        <div>
          <h3 className="text-[12px] font-semibold text-text">Pair a desktop host</h3>
          <p className="mt-0.5 text-[11px] text-text-3">
            Start pairing on the other host, then enter its address, fingerprint, and code.
          </p>
        </div>
        <div className="grid gap-2 sm:grid-cols-2">
          <input
            aria-label="Remote host address"
            placeholder="Host address"
            value={host}
            onChange={(event) => setHost(event.target.value)}
            className="h-8 rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
          />
          <input
            aria-label="Remote host fingerprint"
            placeholder="Certificate fingerprint"
            value={fingerprint}
            onChange={(event) => setFingerprint(event.target.value)}
            className="h-8 rounded-lg border border-line bg-panel px-2 font-mono text-[11px] text-text"
          />
          <input
            aria-label="Remote host pairing code"
            inputMode="numeric"
            placeholder="8-digit pairing code"
            value={code}
            onChange={(event) => setCode(event.target.value)}
            className="h-8 rounded-lg border border-line bg-panel px-2 font-mono text-[12px] text-text"
          />
          <input
            aria-label="Remote host candidate addresses"
            placeholder="Other addresses (comma-separated)"
            value={candidates}
            onChange={(event) => setCandidates(event.target.value)}
            className="h-8 rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
          />
        </div>
        <button
          type="submit"
          disabled={actionId === "pair"}
          className="h-8 justify-self-start rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
        >
          {actionId === "pair" ? "Pairing…" : "Pair host"}
        </button>
      </form>

      {hosts.length === 0 ? (
        <p className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text-3">
          No paired desktop hosts.
        </p>
      ) : (
        <ul className="grid gap-2" data-testid="remote-host-list">
          {hosts.map((remoteHost) => {
            const status = reachability[remoteHost.id];
            return (
              <li
                key={remoteHost.id}
                data-testid={`remote-host-${remoteHost.id}`}
                className="grid gap-2 rounded-lg border border-line bg-bg p-3"
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-semibold text-[12px] text-text">{remoteHost.displayName}</span>
                  <span className="text-[11px] text-text-3">{remoteHost.hostname}</span>
                  {status ? (
                    <span className={status.state === "reachable" ? "text-accent-2" : "text-danger"}>
                      {status.state}
                    </span>
                  ) : (
                    <span className="text-[11px] text-text-3">not checked</span>
                  )}
                </div>
                <div className="grid gap-1 text-[11px] text-text-3">
                  <code className="font-mono" title={remoteHost.fingerprint}>
                    cert {shortRemoteFingerprint(remoteHost.fingerprint)}
                  </code>
                  <span>
                    {remoteHost.candidates.length ? remoteHost.candidates.join(" · ") : "No address candidates"}
                  </span>
                  <span>last seen {remoteRelativeTime(remoteHost.lastSeenAt)}</span>
                  {status?.detail ? <span className="break-words">{status.detail}</span> : null}
                </div>
                {editingId === remoteHost.id ? (
                  <div className="flex flex-wrap gap-2">
                    <input
                      type="text"
                      aria-label={`New name for ${remoteHost.displayName}`}
                      value={name}
                      onChange={(event) => setName(event.target.value)}
                      className="h-8 min-w-[16ch] flex-1 rounded-lg border border-line bg-panel px-2 text-[12px] text-text"
                    />
                    <button
                      type="button"
                      disabled={!name.trim() || actionId === `rename:${remoteHost.id}`}
                      onClick={() => void renameHost(remoteHost.id)}
                      className="h-8 rounded-lg border border-accent px-3 text-[12px] text-text disabled:opacity-40"
                    >
                      Save name
                    </button>
                    <button
                      type="button"
                      onClick={() => setEditingId(null)}
                      className="h-8 rounded-lg border border-line px-3 text-[12px] text-text-2"
                    >
                      Cancel
                    </button>
                  </div>
                ) : (
                  <div className="flex flex-wrap gap-2">
                    <button
                      type="button"
                      disabled={actionId === `check:${remoteHost.id}`}
                      onClick={() => void checkReachability(remoteHost.id)}
                      className="h-7 rounded-md border border-accent bg-panel px-2 text-[11px] text-text disabled:opacity-40"
                    >
                      {actionId === `check:${remoteHost.id}` ? "Checking…" : "Check reachability"}
                    </button>
                    <button
                      type="button"
                      disabled={actionId === `reconnect:${remoteHost.id}`}
                      onClick={() =>
                        void runAction(`reconnect:${remoteHost.id}`, () =>
                          client.remoteHostReconnect(remoteHost.id),
                        )
                      }
                      className="h-7 rounded-md border border-line bg-panel px-2 text-[11px] text-text-2 hover:text-text disabled:opacity-40"
                    >
                      Reconnect
                    </button>
                    <button
                      type="button"
                      onClick={() => {
                        setEditingId(remoteHost.id);
                        setName(remoteHost.displayName);
                      }}
                      className="h-7 rounded-md border border-line bg-panel px-2 text-[11px] text-text-2 hover:text-text"
                    >
                      Rename
                    </button>
                    <button
                      type="button"
                      disabled={actionId === `forget:${remoteHost.id}`}
                      onClick={() =>
                        void runAction(`forget:${remoteHost.id}`, () =>
                          client.remoteHostForget(remoteHost.id),
                        )
                      }
                      className="h-7 rounded-md border border-danger/40 bg-panel px-2 text-[11px] text-danger hover:border-danger disabled:opacity-40"
                    >
                      Forget
                    </button>
                  </div>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

export { describeProjects };
