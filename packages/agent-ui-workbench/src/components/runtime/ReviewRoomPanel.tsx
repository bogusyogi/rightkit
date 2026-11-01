import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  FINDING_GROUPS,
  isCouncilJoinLink,
  isTerminalFinding,
  receiptAnchor,
  type RoomDisposition,
  type RoomEvent,
  type RoomSnapshot,
} from "./reviewRoom";

export default function ReviewRoomPanel() {
  const [joinLink, setJoinLink] = useState("");
  const [snapshot, setSnapshot] = useState<RoomSnapshot | null>(null);
  const [draft, setDraft] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const refresh = useCallback(async () => {
    setSnapshot(await invoke<RoomSnapshot>("agent_room_status"));
  }, []);

  useEffect(() => {
    let disposed = false;
    let stopEvent: (() => void) | undefined;
    let stopDisconnect: (() => void) | undefined;
    void listen<RoomEvent>("agent-room-event", () => {
      if (!disposed) void refresh().catch((reason) => setError(String(reason)));
    }).then((stop) => {
      stopEvent = stop;
    });
    void listen<string>("agent-room-disconnected", (event) => {
      if (!disposed) setError(event.payload);
    }).then((stop) => {
      stopDisconnect = stop;
    });
    return () => {
      disposed = true;
      stopEvent?.();
      stopDisconnect?.();
    };
  }, [refresh]);

  const dispositionById = useMemo(
    () => new Map((snapshot?.dispositions || []).map((item) => [item.disposition_id, item])),
    [snapshot?.dispositions],
  );

  async function join() {
    setPending(true);
    setError(null);
    try {
      setSnapshot(await invoke<RoomSnapshot>("agent_room_join", { joinUrl: joinLink }));
      setJoinLink("");
    } catch (reason) {
      setError(String(reason));
    } finally {
      setPending(false);
    }
  }

  async function say() {
    const body = draft.trim();
    if (!body) return;
    setPending(true);
    setError(null);
    try {
      await invoke("agent_room_say", { body });
      setDraft("");
      await refresh();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setPending(false);
    }
  }

  if (!snapshot) {
    return (
      <div className="flex-1 overflow-y-auto p-6">
        <div className="mx-auto max-w-xl rounded-xl border border-line bg-panel p-5 space-y-4">
          <div>
            <h3 className="text-[14px] font-semibold text-text">Join Review Room</h3>
            <p className="mt-1 text-[12px] text-text-3">
              Rooms run automatically. Paste the private link only when you want to watch or
              intervene here.
            </p>
          </div>
          <label className="block text-[12px] text-text-2">
            Private room link
            <input
              className="mt-1 w-full rounded-lg border border-line bg-canvas px-3 py-2 text-text outline-none"
              type="url"
              autoComplete="off"
              placeholder="http://127.0.0.1:43119/join/…"
              value={joinLink}
              onChange={(event) => setJoinLink(event.target.value)}
            />
          </label>
          <button
            className="rounded-lg bg-accent px-3 py-2 text-[12px] font-semibold text-white disabled:opacity-50"
            type="button"
            disabled={pending || !isCouncilJoinLink(joinLink)}
            onClick={() => void join()}
          >
            Join room
          </button>
          {error && (
            <p role="alert" className="text-[12px] text-danger">
              {error}
            </p>
          )}
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 min-h-0 overflow-y-auto p-5 space-y-5">
      <header className="flex items-center justify-between gap-3">
        <div>
          <h3 className="text-[14px] font-semibold text-text">{snapshot.room_id}</h3>
          <p className="text-[12px] text-text-3">
            {snapshot.phase} · escalation {(snapshot.escalation_rate * 100).toFixed(0)}%
          </p>
        </div>
        <button
          className="text-[12px] text-text-2 hover:text-text"
          type="button"
          onClick={() => void refresh()}
        >
          Refresh
        </button>
      </header>
      <section className="grid gap-3 md:grid-cols-3" aria-label="Room seats">
        {snapshot.seats.map((seat) => (
          <article key={seat.seat_id} className="rounded-xl border border-line bg-panel p-3">
            <div className="font-semibold text-text">{seat.name}</div>
            <div className="text-[11px] text-text-3">
              {seat.model} · {seat.role}
            </div>
            <div className="mt-2 text-[11px] text-text-2">
              {seat.usage.sent_messages}/{seat.budget.max_messages} messages · {seat.usage.wakes}/
              {seat.budget.max_wakes} wakes
            </div>
            <div className="text-[11px] text-text-3">
              {seat.usage.context_bytes}/{seat.budget.max_context_bytes} context bytes
            </div>
          </article>
        ))}
      </section>
      <section className="rounded-xl border border-line bg-panel p-4">
        <h4 className="text-[12px] font-semibold text-text">Human interjection</h4>
        {snapshot.sealed && <p className="mt-1 text-[11px] text-text-3">Room sealed</p>}
        <textarea
          aria-label="Ask, challenge, or rule on the room"
          className="mt-2 min-h-20 w-full resize-y rounded-lg border border-line bg-canvas px-3 py-2 text-[12px] text-text outline-none"
          placeholder="Ask, challenge, or rule on the room"
          value={draft}
          disabled={snapshot.sealed}
          onChange={(event) => setDraft(event.target.value)}
        />
        <button
          className="mt-2 rounded-lg bg-accent px-3 py-2 text-[12px] font-semibold text-white disabled:opacity-50"
          type="button"
          disabled={snapshot.sealed || pending || !draft.trim()}
          onClick={() => void say()}
        >
          Send to room
        </button>
        {error && (
          <p role="alert" className="mt-2 text-[12px] text-danger">
            {error}
          </p>
        )}
      </section>
      <section>
        <h4 className="text-[12px] font-semibold text-text">Finding ledger</h4>
        <div className="mt-2 grid gap-3 xl:grid-cols-5">
          {FINDING_GROUPS.map((group) => (
            <div key={group.status} className="rounded-xl border border-line bg-panel p-3">
              <div className="text-[11px] font-semibold uppercase tracking-wide text-text-3">
                {group.label}
              </div>
              <div className="mt-2 space-y-2">
                {snapshot.findings
                  .filter((finding) => finding.status === group.status)
                  .map((finding) => {
                    const disposition = finding.disposition_id
                      ? dispositionById.get(finding.disposition_id)
                      : undefined;
                    return (
                      <article
                        key={finding.finding_id}
                        className={`rounded-lg border p-2 text-[11px] ${isTerminalFinding(finding) ? "border-success/40" : "border-line"}`}
                      >
                        <p className="text-text">{finding.claim}</p>
                        {isTerminalFinding(finding) && disposition && (
                          <ReceiptLinks disposition={disposition} />
                        )}
                      </article>
                    );
                  })}
              </div>
            </div>
          ))}
        </div>
      </section>
      <section className="rounded-xl border border-line bg-canvas p-4">
        <h4 className="text-[12px] font-semibold text-text">Live transcript</h4>
        <ol className="mt-2 space-y-2 font-mono text-[11px] text-text-2">
          {snapshot.events.map((event) => (
            <li key={event.seq}>
              <span className="text-text-3">
                #{event.seq} {event.seat_id} · {event.kind}
              </span>{" "}
              {event.body}
            </li>
          ))}
        </ol>
      </section>
    </div>
  );
}

function ReceiptLinks({ disposition }: { disposition: RoomDisposition }) {
  return (
    <ul className="mt-2 space-y-1 text-text-3">
      {disposition.receipt_refs.map((receipt, index) => {
        const anchor = receiptAnchor(receipt, index);
        return (
          <li id={anchor} key={anchor}>
            <a
              className="underline hover:text-text"
              href={`#${anchor}`}
              title={receipt.digest || undefined}
            >
              {receipt.kind}: {receipt.locator}
              {receipt.line ? `:${receipt.line}` : ""}
            </a>
          </li>
        );
      })}
    </ul>
  );
}
