// Settings · Agents — pair the local mailright-engine agent, set its profile (read only, draft,
// or send with approval), revoke it, and read the agent audit log. Pairing is always read-only.
// The pairing key is written to the OS credential store by the app and never reaches this view.
// GAP: react-shell has no Select, so the profile picker stays a native select.
import { useCallback, useEffect, useRef, useState } from "react";
import { SettingRow, SettingsGroup } from "@rightkit/settings-ui/core";
import { Badge, Button, Input, Toggle, Select } from "@rightkit/react-shell/react";
import { useAppStore } from "@rightkit/mail-ui/store/index";
import {
  agentAuditList,
  agentClearExports,
  agentImportFile,
  agentLabelDrafts,
  agentPair,
  agentRevoke,
  agentSetLabelDrafts,
  agentSetProfile,
  agentsList,
  type AgentAuditRow,
  type AgentProfile,
  type AgentRow,
} from "@rightkit/mail-ui/lib/ipc";
import { showDialog } from "@rightkit/mail-ui/hooks/dialog";
import { AGENT_FILE_MAX, fileToBase64 } from "../lib/agentFile";
import { SettingsAgentsAudit } from "./SettingsAgentsAudit";
import { hostAppName } from "../../appName";

const PAGE = 50;

const PROFILES: { value: AgentProfile; label: string }[] = [
  { value: "read_only", label: "Read only" },
  { value: "draft", label: "Draft (can prepare drafts for you to send)" },
  { value: "send_with_approval", label: "Send with approval (can ask to send drafts it made)" },
];

const SEND_NOTICE =
  `This agent can ask to send mail. Nothing is sent until you approve each message in ${hostAppName()}.`;

const KEY_NOTICE =
  "The pairing key is stored in this computer's credential store. Any program running under your user account can read it. Revoke an agent here to stop it.";

function profileOf(p: string): AgentProfile {
  return p === "draft" || p === "send_with_approval" ? p : "read_only";
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function when(ms: number): string {
  return new Date(ms).toLocaleString();
}

export function SettingsAgentsSection() {
  const active = useAppStore((s) => s.settingsTab === "diagnostics");
  const setErr = useAppStore((s) => s.setErr);
  const accounts = useAppStore((s) => s.accounts);
  const [agents, setAgents] = useState<AgentRow[]>([]);
  const [rows, setRows] = useState<AgentAuditRow[]>([]);
  const [more, setMore] = useState(false);
  const [name, setName] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [labelDrafts, setLabelDrafts] = useState(false);
  const [exportNote, setExportNote] = useState("");
  const [fileNote, setFileNote] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);
  // The agent the open file dialog is for; set by the click, never by the file.
  const fileFor = useRef<AgentRow | null>(null);

  const reload = useCallback(async () => {
    try {
      const [list, page, label] = await Promise.all([agentsList(), agentAuditList(PAGE), agentLabelDrafts()]);
      setAgents(list);
      setLabelDrafts(label);
      setRows(page);
      setMore(page.length === PAGE);
    } catch (e) {
      setErr(errorText(e));
    }
  }, [setErr]);

  useEffect(() => {
    if (active) void reload();
  }, [active, reload]);

  const loadMore = async () => {
    const last = rows[rows.length - 1];
    if (!last) return;
    try {
      const page = await agentAuditList(PAGE, last.id);
      setRows((r) => [...r, ...page]);
      setMore(page.length === PAGE);
    } catch (e) {
      setErr(errorText(e));
    }
  };

  // One guarded action: skips while busy, reports failures, then reloads.
  const act = async (fn: () => Promise<void>) => {
    if (busy) return;
    setBusy(true);
    try {
      await fn();
    } catch (e) {
      setErr(errorText(e));
    } finally {
      setBusy(false);
    }
    await reload();
  };

  const confirm = async (message: string) => (await showDialog({ kind: "confirm", message })) === "ok";

  const pair = () =>
    !name.trim() || chosen.length === 0
      ? Promise.resolve()
      : act(async () => {
          await agentPair(name.trim(), chosen);
          setName("");
          setChosen([]);
        });

  const revoke = async (agent: AgentRow) => {
    if (busy || !(await confirm(`Revoke "${agent.name}"? It will stop working at once.`))) return;
    await act(() => agentRevoke(agent.id));
  };

  const changeProfile = async (agent: AgentRow, profile: AgentProfile) => {
    if (busy) return;
    if (profile === "send_with_approval" && !(await confirm(`Allow "${agent.name}" to ask to send mail? ${SEND_NOTICE}`))) {
      await reload();
      return;
    }
    await act(async () => { await agentSetProfile(agent.id, profile); });
  };

  const changeLabelDrafts = (enabled: boolean) =>
    act(async () => {
      await agentSetLabelDrafts(enabled);
      setLabelDrafts(enabled);
    });

  const clearExports = async () => {
    if (
      busy ||
      !(await confirm("Delete all files agents have exported from your mail? Your mail in Gmail is not touched."))
    )
      return;
    await act(async () => {
      const n = await agentClearExports();
      setExportNote(n === 1 ? "Removed 1 exported file." : `Removed ${n} exported files.`);
    });
  };

  const pickFile = (a: AgentRow) => {
    fileFor.current = a;
    fileInput.current?.click();
  };

  const giveFile = async (file: File | undefined) => {
    const agent = fileFor.current;
    fileFor.current = null;
    if (!file || !agent || busy) return;
    if (file.size > AGENT_FILE_MAX) {
      setErr("That file is larger than 18 MB, the most one draft can carry.");
      return;
    }
    if (
      !(await confirm(
        `Give ${agent.name} a copy of "${file.name}"? It will be able to attach it to drafts it makes. The original stays where it is.`,
      ))
    )
      return;
    await act(async () => {
      const done = await agentImportFile(agent.id, file.name, await fileToBase64(file));
      setFileNote(
        done.extension_blocked
          ? `Saved as ${done.name}, but this file type can run code, so ${agent.name} cannot attach it.`
          : `Saved as ${done.name}. Tell ${agent.name} to attach ${done.name} to its draft.`,
      );
    });
  };

  const toggle = (id: string) => setChosen((c) => (c.includes(id) ? c.filter((x) => x !== id) : [...c, id]));

  return (
    <>
      <SettingsGroup
        title="Agents"
        description={
          agents.length === 0
            ? "No agents are paired. A paired agent can search and read mail in the accounts you choose."
            : "A paired agent can search and read mail in the accounts you choose. An agent cannot delete mail, move it to Trash or Spam, or change your settings."
        }
      >
        {agents.map((a) => (
          <SettingRow
            key={a.id}
            id={`agents.${a.id}`}
            label={
              <>
                {a.name}{" "}
                <Badge tone={a.revoked_at ? "neutral" : "ok"}>
                  {a.revoked_at ? `revoked ${when(a.revoked_at)}` : "active"}
                </Badge>
              </>
            }
            searchText={`agent ${a.name} ${a.accounts.join(" ")}`}
            description={
              <>
                {a.accounts.join(", ") || "no accounts"}
                {!a.revoked_at && a.profile === "send_with_approval" && <> · {SEND_NOTICE}</>}
              </>
            }
            stacked
          >
            {!a.revoked_at && (
              <div className="flex flex-wrap items-center gap-2">
                <Select
                  value={profileOf(a.profile)}
                  disabled={busy}
                  label={`Profile for ${a.name}`}
                  options={PROFILES}
                  onChange={(v) => void changeProfile(a, v as AgentProfile)}
                />
                {a.profile !== "read_only" && (
                  <Button
                    size="sm"
                    variant="secondary"
                    onClick={() => pickFile(a)}
                    disabled={busy}
                    aria-label={`Give ${a.name} a file`}
                  >
                    Give a file…
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="danger"
                  onClick={() => void revoke(a)}
                  disabled={busy}
                  aria-label={`Revoke ${a.name}`}
                >
                  Revoke
                </Button>
              </div>
            )}
          </SettingRow>
        ))}
        {fileNote && <SettingRow id="agents.fileNote" label={<span role="status">{fileNote}</span>} searchText="agent file" />}
        <SettingRow
          id="agents.labelDrafts"
          label="Label drafts made by agents"
          description={'Adds the Gmail label "Agent draft" to drafts an agent creates. The label is visible in Gmail, so it shows that an agent made the draft.'}
        >
          <Toggle
            label="Label drafts made by agents"
            checked={labelDrafts}
            disabled={busy}
            onChange={(on) => void changeLabelDrafts(on)}
          />
        </SettingRow>
        <SettingRow
          id="agents.exports"
          label="Exported attachments"
          description={`An agent can save attachments to a folder ${hostAppName()} owns, up to 25 MB a file and 200 MB per agent. Nothing there is deleted until you clear it. ${exportNote} "Give a file" copies a file you choose into the same folder so the agent can attach it to a draft (18 MB at most); the agent never names a path on your computer.`}
        >
          <Button size="sm" variant="secondary" onClick={() => void clearExports()} disabled={busy}>
            Clear exported attachments
          </Button>
        </SettingRow>
      </SettingsGroup>

      <Input
        ref={fileInput}
        type="file"
        style={{ display: "none" }}
        aria-hidden="true"
        tabIndex={-1}
        onChange={(e) => {
          const f = e.target.files?.[0];
          e.target.value = "";
          void giveFile(f);
        }}
      />

      <SettingsGroup title="Pair local agent" description={KEY_NOTICE}>
        <SettingRow id="agents.pairName" label="Agent name" keywords={["pair", "new"]}>
          <Input
            aria-label="Agent name"
            value={name}
            maxLength={80}
            onChange={(e) => setName(e.target.value)}
          />
        </SettingRow>
        {accounts.map((acct) => (
          <SettingRow
            key={acct.id}
            id={`agents.pairAccount.${acct.id}`}
            label={acct.id}
            description="The agent may read this account."
            searchText={`pair agent account ${acct.id}`}
          >
            <Toggle
              label={`Agent may read ${acct.id}`}
              checked={chosen.includes(acct.id)}
              onChange={() => toggle(acct.id)}
            />
          </SettingRow>
        ))}
        <SettingRow id="agents.pair" label="Pair the agent">
          <Button
            size="sm"
            variant="primary"
            onClick={() => void pair()}
            disabled={busy || !name.trim() || chosen.length === 0}
          >
            Pair local agent
          </Button>
        </SettingRow>
      </SettingsGroup>

      <SettingsAgentsAudit rows={rows} more={more} onLoadMore={() => void loadMore()} />
    </>
  );
}
