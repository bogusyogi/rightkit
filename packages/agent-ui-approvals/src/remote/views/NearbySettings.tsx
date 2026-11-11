import { Fragment, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Pencil, X } from "lucide-react";
import { Button, IconButton, SegmentedControl, Toggle, Input } from "@rightkit/react-shell/react";
import { isWindows } from "@rightkit/system-ui/api";
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui/core";
import { NeedsLine } from "./settingsKit.js";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";
import { CHAT_KIND_ORDER, chatKindName } from "../lib/chatKind.js";

export type TrustChoice = "allow" | "ask" | "deny";

/** One device with a trust record (`trust.devices` in share-state.json). */
export interface TrustDevice {
  fingerprint: string;
  /** The owner's name for it; empty until given. */
  label: string;
  /** What the device calls itself (a claim). */
  alias: string;
  model: string | null;
  kind: string | null;
  state: TrustChoice;
  present: boolean;
  /** It proved its key by certificate (identity); a callback proof is not this. */
  verified: boolean;
  /** How: its client certificate, or by answering at its address; null when never. */
  proof: "certificate" | "callback" | null;
  firstSeenMs: number;
  lastSeenMs: number;
  refused: number;
}

/** What the sharing service reports (hub/src-tauri/src/share.rs). */
export interface ShareState {
  running: boolean;
  error: string | null;
  alias?: string;
  saveDir?: string;
  devices: { fingerprint: string; alias: string; deviceType?: string | null; ip: string; state?: TrustChoice }[];
  /** Per-device Allow / Ask / Deny; new devices start as `defaultState`. */
  trust?: { defaultState: "ask" | "deny"; devices: TrustDevice[] };
  warnings: string[];
  /** macOS Local Network access: "granted" once a multicast send worked, "blocked" when macOS refuses. */
  localNetwork: "unknown" | "granted" | "blocked";
  /** Chat (pulse_core::chat): chats on this computer and on linked ones (over ssh). */
  chat?: {
    enabled: boolean;
    /** Deliver chat messages to Claude through the the host app plugin: the effective state. */
    claudeHookDelivery?: boolean;
    /** The setting as written: auto (default), on or off. */
    claudeHookDeliveryMode?: "auto" | "on" | "off";
    /** List terminal chats active in the last 6 hours (default on). */
    terminalChats?: boolean;
    /** List and message Claude chats in other Claude profiles (default on). */
    chatClaudeProfiles?: boolean;
    /** List, read and reach Muse chats, and run the host app's Muse hook (default on). */
    chatMuse?: boolean;
    /** Each message a chat sends carries the user's last three typed turns (default on). */
    stampUserWords?: boolean;
    /** Start a turn in an idle Codex chat when a message arrives (default on). */
    codexWakeIdle?: boolean;
    /** Start a turn in an idle Claude Desktop chat when a message arrives (default on). */
    claudeWakeIdle?: boolean;
    /** Start a turn in an idle Muse chat when a message arrives (default on; Muse has no supported wake entry yet). */
    museWakeIdle?: boolean;
    /** Wake an idle Codex chat with one short line; the message goes in as hidden context (default on). */
    codexHiddenDelivery?: boolean;
    /** Show receipts of messages this chat sent (delivered, seen, released, ...) in chat hooks (default on). */
    chatReceiptNotices?: boolean;
    active?: boolean;
    localChats?: number;
    links?: { device: string; ssh: string; chats: number | null; error: string | null; lastOkMs?: number | null; lastErrorMs?: number | null }[];
    activity?: { sentMs?: number; receivedMs?: number; sent?: number; received?: number; lastOutcome?: string | null };
    chats?: { name: string; kind: string; status: string; device: string; local: boolean; cwd?: string; updatedMs?: number | null; liveness?: string; unread?: number; evicted?: number }[];
    lastError?: string | null;
    device?: string;
  };
}


interface ChatChange { target: string; path: string; action: string; note: string }
interface ChatReport { dryRun: boolean; changes: ChatChange[] }
const TARGET_NAMES: Record<string, string> = { claude: "Claude", codex: "Codex" };

/** Polls the sharing service; also refreshes when the service says something changed. */
export function useShareState(): ShareState | null {
  const [state, setState] = useState<ShareState | null>(null);
  useEffect(() => {
    let alive = true;
    const load = () => {
      invoke<ShareState>("share_state")
        .then((value) => { if (alive) setState(value); })
        .catch(() => { if (alive) setState(null); });
    };
    load();
    const timer = visibleInterval(load, 3000);
    const unlisten = listen("share-devices", load);
    return () => {
      alive = false;
      clearVisibleInterval(timer);
      void unlisten.then((f) => f());
    };
  }, []);
  return state;
}

/** The "Chat" block: chats talking to chats on nearby computers. */
function AgentChat({ share, s, set }: { share: ShareState | null; s: Record<string, unknown>; set: (key: string, value: unknown) => void }) {
  const chat = share?.chat;
  const enabled = chat?.enabled !== false;
  const [message, setMessage] = useState<string | null>(null);
  const [report, setReport] = useState<ChatReport | null>(null);
  const [busy, setBusy] = useState(false);

  const toggle = (on: boolean) => {
    invoke<void>("chat_set_enabled", { on }).catch((e) => setMessage(String(e)));
  };
  const install = () => {
    setBusy(true);
    setMessage(null);
    invoke<ChatReport>("chat_install_skill")
      .then((r) => setReport(r))
      .catch((e) => { setReport(null); setMessage(String(e)); })
      .finally(() => setBusy(false));
  };

  let status = "";
  if (!enabled) status = "Off.";
  else {
    const local = chat?.localChats ?? 0;
    const links = (chat?.links ?? []).map((l) => {
      if (l.error) {
        const since = l.lastErrorMs ? `offline ${ageText(l.lastErrorMs)}` : "offline";
        return `${l.device}: ${since}: ${l.error}`;
      }
      const ok = l.lastOkMs ? `, ok ${ageText(l.lastOkMs)}` : "";
      return `${l.chats ?? 0} on ${l.device}${ok}`;
    });
    status = [`${local} ${local === 1 ? "chat" : "chats"} here.`, ...links.map((t) => `${t}.`)].join(" ");
  }

  const activity = chat?.activity;
  const outcome = activity?.sentMs
    ? `Last message: ${activity.lastOutcome ?? "delivered"} · ${ageText(activity.sentMs)} ago`
    : null;

  return (
    <>
      <SettingRow className="ck-set"
        label="Chat"
        description="Lets Claude, Codex and Muse chats on this computer message chats on linked computers over ssh. Link one with: pulse chat link <device> <ssh-host>"
      >
        <Toggle checked={enabled} onChange={toggle} label="Chat" />
      </SettingRow>
      {enabled && (
        <SettingRow className="ck-set" label="Light the Send ring on Chat activity" description="The Send ring pulses for a few seconds when a Chat message is sent or arrives. On by default.">
          <Toggle checked={s.chatRingLight !== false} onChange={(on) => set("chatRingLight", on)} label="Light the Send ring on Chat activity" />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Install Pulse Chat into the agent apps it finds"
          description="Finds Claude Code, Codex and Muse on this computer and installs Pulse's Chat integration where it is missing, with nothing to do on your side. Muse asks once on a notch card; a Not now is not asked again until this is turned off and on. On by default."
        >
          <Toggle checked={s.chatPluginAutoInstall !== false} onChange={(on) => set("chatPluginAutoInstall", on)} label="Install Pulse Chat into the agent apps it finds" />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Deliver chat messages to Claude through the Pulse plugin"
          description="The message arrives as added context labelled as coming from another agent, after the chat's next tool call, at the end of its turn, or with the user's next prompt. An idle chat reads it only on its next turn. Requires the Pulse plugin installed in Claude Code. Auto (the default): on once a Pulse Claude plugin hook has run on this computer within 7 days. On or Off overrides it."
        >
          <SegmentedControl
            label="Deliver chat messages to Claude through the Pulse plugin"
            value={chat?.claudeHookDeliveryMode ?? "auto"}
            options={[
              { value: "auto", label: "Auto" },
              { value: "on", label: "On" },
              { value: "off", label: "Off" },
            ]}
            onChange={(v) => {
              invoke<void>("chat_set_claude_hook_delivery", { mode: v }).catch((e) => setMessage(String(e)));
            }}
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="List terminal chats active in the last 6 hours"
          description="Lists interactive terminal chats (Claude Code, Codex and Muse in a terminal) alongside the app chats, but only while they have been active in the last 6 hours, so old terminal sessions do not clutter the list. Off, only app chats are listed. App chats are always listed. On by default."
        >
          <Toggle
            checked={chat?.terminalChats !== false}
            onChange={(on) => {
              invoke<void>("chat_set_terminal_chats", { on }).catch((e) => setMessage(String(e)));
            }}
            label="List terminal chats active in the last 6 hours"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Claude profiles in other home folders"
          description="Lists and messages Claude chats in other Claude profiles (~/ClaudeProfiles), labelled by profile. On by default."
        >
          <Toggle
            checked={chat?.chatClaudeProfiles !== false}
            onChange={(on) => {
              invoke<void>("chat_set_claude_profiles", { on }).catch((e) => setMessage(String(e)));
            }}
            label="Claude profiles in other home folders"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Muse chats"
          description="Lists, reads and messages Muse Code chats, and lets Pulse's Muse plugin register them and hand them messages. Off, no Muse chat is listed or reached and the plugin's hooks do nothing. Install the plugin with pulse chat install --muse. On by default."
        >
          <Toggle
            checked={chat?.chatMuse !== false}
            onChange={(on) => {
              invoke<void>("chat_set_muse_chats", { on }).catch((e) => setMessage(String(e)));
            }}
            label="Muse chats"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Stamp my words into messages"
          description="Each message a chat sends carries your last three typed turns, read by Pulse from that chat's transcript, so the receiving chat can check what you asked. Up to 3 KB, only to your own linked computers. On by default."
        >
          <Toggle
            checked={chat?.stampUserWords !== false}
            onChange={(on) => {
              invoke<void>("chat_set_stamp_user_words", { on }).catch((e) => setMessage(String(e)));
            }}
            label="Stamp my words into messages"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Start a turn in an idle Codex chat when a message arrives"
          description="On, an idle Codex chat is started on the message without anyone pressing Steer, using Codex's own command line, with that chat's normal permissions, and it uses the account's Codex allowance. Only a chat that is positively idle is started, and never one the Codex app may hold: then the message waits for the chat's next prompt or tool call. Off, the message waits for the owner's next prompt. On by default. The app may not show the new turn until the chat is reopened."
        >
          <Toggle
            checked={chat?.codexWakeIdle !== false}
            onChange={(on) => {
              invoke<void>("chat_set_codex_wake_idle", { on }).catch((e) => setMessage(String(e)));
            }}
            label="Start a turn in an idle Codex chat when a message arrives"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Start a turn in an idle Claude chat when a message arrives"
          description="On, a message for an idle Claude Desktop chat opens that chat in Claude Desktop so it starts a turn, and it uses the account's Claude allowance. Only chats in Claude Desktop's default profile can be opened; a chat in another profile (Claude B) waits for its next turn. Off, the message waits in the chat's inbox. On by default."
        >
          <Toggle
            checked={chat?.claudeWakeIdle !== false}
            onChange={(on) => {
              invoke<void>("chat_set_claude_wake_idle", { on }).catch((e) => setMessage(String(e)));
            }}
            label="Start a turn in an idle Claude chat when a message arrives"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Start a turn in an idle Muse chat when a message arrives"
          description="On, Pulse would start a turn in an idle Muse chat on the message. This Muse version has no supported way to do that, so for now the message waits in the chat's inbox for its next prompt. On by default; it takes effect when Muse offers a supported entry."
        >
          <Toggle
            checked={chat?.museWakeIdle !== false}
            onChange={(on) => {
              invoke<void>("chat_set_muse_wake_idle", { on }).catch((e) => setMessage(String(e)));
            }}
            label="Start a turn in an idle Muse chat when a message arrives"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Hidden delivery into Codex"
          description="Wake an idle Codex chat with one short line; the message itself goes in as hidden context, like Claude's collapsed message row."
        >
          <Toggle
            checked={chat?.codexHiddenDelivery !== false}
            onChange={(on) => {
              invoke<void>("chat_set_codex_hidden_delivery", { on }).catch((e) => setMessage(String(e)));
            }}
            label="Hidden delivery into Codex"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Show receipts of messages you sent in chat hooks"
          description="On, a chat whose hook runs is told, in a short line, when a message it sent was seen by the receiving chat, picked up, released, refused or expired. At most one such notice a minute, batched. Off, the receipts are still kept (pulse chat receipts)."
        >
          <Toggle
            checked={chat?.chatReceiptNotices !== false}
            onChange={(on) => {
              invoke<void>("chat_set_receipt_notices", { on }).catch((e) => setMessage(String(e)));
            }}
            label="Show receipts of messages you sent in chat hooks"
          />
        </SettingRow>
      )}
      {enabled && (
        <SettingRow className="ck-set"
          label="Pulse skill"
          description="Teaches Claude, Codex and Muse chats how to message chats on other computers. Restart a chat to see it."
        >
          <Button size="sm" variant="secondary" disabled={busy} onClick={install}>
            Install the Pulse skill for Claude and Codex
          </Button>
        </SettingRow>
      )}
      {report && report.changes.map((c) => (
        <div key={c.target} className="ck-sub ck-foot">
          {TARGET_NAMES[c.target] ?? c.target}: {c.action}. {c.path}
        </div>
      ))}
      {message && <div className="error" role="alert">{message}</div>}
      <div className={chat?.lastError ? "error" : "ck-sub ck-foot"} role="status">
        {chat?.lastError ?? status}
      </div>
      {enabled && outcome && <div className="ck-sub ck-foot" role="status">{outcome}</div>}
      {enabled && (chat?.chats?.length ?? 0) > 0 && <ChatList chats={chat!.chats!} />}
    </>
  );
}

/** "42s", "5min", "3h" or "2d" since `ms`. */
function ageText(ms: number): string {
  const secs = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}min`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

type Chat = NonNullable<NonNullable<ShareState["chat"]>["chats"]>[number];

function ChatRow({ c, kind }: { c: Chat; kind: string }) {
  const cwd = c.cwd ?? "";
  const folder = cwd.split(/[\\/]/).filter(Boolean).pop() ?? "";
  return (
    <li className="ck-chats-row">
      <span className={`ck-chats-kind ck-chats-kind-${kind}`}>{chatKindName(kind)}</span>
      <span className="ck-chats-name" title={c.name}>{c.name}</span>
      <span className="ck-chats-folder" title={cwd || undefined}>{folder}</span>
      <span className="ck-chats-when">
        {c.updatedMs ? <span className="ck-chats-age">{ageText(c.updatedMs)} ago</span> : null}
        <span className={`ck-chats-status ck-chats-status-${c.status}`}>
          {c.status}{c.liveness ? ` · ${c.liveness}` : ""}
        </span>
      </span>
      {c.local && ((c.unread ?? 0) > 0 || (c.evicted ?? 0) > 0) && (
        <span className="ck-sub ck-chats-inbox">
          Inbox: {c.unread ?? 0} unread{(c.evicted ?? 0) > 0 ? `, ${c.evicted} evicted` : ""}
        </span>
      )}
    </li>
  );
}

/** Every chat the host app can message, grouped by computer and then by kind: Claude chats live; Codex and Muse threads active when written to in the last ten minutes. */
function ChatList({ chats }: { chats: Chat[] }) {
  const devices = Array.from(new Set(chats.map((c) => c.device)));
  const ordered = [...devices.filter((d) => chats.some((c) => c.device === d && c.local)), ...devices.filter((d) => !chats.some((c) => c.device === d && c.local))];
  // Known kinds in their fixed order, then any other kind the listing reports, in first-seen order.
  const kinds = [...CHAT_KIND_ORDER, ...Array.from(new Set(chats.map((c) => c.kind))).filter((k) => !CHAT_KIND_ORDER.includes(k))];
  return (
    <div className="ck-chats" aria-label="Chats Pulse can message">
      {ordered.map((device) => {
        const here = chats.filter((c) => c.device === device);
        const counts = kinds.map((kind) => ({ kind, rows: here.filter((c) => c.kind === kind) }));
        // Claude and Codex always show their count, as before; other kinds show once a chat of that kind is here.
        const headed = counts.filter((g) => g.rows.length > 0 || CHAT_KIND_ORDER.slice(0, 2).includes(g.kind));
        return (
          <section key={device} className="ck-chats-device">
            <h3 className="ck-chats-head">{device} <span className="ck-sub">{headed.map((g) => `${g.rows.length} ${chatKindName(g.kind)}`).join(" · ")}</span></h3>
            <ul className="ck-chats-list">
              {counts.map(({ kind, rows }) => {
                // Claude chats are all listed; other kinds show eight and say how many more there are.
                const shown = kind === "claude" ? rows : rows.slice(0, 8);
                return (
                  <Fragment key={kind}>
                    {shown.map((c, i) => <ChatRow key={`${kind}-${i}-${c.name}`} c={c} kind={kind} />)}
                    {rows.length > shown.length && <li className="ck-sub ck-chats-more">+{rows.length - shown.length} more {chatKindName(kind)} threads (`pulse chat peers` lists all)</li>}
                  </Fragment>
                );
              })}
            </ul>
          </section>
        );
      })}
    </div>
  );
}

const KIND_NAMES: Record<string, string> = {
  mobile: "Phone", desktop: "Computer", web: "Browser", headless: "Terminal", server: "Server",
};

const NOT_VERIFIED_NOTE = "Not verified: this app can't prove its identity. Allow asks first until it answers a check at its address.";

function proofText(d: TrustDevice): { text: string; note: string } {
  if (d.proof === "certificate") return { text: "verified (proved its key)", note: "This device proved it holds its key when it connected." };
  if (d.proof === "callback") return { text: "answered at its address (weaker: files only, kept apart, never the clipboard)", note: "This device answered a check at its network address. Anyone on the network can relay that check while the real device is online, so it only skips the question; files are kept apart and nothing reaches the clipboard." };
  return { text: "not verified", note: NOT_VERIFIED_NOTE };
}

function DeviceRow({ d, run }: { d: TrustDevice; run: (command: string, args: Record<string, unknown>) => void }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(d.label);
  const cancelled = useRef(false);
  useEffect(() => { if (!editing) setDraft(d.label); }, [d.label, editing]);
  const name = d.label || d.alias || "Unnamed device";
  const proof = proofText(d);
  const commit = () => {
    if (cancelled.current) { cancelled.current = false; return; }
    const next = draft.trim();
    if (next !== d.label) run("share_trust_label", { fingerprint: d.fingerprint, label: next });
    setEditing(false);
  };
  const what = [d.kind ? KIND_NAMES[d.kind] ?? "Device" : null, d.model].filter(Boolean).join(" · ");
  const when = d.present ? "nearby now" : `last seen ${ageText(d.lastSeenMs)} ago`;
  const refused = d.refused > 0 ? `refused ${d.refused} ${d.refused === 1 ? "time" : "times"}` : null;
  return (
    <div className="ck-device" data-device={d.fingerprint}>
      <div className="ck-text">
        <div className="ck-account-name">
          {editing ? (
            <Input
              className="ck-input ck-claude-edit"
              autoFocus
              value={draft}
              placeholder={d.alias || "Name this device"}
              onChange={(e) => setDraft(e.target.value)}
              onFocus={(e) => e.target.select()}
              onBlur={commit}
              onKeyDown={(e) => {
                if (e.key === "Enter") (e.target as HTMLInputElement).blur();
                if (e.key === "Escape") { cancelled.current = true; setDraft(d.label); setEditing(false); }
              }}
              aria-label={`Name for ${name}`}
              title="Enter saves, Escape cancels, empty restores the device's own name"
            />
          ) : (
            <Button variant="ghost" className="ck-claude-name" onClick={() => setEditing(true)}
              aria-label={`Rename ${name}`} title="Rename this device">
              <span className="ck-claude-name-text">{name}</span>
              <Pencil size={12} strokeWidth={1.75} aria-hidden="true" />
            </Button>
          )}
          <span className={`ck-status ck-status-${d.proof === "certificate" ? "granted" : "unknown"}`} title={proof.note}>
            {proof.text}
          </span>
        </div>
        <div className="ck-sub">
          {[what, when, refused].filter(Boolean).join(" · ")}
        </div>
        {!d.verified && d.state === "allow" && <div className="ck-sub">{NOT_VERIFIED_NOTE}</div>}
      </div>
      <div className="ck-ctl">
        <span className="ck-ctls">
          <SegmentedControl
            label={`Trust for ${name}`}
            value={d.state}
            options={[{ value: "allow", label: "Allow" }, { value: "ask", label: "Ask" }, { value: "deny", label: "Deny" }]}
            onChange={(v) => run("share_trust_set", { fingerprint: d.fingerprint, state: v })}
          />
          {!d.present && (
            <IconButton className="ck-forget" label={`Forget ${name}`}
              onClick={() => run("share_trust_forget", { fingerprint: d.fingerprint })}
              title="Removes this device from the list; it returns when it is next seen.">
              <X size={14} strokeWidth={1.75} aria-hidden="true" />
            </IconButton>
          )}
        </span>
      </div>
    </div>
  );
}

/** Every device seen on the network or in a request, with Allow / Ask / Deny. */
function DevicesGroup({ share }: { share: ShareState }) {
  const trust = share.trust;
  const [message, setMessage] = useState<string | null>(null);
  const run = (command: string, args: Record<string, unknown>) => {
    setMessage(null);
    invoke<void>(command, args).catch((e) => setMessage(String(e)));
  };
  if (!trust) return null;
  const devices = [...trust.devices].sort((a, b) =>
    Number(b.present) - Number(a.present) || b.lastSeenMs - a.lastSeenMs);
  return (
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Devices">
      <SettingRow className="ck-set"
        label="New devices"
        description="What a device gets the first time it is seen. Allow sends and receives with no question; Ask asks every time; Deny refuses."
      >
        <SegmentedControl
          label="New devices"
          value={trust.defaultState}
          options={[{ value: "ask", label: "Ask" }, { value: "deny", label: "Deny" }]}
          onChange={(v) => run("share_trust_default", { state: v })}
        />
      </SettingRow>
      {devices.length === 0 && <div className="ck-sub ck-foot">No devices seen yet.</div>}
      {devices.map((d) => <DeviceRow key={d.fingerprint} d={d} run={run} />)}
      {message && <div className="error" role="alert">{message}</div>}
    </SettingsGroup>
  );
}

/** General > Nearby sharing: send and receive files with LocalSend and other the host app Macs. */
export function NearbyGroup({ s, set }: {
  s: Record<string, unknown>;
  set: (key: string, value: unknown) => void;
}) {
  const share = useShareState();
  const enabled = s.nearbyEnabled !== false;
  const stored = typeof s.nearbyAlias === "string" ? s.nearbyAlias : "";
  const [alias, setAlias] = useState(stored);
  useEffect(() => setAlias(stored), [stored]);
  const folder = typeof s.nearbySaveFolder === "string" ? s.nearbySaveFolder : "";

  const commitAlias = () => {
    if (alias.trim() !== stored.trim()) set("nearbyAlias", alias.trim());
  };
  const chooseFolder = () => {
    invoke<string | null>("file_choose_folder")
      .then((picked) => { if (picked) set("nearbySaveFolder", picked); })
      .catch(() => undefined);
  };

  let status = "";
  if (!enabled) status = "Off.";
  else if (!share) status = "Starting…";
  else if (share.error) status = share.error;
  else if (share.running) {
    const count = share.devices.length;
    status = `Visible as "${share.alias ?? ""}". ${count === 0 ? "No devices nearby." : `${count} nearby.`}`;
  }

  return (
    <>
    <SettingsGroup hideWhenEmpty headingLevel={2} className="ck-sgroup" rowsClassName="ck-card" title="Nearby sharing">
      <SettingRow className="ck-set"
        label="Send and receive files nearby"
        description={`Works with the LocalSend app on iPhone, Android, ${isWindows ? "Mac" : "Windows"} and Linux. Pulse can be found on your network while this is on.`}
      >
        <Toggle checked={enabled} onChange={(v) => set("nearbyEnabled", v)} label="Nearby sharing" />
      </SettingRow>
      {enabled && (
        <>
          <SettingRow className="ck-set" label="Device name" description={isWindows ? "How other devices list this PC. Empty uses the PC's name." : "How other devices list this Mac. Empty uses the Mac's name."}>
            <Input
              className="ck-input ck-input-name"
              value={alias}
              placeholder={share?.alias ?? (isWindows ? "This PC" : "This Mac")}
              onChange={(e) => setAlias(e.target.value)}
              onBlur={commitAlias}
              onKeyDown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
              aria-label="Device name"
            />
          </SettingRow>
          <SettingRow className="ck-set" label="Save received files to" description={folder || "Downloads"}>
            <span className="ck-ctls">
              <Button size="sm" variant="secondary" onClick={chooseFolder}>Choose…</Button>
              {folder && <Button size="sm" variant="ghost" onClick={() => set("nearbySaveFolder", "")}>Use Downloads</Button>}
            </span>
          </SettingRow>
          <AgentChat share={share} s={s} set={set} />
        </>
      )}
      <div className={share?.error ? "error" : "ck-sub ck-foot"} role={share?.error ? "alert" : "status"}>{status}</div>
      {enabled && share?.warnings.map((w) => <div key={w} className="ck-sub ck-foot">{w}</div>)}
    </SettingsGroup>
    {enabled && share?.running && <DevicesGroup share={share} />}
    </>
  );
}

/**
 * The Permissions row for macOS Local Network access (Windows Firewall on Windows). Neither
 * system can be asked for its state, so it is read from whether multicast sends are refused.
 * With nearby sharing off nothing sends, so there is nothing to check: the row reads Off,
 * as the Mac's does.
 */
export function LocalNetworkRow({ enabled, nearby = true, needs }: { enabled: boolean; nearby?: boolean; needs?: string[] }) {
  const share = useShareState();
  if (!enabled) return null;
  const blocked = nearby && share?.localNetwork === "blocked";
  const granted = nearby && share?.localNetwork === "granted";
  const status = !nearby ? "off" : blocked ? "needsApproval" : granted ? "granted" : "unknown";
  const text = !nearby ? "Off" : blocked ? "Needs approval" : granted ? "Granted" : "Checking…";
  return (
    <div className="ck-permrow">
      <div className="ck-set">
        <div className="ck-text">
          <strong>{isWindows ? "Windows Firewall" : "Local network"}</strong>
          <div className="ck-sub">
            {isWindows
              ? "Lets Pulse find and talk to nearby devices for sharing files. Windows Firewall must allow Pulse on private networks."
              : "Lets Pulse find and talk to nearby devices for sharing files. macOS asks the first time a device connects."}
          </div>
        </div>
        <div className="ck-ctl">
          <span className="ck-ctls">
            <span className={`ck-status ck-status-${status}`}>{text}</span>
            <Button size="sm" variant="secondary" disabled={granted}
              onClick={() => invoke<void>("open_local_network_settings")}>{isWindows ? "Open Firewall" : "Open Settings"}</Button>
          </span>
        </div>
      </div>
      <NeedsLine needs={needs} />
    </div>
  );
}
