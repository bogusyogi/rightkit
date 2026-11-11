// Meet — a landing surface (parity with meet.google.com's home): start an
// instant meeting, join with a code, email a link, or schedule in Calendar.
// The live video call is Google's WebRTC client, so starting/joining launches it
// externally (openExternal) — every desktop client does this.
import { useState } from "react";
import { Icon } from "../icons.js";

import { Button, Input } from "@rightkit/react-shell/react";
import { useWorkspaceClient, useWorkspaceHost, useWorkspaceState } from "../host.js";
/** Accept a raw code ("abc-defg-hij"), a full URL, or a code with spaces. */
function meetUrlFromCode(input: string): string | null {
  const s = input.trim();
  if (!s) return null;
  if (/^https?:\/\//i.test(s)) return s;
  const code = s.replace(/^meet\.google\.com\//i, "").replace(/\s+/g, "");
  if (!/^[a-z0-9-]{3,}$/i.test(code)) return null;
  return `https://meet.google.com/${code}`;
}

export function MeetView() {
  const { createMeet, openExternal } = useWorkspaceClient();
  const activeAccount = useWorkspaceState((s) => s.activeAccount);
  const setErr = useWorkspaceHost().reportError;
  const showUndo = useWorkspaceHost().notify;
  const openComposeTo = useWorkspaceHost().composeTo;
  const setEventSeed = useWorkspaceState((s) => s.setEventSeed);
  const setView = useWorkspaceHost().openView;
  const [code, setCode] = useState("");
  const [starting, setStarting] = useState(false);

  async function startInstant() {
    if (!activeAccount || starting) return;
    setStarting(true);
    try {
      const sp = await createMeet(activeAccount);
      if (sp.meeting_uri) {
        await openExternal(sp.meeting_uri);
        showUndo(`Meet ready · ${sp.meeting_code}`);
      }
    } catch (e) {
      setErr(String(e));
    } finally {
      setStarting(false);
    }
  }
  async function emailLink() {
    if (!activeAccount) return;
    try {
      const sp = await createMeet(activeAccount);
      const uri = sp.meeting_uri || "";
      const body =
        `<p>You're invited to a Google Meet video call.</p>` +
        (uri ? `<p>Join: <a href="${uri}">${uri}</a></p>` : "") +
        (sp.meeting_code ? `<p>Meeting code: ${sp.meeting_code}</p>` : "");
      openComposeTo("", "Meeting invitation", body);
      showUndo("Meeting link added to a new email");
    } catch (e) {
      setErr(String(e));
    }
  }
  function join() {
    const url = meetUrlFromCode(code);
    if (!url) {
      setErr("Enter a valid meeting code or link (e.g. abc-defg-hij).");
      return;
    }
    openExternal(url).catch((e) => setErr(String(e)));
  }
  function schedule() {
    setEventSeed({ summary: "Meeting", withMeet: true });
    setView("calendar");
  }

  const disabled = !activeAccount;

  return (
    <div className="meetview">
      <div className="meet-hero">
        <div className="meet-mark"><Icon name="video" size={40} /></div>
        <h1 className="meet-title">Video calls and meetings for everyone</h1>
        <p className="meet-sub">Connect, collaborate, and celebrate from anywhere with Google Meet. The call opens in your browser, where you're already signed in.</p>

        <div className="meet-actions">
          <Button variant="primary" className="meet-start" disabled={disabled || starting} onClick={startInstant}>
            <Icon name="video" size={18} /> {starting ? "Creating…" : "New meeting"}
          </Button>
          <div className="meet-join">
            <Icon name="people" size={18} className="meet-join-ic" />
            <Input
              className="meet-code"
              placeholder="Enter a code or link"
              value={code}
              onChange={(e) => setCode(e.target.value)}
              onKeyDown={(e) => { if (e.key === "Enter") join(); }}
            />
            <Button variant="ghost" size="sm" className="meet-join-btn" disabled={!code.trim()} onClick={join}>Join</Button>
          </div>
        </div>

        <div className="meet-secondary">
          <Button variant="ghost" disabled={disabled} onClick={schedule}>
            <Icon name="calendar" size={16} /> Schedule in Calendar
          </Button>
          <Button variant="ghost" disabled={disabled} onClick={emailLink}>
            <Icon name="draft" size={16} /> Email a meeting link
          </Button>
        </div>
      </div>
    </div>
  );
}
