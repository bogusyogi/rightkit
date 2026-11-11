import { Fragment, useCallback, useEffect, useRef, useState, type KeyboardEvent, type MouseEvent, type WheelEvent } from "react";
import { Button, Input } from "@rightkit/react-shell/react";
import { SettingRow, SettingsGroup, SettingsItem } from "@rightkit/settings-ui/core";
import { browserRemoteApi, browserRows, type BrowserFrame, type BrowserInput, type BrowserRequest, type BrowserSession } from "../api.js";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";

/** Settings > Devices, under the paired computers: the sign-ins an agent asked for on each one, and the live view of a session. */

/** How often the lists are re-read while Devices is open. */
const REFRESH_MS = 5000;
/** About four frames a second, as the engine's `browser.frame` is meant to be polled. */
const FRAME_MS = 250;
/** Wheel movement is sent in batches, so a fast scroll is one input, not one per notch. */
const WHEEL_MS = 120;

/** A paired computer as this view needs it: its id for the engine and its name for the page. */
export interface BrowserDevice {
  id: string;
  name: string;
}

interface DeviceLogins {
  requests: BrowserRequest[];
  sessions: BrowserSession[];
  problem: string | null;
}

const message = (e: unknown): string => (typeof e === "string" ? e : e instanceof Error ? e.message : String(e));

const sitesText = (sites: string[] | undefined): string => (sites && sites.length > 0 ? sites.join(", ") : "a site");

/** The keys the live view forwards from the keyboard while its frame has focus. */
const FORWARDED_KEYS = new Set(["Tab", "Backspace", "Enter"]);

/**
 * The live view of one session that needs the owner: a frame polled about four times a second, clicks
 * and wheel movement mapped to frame pixels, a text field and Done. Typed text lives in this component's
 * state only while it is typed; it is sent once and cleared, and it is never logged.
 */
function LiveSession({ device, session, onDone }: { device: string; session: string; onDone: () => void }) {
  const [frame, setFrame] = useState<BrowserFrame | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [reveal, setReveal] = useState(false);
  const seq = useRef(0);
  const inFlight = useRef(false);
  const imageRef = useRef<HTMLImageElement>(null);
  const wheelDy = useRef(0);
  const wheelTimer = useRef<number | null>(null);

  useEffect(() => {
    seq.current = 0;
    const tick = () => {
      if (inFlight.current) return;
      inFlight.current = true;
      browserRemoteApi
        .frame(device, session, seq.current)
        .then((next) => {
          setProblem(null);
          if (next && next.seq !== seq.current) {
            seq.current = next.seq;
            setFrame(next);
          }
        })
        .catch((e: unknown) => setProblem(message(e)))
        .finally(() => {
          inFlight.current = false;
        });
    };
    tick();
    const timer = visibleInterval(tick, FRAME_MS);
    return () => clearVisibleInterval(timer);
  }, [device, session]);

  useEffect(() => () => {
    if (wheelTimer.current !== null) window.clearTimeout(wheelTimer.current);
  }, []);

  const send = (events: BrowserInput[]) => {
    browserRemoteApi.input(device, session, events).catch((e: unknown) => setProblem(message(e)));
  };

  /** A click maps the displayed picture back to frame pixels, so it lands where the page is. */
  const onClick = (e: MouseEvent<HTMLImageElement>) => {
    const img = imageRef.current;
    if (!img || !frame) return;
    const box = img.getBoundingClientRect();
    if (box.width <= 0 || box.height <= 0) return;
    const x = Math.round(((e.clientX - box.left) / box.width) * frame.width);
    const y = Math.round(((e.clientY - box.top) / box.height) * frame.height);
    send([{ type: "click", x, y }]);
  };

  const onWheel = (e: WheelEvent<HTMLDivElement>) => {
    wheelDy.current += e.deltaY;
    if (wheelTimer.current !== null) return;
    wheelTimer.current = window.setTimeout(() => {
      const dy = Math.round(wheelDy.current);
      wheelDy.current = 0;
      wheelTimer.current = null;
      if (dy !== 0) send([{ type: "scroll", dy }]);
    }, WHEEL_MS);
  };

  /** Tab, Backspace and Enter pressed while the picture has focus go to the page. */
  const onFrameKey = (e: KeyboardEvent<HTMLDivElement>) => {
    if (!FORWARDED_KEYS.has(e.key)) return;
    e.preventDefault();
    send([{ type: "key", key: e.key }]);
  };

  /** Enter sends the typed text, then Enter; the field is cleared at once. */
  const sendText = () => {
    const typed = text;
    setText("");
    send(typed === "" ? [{ type: "key", key: "Enter" }] : [{ type: "text", text: typed }, { type: "key", key: "Enter" }]);
  };

  const finish = () => {
    browserRemoteApi
      .done(device, session)
      .catch((e: unknown) => setProblem(message(e)))
      .finally(onDone);
  };

  return (
    <div className="ck-set ck-stack" aria-label="Live view of the sign-in">
      <div className="ck-sub">{frame ? [frame.title, frame.url].filter(Boolean).join(" · ") : "Waiting for the first picture…"}</div>
      <div
        tabIndex={0}
        role="application"
        aria-label="Sign-in page; click to press, scroll to move the page"
        onKeyDown={onFrameKey}
        onWheel={onWheel}
        style={{ width: "100%", outline: "none" }}
      >
        {frame && (
          <img
            ref={imageRef}
            alt="Sign-in page"
            src={`data:image/jpeg;base64,${frame.jpeg_b64}`}
            onClick={onClick}
            draggable={false}
            style={{ display: "block", width: "100%", height: "auto", cursor: "pointer", borderRadius: 6 }}
          />
        )}
      </div>
      {problem !== null && <div className="ck-sub" role="status">{problem}</div>}
      <div className="ck-line">
        <Input className="ck-input" aria-label="Text for the page" placeholder="Type here, then Enter"
          type={reveal ? "text" : "password"} autoComplete="off" autoCorrect="off" autoCapitalize="off" spellCheck={false}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter") { e.preventDefault(); sendText(); } }} />
        <Button size="sm" variant="secondary" aria-pressed={reveal} onClick={() => setReveal((on) => !on)}>{reveal ? "Hide" : "Show"}</Button>
        <Button size="sm" variant="secondary" onClick={() => send([{ type: "key", key: "Tab" }])}>Tab</Button>
        <Button size="sm" variant="secondary" onClick={() => send([{ type: "key", key: "Backspace" }])}>Backspace</Button>
        <Button size="sm" variant="primary" onClick={finish}>Done</Button>
      </div>
    </div>
  );
}

/**
 * Browser logins for each paired computer: the sign-ins agents asked for (sites, chat, reason, Approve and
 * Deny) and the sessions open on it. A session that needs the owner shows its live view here. Shown only
 * while the `agentBrowser` setting is on; the caller decides.
 */
export function BrowserLogins({ devices }: { devices: BrowserDevice[] }) {
  const [logins, setLogins] = useState<Record<string, DeviceLogins>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const key = devices.map((d) => d.id).join("\n");

  const load = useCallback(() => {
    const ids = key === "" ? [] : key.split("\n");
    for (const id of ids) {
      Promise.all([browserRemoteApi.pending(id), browserRemoteApi.sessions(id)])
        .then(([pending, sessions]) => {
          setLogins((all) => ({
            ...all,
            [id]: { requests: browserRows<BrowserRequest>(pending, "requests"), sessions: browserRows<BrowserSession>(sessions, "sessions"), problem: null },
          }));
        })
        .catch((e: unknown) => {
          setLogins((all) => ({ ...all, [id]: { requests: [], sessions: [], problem: message(e) } }));
        });
    }
  }, [key]);

  useEffect(() => {
    load();
    const timer = visibleInterval(load, REFRESH_MS);
    return () => clearVisibleInterval(timer);
  }, [load]);

  /** Approve asks the engine for a nonce, then approves with it; the engine runs the owner check. */
  const approve = (device: string, request: BrowserRequest) => {
    setBusy(request.id);
    browserRemoteApi
      .challenge(device, request.id)
      .then((c) => {
        if (!c?.nonce) throw new Error("The engine gave no approval nonce.");
        return browserRemoteApi.approve(device, request.id, c.nonce);
      })
      .catch((e: unknown) => {
        setLogins((all) => ({ ...all, [device]: { ...(all[device] ?? { requests: [], sessions: [] }), problem: message(e) } }));
      })
      .finally(() => {
        setBusy(null);
        load();
      });
  };

  const deny = (device: string, request: BrowserRequest) => {
    setBusy(request.id);
    browserRemoteApi
      .deny(device, request.id)
      .catch((e: unknown) => {
        setLogins((all) => ({ ...all, [device]: { ...(all[device] ?? { requests: [], sessions: [] }), problem: message(e) } }));
      })
      .finally(() => {
        setBusy(null);
        load();
      });
  };

  return (
    <SettingsGroup headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Browser logins">
      <SettingRow className="ck-set" label="Sign-ins for agents"
        description="An agent asks to sign in to named sites on one of your computers. Nothing is used until you approve it here or on the notch. A session reaches only those sites, lasts a limited time and ends when you press Done.">
        <Button size="sm" variant="ghost" onClick={load}>Refresh</Button>
      </SettingRow>
      {devices.length === 0 && (
        <SettingRow className="ck-set" label="No paired computers" description="Pair the computer that keeps the logins to see its sign-ins here.">{null}</SettingRow>
      )}
      {devices.map((device) => {
        const state = logins[device.id];
        const count = (state?.requests.length ?? 0) + (state?.sessions.length ?? 0);
        return (
          <Fragment key={device.id}>
            <SettingRow className="ck-set" label={device.name}
              description={state?.problem ? state.problem : state === undefined ? "Reading sign-ins…" : count === 0 ? "No sign-in is waiting or open." : `${count} waiting or open`}>
              {null}
            </SettingRow>
            {state?.requests.map((request) => (
              <SettingsItem key={request.id} text={`${sitesText(request.sites)} for ${request.chat || "an agent"}`} className="ck-set ck-stack">
                <div className="ck-text">
                  <strong>{`${sitesText(request.sites)} for ${request.chat || "an agent"}`}</strong>
                  {request.reason && <div className="ck-sub">{request.reason}</div>}
                </div>
                <div className="ck-ctl">
                  <Button size="sm" variant="primary" disabled={busy !== null} onClick={() => approve(device.id, request)}>Approve</Button>
                  <Button size="sm" variant="secondary" disabled={busy !== null} onClick={() => deny(device.id, request)}>Deny</Button>
                </div>
              </SettingsItem>
            ))}
            {state?.sessions.map((session) => {
              const owner = session.state === "needs-owner";
              return (
                <SettingsItem key={session.session} text={`${sitesText(session.sites)} ${session.state}`} className="ck-set ck-stack">
                  <div className="ck-text">
                    <strong>{`${sitesText(session.sites)} for ${session.chat || "an agent"}`}</strong>
                    <span className="ck-sub">
                      {owner ? "Needs you: sign in below, then press Done." : `The agent is using this session (${session.state}).`}
                    </span>
                  </div>
                  {owner && (
                    <LiveSession device={device.id} session={session.session} onDone={load} />
                  )}
                </SettingsItem>
              );
            })}
          </Fragment>
        );
      })}
    </SettingsGroup>
  );
}
