import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { Button, Toggle, Input } from "@rightkit/react-shell/react";
import { isWindows } from "@rightkit/system-ui/api";
import { onProtectedPin, remoteUnlockApi, setProtectedSetting, type PinAsk } from "../api.js";
import { SettingRow, SettingsGroup, SettingsItem } from "@rightkit/settings-ui/core";
import { RemoteApprovals } from "./RemoteApprovals.js";

type Published = Record<string, unknown>;
type SetFn = (key: string, value: unknown) => void;

const isNum = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);

/** A list the notch publishes: an array, or JSON text of one. */
const asList = (v: unknown): string[] => {
  if (Array.isArray(v)) return v.map(String);
  if (typeof v === "string" && v.trim() !== "") {
    try {
      const parsed: unknown = JSON.parse(v);
      return Array.isArray(parsed) ? parsed.map(String) : [];
    } catch {
      return [];
    }
  }
  return [];
};

/** A switch row, hidden when the notch did not publish its key. */
function SwitchRow({ s, set, k, label, note, disabled }: {
  s: Published;
  set: SetFn;
  k: string;
  label: string;
  note: string;
  disabled?: boolean;
}) {
  if (!(k in s)) return null;
  return (
    <SettingRow className="ck-set" label={label} description={note}>
      <Toggle checked={Boolean(s[k])} disabled={disabled} onChange={(v) => set(k, v)} label={label} />
    </SettingRow>
  );
}

/** A whole-number setting, saved when the field loses focus or Enter is pressed; out-of-range values snap back. Hidden when unpublished. */
export function NumberRow({ s, set, k, label, note, min, max }: {
  s: Published;
  set: SetFn;
  k: string;
  label: string;
  note: string;
  min: number;
  max: number;
}) {
  const published = s[k];
  const [draft, setDraft] = useState<string | null>(null);
  if (!isNum(published)) return null;
  const commit = () => {
    const n = Math.round(Number(draft));
    setDraft(null);
    if (draft === null || draft.trim() === "" || !Number.isFinite(n)) return;
    const next = Math.min(max, Math.max(min, n));
    if (next !== published) set(k, next);
  };
  return (
    <SettingRow className="ck-set" label={label} description={note}>
      <Input type="number" className="ck-input" aria-label={label} min={min} max={max} step={1}
        value={draft ?? String(published)}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => { if (e.key === "Enter") e.currentTarget.blur(); }} />
    </SettingRow>
  );
}

/** A list of strings, one per line, saved when the box loses focus. Sent in the shape the notch published (array or JSON text). */
function TextListRow({ s, set, k, label, note }: {
  s: Published;
  set: SetFn;
  k: string;
  label: string;
  note: string;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  if (!(k in s)) return null;
  const published = s[k];
  const list = asList(published);
  const commit = () => {
    const next = (draft ?? "").split("\n").map((l) => l.trim()).filter((l) => l !== "");
    setDraft(null);
    if (draft === null || next.join("\n") === list.join("\n")) return;
    set(k, typeof published === "string" ? JSON.stringify(next) : next);
  };
  return (
    <SettingRow className="ck-set" label={label} description={note}>
      <textarea className="ck-input" aria-label={label} rows={Math.min(8, Math.max(3, list.length + 1))} spellCheck={false}
        value={draft ?? list.join("\n")}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit} />
    </SettingRow>
  );
}

/** The approved folders: Add opens the native folder picker, each folder has Remove. */
function FolderListRows({ s, set }: { s: Published; set: SetFn }) {
  if (!("remoteRoots" in s)) return null;
  const published = s.remoteRoots;
  const list = asList(published);
  const send = (next: string[]) => set("remoteRoots", typeof published === "string" ? JSON.stringify(next) : next);
  const add = () => {
    invoke<string | null>("file_choose_folder")
      .then((picked) => { if (picked && !list.includes(picked)) send([...list, picked]); })
      .catch(() => undefined);
  };
  return (
    <>
      <SettingRow className="ck-set" label="Approved folders"
        description="The folders remote tools may read. None by default, so every read fails until you add one. Folders that hold credentials (.ssh, .aws and the like) are never approved, and a drive root is refused.">
        <Button size="sm" variant="secondary" onClick={add}>Add folder…</Button>
      </SettingRow>
      {list.map((path) => (
        <SettingsItem key={path} text={path} className="ck-set">
          <div className="ck-text"><span className="ck-sub" title={path}>{path}</span></div>
          <div className="ck-ctl">
            <Button size="sm" variant="ghost" onClick={() => send(list.filter((p) => p !== path))}>Remove</Button>
          </div>
        </SettingsItem>
      ))}
    </>
  );
}

type PickedApp = { id: string; name: string };

/**
 * Blocked apps: the apps no remote tool may open, see or act on. Add opens the native app picker
 * (a .app on the Mac, an .exe or shortcut on Windows); dropping a .app (Mac) or an .exe or .lnk
 * (Windows) on the list adds it too. The main process reads the item to its bundle id, AppUserModelID
 * or exe path. Each entry has Remove. None by default: every app is in reach.
 */
function BlockedAppsRows({ s, set }: { s: Published; set: SetFn }) {
  const k = "remoteBlockedApps";
  const [over, setOver] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const published = s[k];
  const list = asList(published);
  const present = k in s;
  const listKey = list.join("\n");
  const send = (next: string[]) => set(k, typeof published === "string" ? JSON.stringify(next) : next);
  const addPicked = (picked: PickedApp) => {
    setProblem(null);
    if (!list.some((id) => id.toLowerCase() === picked.id.toLowerCase())) send([...list, picked.id]);
  };
  const add = () => {
    invoke<PickedApp | null>("app_choose")
      .then((picked) => { if (picked) addPicked(picked); })
      .catch((e) => setProblem(String(e)));
  };
  useEffect(() => {
    if (!present) return undefined;
    let stop: (() => void) | undefined;
    let gone = false;
    getCurrentWebview()
      .onDragDropEvent((event) => {
        const e = event.payload;
        if (e.type === "enter" || e.type === "over") setOver(true);
        else if (e.type === "leave") setOver(false);
        else if (e.type === "drop") {
          setOver(false);
          const known = listKey === "" ? [] : listKey.split("\n");
          Promise.all(e.paths.map((path) => invoke<PickedApp>("app_identify", { path }).then((a) => a, (err) => String(err))))
            .then((found) => {
              const fresh = [...known];
              let failed: string | null = null;
              for (const item of found) {
                if (typeof item === "string") failed = item;
                else if (!fresh.some((id) => id.toLowerCase() === item.id.toLowerCase())) fresh.push(item.id);
              }
              setProblem(failed);
              if (fresh.length !== known.length) send(fresh);
            });
        }
      })
      .then((fn) => { if (gone) fn(); else stop = fn; })
      .catch(() => undefined);
    return () => { gone = true; stop?.(); };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [present, listKey]);
  if (!present) return null;
  return (
    <>
      <SettingRow className="ck-set" label="Blocked apps"
        description={(isWindows
          ? "Apps Pulse's agents may never open, see or control. Add one with the picker, or drop an .exe or a shortcut (.lnk) on this list. "
          : "Apps Pulse's agents may never open, see or control. Add one with the picker, or drop a .app on this list. ")
          + `None by default: every other app is in reach. Pulse itself, ${isWindows ? "Windows Settings" : "System Settings"} and security prompts are always off limits.`}>
        <Button size="sm" variant="secondary" onClick={add}>Add app…</Button>
      </SettingRow>
      <div className={over ? "ck-drop ck-drop-over" : "ck-drop"} aria-label="Drop apps here to block them">
        {list.length === 0 && <span className="ck-sub">{isWindows ? "Drop an .exe or .lnk here" : "Drop a .app here"}</span>}
        {list.map((id) => (
          <SettingsItem key={id} text={id} className="ck-set">
            <div className="ck-text"><span className="ck-sub" title={id}>{id}</span></div>
            <div className="ck-ctl">
              <Button size="sm" variant="ghost" onClick={() => send(list.filter((p) => p !== id))}>Remove</Button>
            </div>
          </SettingsItem>
        ))}
        {problem && <span className="ck-sub" role="alert">{problem}</span>}
      </div>
    </>
  );
}

const pinError = (e: unknown): string => {
  const text = typeof e === "string" ? e : e instanceof Error ? e.message : "The PIN could not be saved.";
  const m = /^(?:pin_not_set|pin_wrong|pin_locked|pin_invalid|approval_changed|unlock_needs_auth): (.*)$/s.exec(text);
  return m ? m[1] : text;
};

/** The the host app PIN: shows Set / Not set and a Set or Change form. The engine stores it hashed; this page never keeps, logs or echoes it. */
function PulsePinRow() {
  const [isSet, setIsSet] = useState<boolean | null>(null);
  const [open, setOpen] = useState(false);
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);

  const load = () => {
    remoteUnlockApi.pinStatus()
      .then((r) => setIsSet(Boolean((r as { set?: unknown } | null)?.set)))
      .catch((e: unknown) => { setIsSet(null); setProblem(pinError(e)); });
  };
  useEffect(load, []);

  const clear = () => { setCurrent(""); setNext(""); setConfirm(""); };
  const submit = () => {
    if (next !== confirm) { setProblem("The new PIN and its confirmation differ."); return; }
    if (next.length < 4 || next.length > 64) { setProblem("A PIN is 4 to 64 characters."); return; }
    const pin = next;
    const cur = current;
    clear();
    setBusy(true);
    setProblem(null);
    remoteUnlockApi.pinSet(pin, isSet ? cur : undefined)
      .then(() => { setSaved(true); setOpen(false); setIsSet(true); })
      .catch((e: unknown) => { setSaved(false); setProblem(pinError(e)); })
      .finally(() => setBusy(false));
  };
  const field = (label: string, value: string, set: (v: string) => void) => (
    <Input type="password" className="ck-input" aria-label={label} placeholder={label} value={value}
      autoComplete="off" autoCorrect="off" autoCapitalize="off" spellCheck={false} maxLength={64}
      onChange={(e) => set(e.target.value)}
      onKeyDown={(e) => { if (e.key === "Enter" && !busy) submit(); }} />
  );
  const state = isSet === null ? "Unknown" : isSet ? "Set" : "Not set";
  return (
    <>
      <SettingRow className="ck-set" label="Pulse PIN"
        description={`${state}. A PIN of your own, 4 to 64 characters, that unlocks a chat from the hub or the notch instead of Touch ID or Windows Hello. Kept hashed in the system credential store, never shown. Not set by default.`}>
        <Button size="sm" variant="secondary" disabled={busy} onClick={() => { setOpen(!open); setSaved(false); setProblem(null); clear(); }}>
          {open ? "Cancel" : isSet ? "Change PIN" : "Set PIN"}
        </Button>
      </SettingRow>
      {open && (
        <div className="ck-set ck-stack">
          <div className="ck-ctl">
            {isSet && field("Current PIN", current, setCurrent)}
            {field("New PIN", next, setNext)}
            {field("Confirm new PIN", confirm, setConfirm)}
            <Button size="sm" variant="primary" disabled={busy || next === ""} onClick={submit}>{isSet ? "Change PIN" : "Set PIN"}</Button>
          </div>
        </div>
      )}
      {problem && <span className="ck-sub" role="alert">{problem}</span>}
      {saved && <span className="ck-sub" role="status">PIN saved.</span>}
    </>
  );
}

/**
 * Asks for the the host app PIN when the engine refuses a security change that makes a gate less safe
 * (`unlock_needs_auth`). Shown at the top of Settings; the change is sent again with the PIN, which
 * this page never keeps, logs or echoes. Without a PIN set, set one under Remote access first.
 */
export function ProtectedPinPrompt() {
  const [ask, setAsk] = useState<PinAsk | null>(null);
  const [pin, setPin] = useState("");
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    onProtectedPin((next) => { setAsk(next); setPin(""); });
    return () => onProtectedPin(null);
  }, []);
  if (!ask) return null;
  const submit = () => {
    const typed = pin;
    setPin("");
    if (typed === "") return;
    setBusy(true);
    setProtectedSetting(ask.key, ask.value, typed)
      .catch((e: unknown) => setAsk({ ...ask, problem: pinError(e) }))
      .finally(() => setBusy(false));
  };
  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Confirm with your Pulse PIN">
      <SettingRow className="ck-set" label="Pulse PIN"
        description={ask.problem ?? "This change makes a gate less safe, so Pulse asks for your PIN. Set a PIN under Remote access first if you have none."}>
        <div className="ck-set ck-stack">
          <Input type="password" className="ck-input" aria-label="Pulse PIN" autoComplete="off" value={pin} maxLength={64}
            onChange={(e) => setPin(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") submit(); }} />
          <Button size="sm" variant="primary" disabled={busy || pin === ""} onClick={submit}>Confirm</Button>
          <Button size="sm" variant="ghost" onClick={() => { setAsk(null); setPin(""); }}>Cancel</Button>
        </div>
      </SettingRow>
    </SettingsGroup>
  );
}

/** General > Remote access: read-only access to this computer for agents, and the ChatGPT gateway. Every row hides when its key is not published. */
export function RemoteAccessGroup({ s, set }: { s: Published; set: SetFn }) {
  if (!("remoteTools" in s)) return null;
  const on = Boolean(s.remoteTools);
  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Remote access">
      <SwitchRow s={s} set={set} k="remoteTools" label="Let agents read files and chats here"
        note="Lets Claude Code and Codex read files in the approved folders and the chats you select, through the pulse command. Read-only, this computer only. Off by default; while off, every call is refused." />
      <SwitchRow s={s} set={set} k="remoteGateway" label="ChatGPT gateway" disabled={!on}
        note="Lets ChatGPT reach the same read-only tools through OpenAI's Secure MCP Tunnel. Needs the tunnel client, and needs the switch above on. Whatever is read from an approved folder is returned to OpenAI as written. Off by default." />
      <SwitchRow s={s} set={set} k="remoteDocs" label="Document tools" disabled={!on}
        note="Lets agents read the text of a PDF in an approved folder (pulse docs read). Needs the switch above on. Off by default." />
      <SwitchRow s={s} set={set} k="remoteFilesWrite" label="Let remote tools change files in approved folders" disabled={!on}
        note="Lets agents create, edit, move, copy and trash files inside the approved folders. Needs the switch above on. Each change needs your approval here first; nothing is deleted outright. Off by default." />
      <SwitchRow s={s} set={set} k="remoteTerminals" label="Let remote tools run terminal commands" disabled={!on}
        note="Lets agents run commands as you through the system shell, and list or stop this user's processes. Needs the switch above on. Each terminal session needs your approval here first, unless the next switch is on. Unrestricted user-level execution once approved. Off by default." />
      <SwitchRow s={s} set={set} k="remoteTermStanding" label="Approve terminal sessions without asking each time" disabled={!on || !Boolean(s.remoteTerminals)}
        note="Skips the per-session approval for terminal commands; sessions still end after 30 minutes idle. Needs the terminal switch above on. Off by default." />
      <TextListRow s={s} set={set} k="remoteTermAllow" label="Commands the terminal may run"
        note="One program per line. Empty means no restriction. Only the programs listed here may start through a terminal command." />
      <TextListRow s={s} set={set} k="remoteTermDeny" label="Commands the terminal refuses"
        note="One program per line. A program listed here is refused. None by default." />
      <SwitchRow s={s} set={set} k="remoteClipboard" label="Read the clipboard (asks each time)" disabled={!on || !Boolean(s.remoteScreenDisclosure)}
        note="Lets agents read the clipboard, with a local approval each time. Needs the switch above and the app-window disclosure grant. Off by default." />
      <SwitchRow s={s} set={set} k="remoteReadOnly" label="Read-only: refuse every tool that changes something"
        note="The ChatGPT gateway refuses every tool that changes something, whatever the switches above say. Off by default." />
      <SwitchRow s={s} set={set} k="remoteScreenDisclosure" label="Let app windows leave this computer" disabled={!on}
        note="Your grant that what an app window shows (its text, field values and a screenshot) goes to the remote caller, for example OpenAI when it is ChatGPT. The two switches below do nothing without it. Off by default." />
      <SwitchRow s={s} set={set} k="remoteScreenObserve" label="Let remote tools see app windows" disabled={!on || !Boolean(s.remoteScreenDisclosure)}
        note="Lets agents list the windows on screen, read a window's accessibility tree and take a screenshot of it, in any app that is not blocked. Needs the switches above on and the Accessibility permission (Screen Recording for screenshots), which Pulse never asks for on a remote call. Password fields are withheld. Off by default." />
      <SwitchRow s={s} set={set} k="remoteScreenAct" label="Let remote tools act in app windows" disabled={!on || !Boolean(s.remoteScreenDisclosure)}
        note="Lets agents press, type and click in any app that is not blocked. Your own agent here is not asked about ordinary actions; anything that sends, buys or publishes still asks every time, and passwords, card numbers, bank details and IDs are never typed. ChatGPT through the gateway is asked for every action. It never takes your focus. Off by default." />
      <SwitchRow s={s} set={set} k="remoteLaunch" label="Let remote tools open apps" disabled={!on}
        note="Lets agents open any app that is not blocked (apps.launch), with no question for your own agent. ChatGPT through the gateway is asked once per app, then it stays approved until you stop using it for a month. Needs the first switch on. Off by default." />
      <BlockedAppsRows s={s} set={set} />
      <SwitchRow s={s} set={set} k="remoteMedia" label="Let remote tools control media" disabled={!on}
        note="Lets agents play, pause, skip and search-and-play in Spotify and Music (media.control). Low risk, so it needs only this switch and the first one. Off by default." />
      <SwitchRow s={s} set={set} k="remoteFormFill" label="Let remote tools fill in forms with my details" disabled={!on || !Boolean(s.remoteScreenAct)}
        note="Lets agents type your name, email, phone and address, from the profile below, into forms in any app that is not blocked. Passwords, card numbers, bank details and government IDs are never typed; use your password manager or autofill for those. Submitting, buying or sending always asks. Needs the act switch above. Off by default." />
      <TextListRow s={s} set={set} k="remoteFormFillStanding" label="Apps where ChatGPT's form fill needs no question"
        note="One entry per line: an app's bundle id or exe name, optionally followed by :: and part of a window title. Only the ChatGPT gateway asks; your own agent never does. None by default." />
      <TextListRow s={s} set={set} k="remoteFormFillProfile" label="Your details for form fill"
        note="One per line as field=value: name=…, email=…, phone=…, address=…. Typed here by you and never read from anywhere else. None by default." />
      <SwitchRow s={s} set={set} k="chatUnlock" label="Ask me before a chat can act on this computer"
        note="The first effectful call from each Claude Code or Codex chat asks for Touch ID, Windows Hello or your Pulse PIN. Agents you start inside Pulse are already unlocked. ChatGPT keeps its per-action approvals. Passwords and other sensitive values are never typed, and submitting, buying or sending still asks every time. On by default." />
      <NumberRow s={s} set={set} k="chatUnlockMinutes" label="Keep a chat unlocked for (minutes, 0 = until the chat ends)" min={0} max={10080}
        note="How long an unlocked chat stays unlocked, 0 to 10080 minutes. 0 means until the chat ends. 0 by default." />
      {"chatUnlock" in s && <PulsePinRow />}
      <RemoteApprovals />
      <FolderListRows s={s} set={set} />
      <TextListRow s={s} set={set} k="remoteChatRead" label="Chats a remote reader may read"
        note="One chat id per line: a Claude session id, a Codex thread id or a Muse session id, as <id>, claude:<id>, codex:<id> or muse:<id>. A chat not listed is never opened. Secrets are redacted and tool output is left out. None by default." />
      <SwitchRow s={s} set={set} k="phoneChatReadAll" label="My paired phones may read every chat"
        note="On by default: your own paired phones read every chat on this computer. Off: a phone reads only the chats listed above, and any other chat is refused. Never applies to cloud agents (guests), the ChatGPT gateway or agent tools, which read only the chats listed above. The Remote tools switch does not gate it, and removing a phone ends its access. Turning it back on needs your Pulse PIN." />
      <NumberRow s={s} set={set} k="remoteReadMaxBytes" label="Largest file read (bytes)" min={1} max={2 * 1024 * 1024}
        note="The most bytes one file read returns, 1 to 2097152. A larger file is returned in windows. 1048576 (1 MiB) by default." />
      <NumberRow s={s} set={set} k="remoteFindMaxResults" label="Most find results" min={1} max={1000}
        note="The most results one file search returns, 1 to 1000. 200 by default." />
      <NumberRow s={s} set={set} k="remoteChatReadMaxTurns" label="Most chat turns" min={1} max={500}
        note="The most turns one chat read returns, 1 to 500. 50 by default." />
      <NumberRow s={s} set={set} k="remoteChatReadMaxBytes" label="Most chat text (bytes)" min={1} max={2 * 1024 * 1024}
        note="The most bytes of turn text one chat read returns, 1 to 2097152; older turns are dropped first. 262144 (256 KiB) by default." />
      <NumberRow s={s} set={set} k="remoteImageMaxBytes" label="Largest image (bytes)" min={1} max={25 * 1024 * 1024}
        note="The most bytes of source image an image read would accept, 1 to 26214400. Stored now; image reads are not built yet. 10485760 (10 MiB) by default." />
      <NumberRow s={s} set={set} k="remoteDocsMaxBytes" label="Largest document (bytes)" min={1} max={64 * 1024 * 1024}
        note="The most bytes of one document file a document read opens, 1 to 67108864. The file is read whole into memory, so raise it with care. 20971520 (20 MiB) by default." />
    </SettingsGroup>
  );
}

/** Background rows for the hub's own health: the resource watchdog and its budgets, the log, and (Mac) the system reports. */
export function HubHealthRows({ s, set }: { s: Published; set: SetFn }) {
  return (
    <>
      <SwitchRow s={s} set={set} k="hubWatchdog" label="Watch the hub's own resource use"
        note="Every minute the hub checks its own CPU, memory, threads and disk writes, and reports in Activity, pulse status and the hub log when one stays over budget. It only reports. On by default." />
      {(
        <>
          <NumberRow s={s} set={set} k="hubWatchdogCpuPercent" label="Hub CPU budget (%)" min={1} max={800}
            note="Mean CPU, as a share of one core, above which the hub is reported. 5 by default." />
          <NumberRow s={s} set={set} k="hubWatchdogMinutes" label="Hub budget window (minutes)" min={1} max={60}
            note="How long a budget must be exceeded before it is reported. 10 by default." />
          <NumberRow s={s} set={set} k="hubWatchdogFootprintMb" label="Hub memory budget (MB)" min={64} max={65536}
            note={`Memory in use above which the hub is reported (${isWindows ? "private bytes" : "footprint"}). 1024 by default.`} />
          <NumberRow s={s} set={set} k="hubWatchdogThreads" label="Hub thread budget" min={10} max={10000}
            note="Thread count above which the hub is reported. 200 by default." />
          <NumberRow s={s} set={set} k="hubWatchdogWriteMbPerHour" label="Hub disk writes budget (MB per hour)" min={1} max={1048576}
            note="Bytes written to disk per hour above which the hub is reported. 500 by default." />
        </>
      )}
      <SwitchRow s={s} set={set} k="hubLog" label="Keep a hub log"
        note="Writes hub.jsonl in Pulse's logs folder: starts, stops, services, file-index events, watchdog trips and crashes. No file names, message text or contents. On by default." />
      {!isWindows && (
        <SwitchRow s={s} set={set} k="hubDiagReports" label="Watch macOS reports about Pulse"
          note="Each hour, looks (names and dates only) for new crash, CPU and disk-write reports about Pulse in the system's DiagnosticReports folders and lists them in Activity. On by default." />
      )}
    </>
  );
}

/** Under "Index file names": pause and then switch the index off when it keeps misbehaving, and the limits that decide it. */
export function FileIndexLadderRows({ s, set }: { s: Published; set: SetFn }) {
  if (isWindows) return null;
  return (
    <>
      <SwitchRow s={s} set={set} k="hubFileIndexAutoPause" label="Pause the file index when it misbehaves"
        note="When the index stays unhealthy, pauses it for 5 minutes, then 10, then switches it off and says so in Activity. Two healthy hours reset the count; switching the index off and on clears a pause. On by default." />
      {(
        <>
          <NumberRow s={s} set={set} k="hubFileIndexMaxTrips" label="Index pauses before it is switched off" min={1} max={20}
            note="3 by default." />
          <NumberRow s={s} set={set} k="hubFileIndexPauseMinutes" label="First index pause (minutes)" min={1} max={240}
            note="Doubles each time. 5 by default." />
          <NumberRow s={s} set={set} k="hubFileIndexMaxCpuPercent" label="Index CPU limit (%)" min={10} max={800}
            note="Hub CPU above this for 10 minutes while the index is busy counts as unhealthy. 100 by default." />
          <NumberRow s={s} set={set} k="hubFileIndexMaxFootprintMb" label="Index memory limit (MB)" min={256} max={65536}
            note="Hub memory above this while the index is busy counts as unhealthy. 2048 by default." />
          <NumberRow s={s} set={set} k="hubFileIndexMaxWriteMbPerHour" label="Index disk writes limit (MB per hour)" min={64} max={1048576}
            note="Disk writes above this while the index is busy count as unhealthy. 2048 by default." />
        </>
      )}
    </>
  );
}
