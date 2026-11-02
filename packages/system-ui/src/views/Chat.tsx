import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Button, ConfirmDialog, Input } from "@rightkit/react-shell/react";
import { ChevronDown, ChevronRight, Search } from "lucide-react";
import { clearVisibleInterval, visibleInterval } from "../visible.js";
import { chatKindName } from "../lib/chatKind.js";

/** Chat: messages between agent chats on this computer and linked computers (the `pulse chat` engine). */

interface ChatLink {
  device: string;
  ssh: string;
  online: boolean;
  lastOkMs: number | null;
  lastErrorMs: number | null;
  lastError: string | null;
}

interface ChatStatus {
  enabled: boolean;
  hubRunning: boolean;
  device: string;
  chatsHere: number;
  links: ChatLink[];
}

interface ChatPeer {
  /** What a send takes: the chat id here, `<device>:<session>` elsewhere. */
  id: string;
  name: string;
  display: string;
  device: string;
  local: boolean;
  kind: string;
  /** Claude: busy or idle. Codex: active or idle. */
  state: string;
  liveness: string;
  updatedMs: number | null;
  folder: string;
}

interface ChatReceipt {
  msgId: string;
  chat: string;
  state: string;
  detail: string;
}

interface ChatMessage {
  id: string;
  chat: string;
  from: string;
  body: string;
  receivedMs: number;
}

interface ChatInbox {
  messages: ChatMessage[];
  evicted: number;
}

interface ChatLinked {
  device: string;
  ssh: string;
  chats: number;
  theirName: string;
}

/** The hub's web view is destroyed on window close, so the chosen chat and the unsent draft are kept here. */
const KEPT_CHAT = "pulse.hub.chat.selected";
const KEPT_DRAFT = "pulse.hub.chat.draft";
const readKept = (key: string): string | null => {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
};
const writeKept = (key: string, value: string | null) => {
  try {
    if (value) localStorage.setItem(key, value);
    else localStorage.removeItem(key);
  } catch {
    // Storage unavailable: the draft is lost on close, as before.
  }
};

/** The receipt states that mean the chat's side took the message, so the draft can go. */
const TOOK_IT = new Set(["delivered", "sent", "queued", "held"]);

/** What each receipt state means, in plain words. The receipt's own detail follows it. */
const RECEIPT_WORDS: Record<string, string> = {
  delivered: "Delivered.",
  sent: "Sent, no verdict. The chat raised no objection, but it has not confirmed it read the message.",
  queued: "Queued. It shows at the chat's next turn; reading is not confirmed.",
  held: "Held. It will not be read until its owner continues or approves it.",
  refused: "Refused.",
  unsupported: "Unsupported. That chat cannot take messages this way.",
  unknown: "Unknown. It may or may not have arrived. Check the inbox, then send once more.",
};

/** "42s", "5min", "3h" or "2d" since `ms`. */
function ageOf(ms: number): string {
  const secs = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}min`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

function lastSeen(ms: number | null): string {
  return ms ? `${ageOf(ms)} ago` : "not seen";
}

function plural(count: number, word: string): string {
  return `${count} ${word}${count === 1 ? "" : "s"}`;
}

function linkLine(link: ChatLink): string {
  if (link.online) return `${link.device}: reached ${link.lastOkMs ? ageOf(link.lastOkMs) : "just now"} ago`;
  const since = link.lastOkMs ? `, last reached ${ageOf(link.lastOkMs)} ago` : ", never reached";
  return `${link.device}: offline${since}`;
}

/** Chats grouped by computer: this computer first, then the linked ones in the order given. */
function groupByDevice(chats: ChatPeer[]): [string, ChatPeer[]][] {
  const groups = new Map<string, ChatPeer[]>();
  for (const chat of chats) {
    const rows = groups.get(chat.device) ?? [];
    rows.push(chat);
    groups.set(chat.device, rows);
  }
  const isLocal = (rows: ChatPeer[]) => (rows.some((c) => c.local) ? 1 : 0);
  return Array.from(groups.entries()).sort(([, a], [, b]) => isLocal(b) - isLocal(a));
}

export function Chat() {
  const [status, setStatus] = useState<ChatStatus | null>(null);
  const [liveError, setLiveError] = useState<string | null>(null);
  const [inbox, setInbox] = useState<ChatInbox | null>(null);
  const [inboxOpen, setInboxOpen] = useState(false);
  const [peers, setPeers] = useState<ChatPeer[] | null>(null);
  const [peersError, setPeersError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string | null>(() => readKept(KEPT_CHAT));
  const [draft, setDraft] = useState(() => readKept(KEPT_DRAFT) ?? "");
  const [sending, setSending] = useState(false);
  const [receipt, setReceipt] = useState<ChatReceipt | null>(null);
  const [sendError, setSendError] = useState<string | null>(null);
  const [linkDevice, setLinkDevice] = useState("");
  const [linkSsh, setLinkSsh] = useState("");
  const [linkPulse, setLinkPulse] = useState("");
  const [linking, setLinking] = useState(false);
  const [linkNote, setLinkNote] = useState<string | null>(null);
  const [linkError, setLinkError] = useState<string | null>(null);
  const [unlinking, setUnlinking] = useState<string | null>(null);

  // Set while the section is shown; a late answer after it closes is dropped.
  const alive = useRef(false);
  // One chat listing at a time: linked computers answer over ssh, which can take seconds.
  const peersBusy = useRef(false);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const refreshLive = useCallback(() => {
    invoke<ChatStatus>("chat_status")
      .then((value) => {
        if (!alive.current) return;
        setStatus(value);
        setLiveError(null);
      })
      .catch((e) => {
        if (alive.current) setLiveError(String(e));
      });
    invoke<ChatInbox>("chat_inbox")
      .then((value) => {
        if (alive.current) setInbox(value);
      })
      .catch((e) => {
        if (alive.current) setLiveError(String(e));
      });
  }, []);

  const refreshPeers = useCallback(() => {
    if (peersBusy.current) return;
    peersBusy.current = true;
    invoke<ChatPeer[]>("chat_peers")
      .then((list) => {
        if (!alive.current) return;
        setPeers(list);
        setPeersError(null);
      })
      .catch((e) => {
        if (alive.current) setPeersError(String(e));
      })
      .finally(() => {
        peersBusy.current = false;
      });
  }, []);

  // Local state every 3 seconds (as the other pages poll); linked computers every 10 seconds.
  // The timers run only while this section is shown.
  useEffect(() => {
    refreshLive();
    const timer = visibleInterval(refreshLive, 3000);
    return () => clearVisibleInterval(timer);
  }, [refreshLive]);

  useEffect(() => {
    refreshPeers();
    const timer = visibleInterval(refreshPeers, 30000);
    return () => clearVisibleInterval(timer);
  }, [refreshPeers]);

  const list = peers ?? [];
  const q = query.trim().toLowerCase();
  const visible = q
    ? list.filter((c) => [c.name, c.device, c.kind, c.folder, c.state].join(" ").toLowerCase().includes(q))
    : list;
  useEffect(() => writeKept(KEPT_CHAT, selectedId), [selectedId]);
  useEffect(() => writeKept(KEPT_DRAFT, draft), [draft]);
  const chosen = list.find((c) => c.id === selectedId) ?? null;
  const canSend = chosen !== null && draft.trim() !== "" && !sending;

  const send = () => {
    if (!chosen || !canSend) return;
    const text = draft;
    setSending(true);
    setSendError(null);
    invoke<ChatReceipt>("chat_send", { target: chosen.id, text })
      .then((value) => {
        if (!alive.current) return;
        setReceipt(value);
        // Keep the draft when the chat did not take it, so it can be sent again.
        if (TOOK_IT.has(value.state)) setDraft((current) => (current === text ? "" : current));
      })
      .catch((e) => {
        if (alive.current) setSendError(String(e));
      })
      .finally(() => {
        if (alive.current) setSending(false);
      });
  };

  const onDraftKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      send();
    }
  };

  const canLink = linkDevice.trim() !== "" && linkSsh.trim() !== "" && !linking;

  const linkComputer = () => {
    if (!canLink) return;
    setLinking(true);
    setLinkError(null);
    setLinkNote(null);
    invoke<ChatLinked>("chat_link", { device: linkDevice, sshHost: linkSsh, pulse: linkPulse.trim() || null })
      .then((value) => {
        if (!alive.current) return;
        setLinkNote(`Linked ${value.device} (${value.ssh}): ${plural(value.chats, "chat")} there now.`);
        setLinkDevice("");
        setLinkSsh("");
        setLinkPulse("");
        refreshLive();
        refreshPeers();
      })
      .catch((e) => {
        if (alive.current) setLinkError(String(e));
      })
      .finally(() => {
        if (alive.current) setLinking(false);
      });
  };

  const unlink = (device: string) => {
    setUnlinking(null);
    setLinkError(null);
    setLinkNote(null);
    invoke<void>("chat_unlink", { device })
      .then(() => {
        if (!alive.current) return;
        setLinkNote(`Unlinked ${device}.`);
        refreshLive();
        refreshPeers();
      })
      .catch((e) => {
        if (alive.current) setLinkError(String(e));
      });
  };

  let statusLine = "Reading…";
  if (status && !status.enabled) {
    statusLine = "Chat is off on this computer, so messages are refused. Turn it on in Settings, under General.";
  } else if (status) {
    statusLine = `${plural(status.chatsHere, "chat")} on ${status.device}.`;
    if (!status.hubRunning) {
      statusLine += " Pulse's hub is not reporting as running, so messages to Claude chats may be held until it is.";
    }
  }

  const groups = groupByDevice(visible);
  const inboxCount = inbox?.messages.length ?? 0;

  return (
    <div className="view ck-chat">
      <p className="ck-chat-intro">
        Chat sends a message from one agent chat to another, on this computer or a linked one. The receiver is
        another agent, not a person. A reply arrives in the chat that sent the message, not in this window.
      </p>

      <section className="ck-chat-panel" aria-label="Chat status">
        <div className="ck-chat-status" role="status">{statusLine}</div>
        {liveError && <div className="error" role="alert">{liveError}</div>}
        {status?.enabled && status.links.map((link) => (
          <div key={link.device} className="ck-chat-link-line">
            <span>{linkLine(link)}</span>
            {!link.online && link.lastError && <span className="ck-chat-link-error">{link.lastError}</span>}
          </div>
        ))}
      </section>

      <div className="ck-chat-main">
        <section className="ck-chat-panel ck-chat-chats" aria-label="Chats">
          <h2>Chats</h2>
          <label className="ck-chat-search">
            <Search size={15} strokeWidth={1.75} aria-hidden="true" />
            <Input
              type="search"
              aria-label="Search chats"
              placeholder="Search by name, computer or folder"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
          {peersError && <div className="error" role="alert">{peersError}</div>}
          {peers === null && !peersError && <p className="ck-sub">Reading chats…</p>}
          {peers !== null && list.length === 0 && (
            <p className="ck-sub">No chats found. Open a Claude, Codex or Muse chat on this computer, or link another computer.</p>
          )}
          {peers !== null && list.length > 0 && visible.length === 0 && <p className="ck-sub">No chat matches the search.</p>}
          {groups.map(([device, rows]) => (
            <div key={device} className="ck-chat-device">
              <h3>{device}</h3>
              <ul className="ck-chat-list">
                {rows.map((c) => (
                  <li key={c.id}>
                    <Button variant="ghost"
                      className="ck-chat-item"
                      aria-pressed={c.id === selectedId}
                      onClick={() => setSelectedId(c.id)}
                    >
                      <span className={`ck-chat-kind ck-chat-kind-${c.kind}`}>{chatKindName(c.kind)}</span>
                      <span className="ck-chat-name" title={c.folder || undefined}>{c.name}</span>
                      <span className="ck-chat-meta">
                        {c.state}
                        {c.liveness !== "unknown" ? ` · ${c.liveness}` : ""} · {lastSeen(c.updatedMs)}
                      </span>
                    </Button>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </section>

        <section className="ck-chat-panel ck-chat-compose" aria-label="Message">
          <h2>{chosen ? `Message to ${chosen.display}` : "Message"}</h2>
          {!chosen && <p className="ck-sub">{selectedId ? "That chat is no longer listed." : "Choose a chat to message."}</p>}
          <label className="ck-chat-field">
            <span>Message text</span>
            <textarea
              rows={5}
              value={draft}
              disabled={!chosen}
              placeholder="Written for another agent, not a person"
              onChange={(e) => setDraft(e.target.value)}
              onKeyDown={onDraftKey}
            />
          </label>
          <div className="ck-chat-compose-foot">
            <span className="ck-sub">Cmd+Enter or Ctrl+Enter sends. Sent from here, the message has no sending chat, so a reply has nowhere to return.</span>
            <Button variant="primary" size="sm" disabled={!canSend} onClick={send}>
              {sending ? "Sending…" : "Send"}
            </Button>
          </div>
          {sendError && <div className="error" role="alert">{sendError}</div>}
          {receipt && (
            <div className="ck-chat-receipt" role="status">
              <strong>{receipt.chat}:</strong> {RECEIPT_WORDS[receipt.state] ?? receipt.state} {receipt.detail}
            </div>
          )}
        </section>
      </div>

      <section className="ck-chat-panel" aria-label="Inbox">
        <Button variant="ghost"
          className="ck-chat-disclosure"
          aria-expanded={inboxOpen}
          aria-controls="ck-chat-inbox-body"
          onClick={() => setInboxOpen((open) => !open)}
        >
          {inboxOpen ? <ChevronDown size={15} strokeWidth={1.75} aria-hidden="true" /> : <ChevronRight size={15} strokeWidth={1.75} aria-hidden="true" />}
          Inbox{inbox ? ` (${inboxCount})` : ""}
        </Button>
        {inboxOpen && (
          <div id="ck-chat-inbox-body" className="ck-chat-inbox">
            <p className="ck-sub">
              Messages for chats on this computer that they have not read yet. Opening this list does not mark them read.
            </p>
            {inboxCount === 0 && <p className="ck-sub">No unread messages.</p>}
            {inbox?.messages.map((m) => (
              <div key={m.id} className="ck-chat-message">
                <div className="ck-sub">
                  {m.from} to {m.chat} · {lastSeen(m.receivedMs)}
                </div>
                <pre>{m.body}</pre>
              </div>
            ))}
            {inbox && inbox.evicted > 0 && (
              <p className="ck-sub">{plural(inbox.evicted, "older message")} dropped to keep the inbox small.</p>
            )}
          </div>
        )}
      </section>

      <section className="ck-chat-panel" aria-label="Linked computers">
        <h2>Link a computer</h2>
        <p className="ck-sub">
          Link each computer to the other, so replies can come back. The computer must be reachable over ssh with keys
          that work. Checking the link can take a little while.
        </p>
        {status?.links.map((link) => (
          <div key={link.device} className="ck-chat-link-row">
            <span className="ck-chat-link-name">{link.device} <span className="ck-sub">{link.ssh}</span></span>
            <Button size="sm" variant="secondary" onClick={() => setUnlinking(link.device)}>
              Unlink
            </Button>
          </div>
        ))}
        {status && status.links.length === 0 && <p className="ck-sub">No linked computers yet.</p>}
        <div className="ck-chat-form">
          <label className="ck-chat-field">
            <span>Computer name</span>
            <Input type="text" value={linkDevice} placeholder="mac-mini" onChange={(e) => setLinkDevice(e.target.value)} />
          </label>
          <label className="ck-chat-field">
            <span>ssh host</span>
            <Input type="text" value={linkSsh} placeholder="mac-mini.local" onChange={(e) => setLinkSsh(e.target.value)} />
          </label>
          <label className="ck-chat-field">
            <span>Path of pulse there (optional)</span>
            <Input type="text" value={linkPulse} placeholder="pulse" onChange={(e) => setLinkPulse(e.target.value)} />
          </label>
          <Button size="sm" variant="primary" disabled={!canLink} onClick={linkComputer}>
            {linking ? "Linking…" : "Link"}
          </Button>
        </div>
        {linkNote && <div className="ck-sub" role="status">{linkNote}</div>}
        {linkError && <div className="error" role="alert">{linkError}</div>}
      </section>

      {unlinking && (
        <ConfirmDialog
          danger
          title={`Unlink ${unlinking}?`}
          description="Chats there stop being reachable from this computer until you link it again."
          confirmLabel="Unlink"
          onConfirm={() => unlink(unlinking)}
          onCancel={() => setUnlinking(null)}
        />
      )}
    </div>
  );
}
