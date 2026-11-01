import { Select } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useState } from "react";
import {
  createBot,
  deleteBot,
  exportBot,
  getBot,
  importBot,
  listBots,
  runBot,
  updateBot,
  type BotListItem,
  type BotProfileView,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";

type TriggerKind = "cron" | "once" | "interval" | "monitor";

function blankBot(): BotProfileView {
  return {
    schemaVersion: 1,
    id: "",
    name: "",
    version: 1,
    rolePack: "assistant",
    model: "catalog/default",
    instructions: "",
    browser: { mode: "disabled", allowNetwork: false },
    budget: {},
    verifier: { required: false },
    triggers: [],
    concurrency: "skip",
    concurrencyCap: 1,
    enabled: true,
  };
}

function triggerExpression(kind: TriggerKind, expression: string) {
  void kind;
  // Contract carries kind separately; daemon adds its scheduler prefix once.
  return expression.trim();
}

function splitTrigger(expression: string | null | undefined): {
  kind: TriggerKind;
  expression: string;
} {
  const value = expression?.trim() ?? "";
  for (const kind of ["once", "interval", "monitor"] as const) {
    if (value.startsWith(`${kind}:`)) return { kind, expression: value.slice(kind.length + 1) };
  }
  return { kind: "cron", expression: value };
}

export function BotsPane({
  config,
  workspaceRoot,
}: {
  config: DaemonConfig;
  workspaceRoot: string;
}) {
  const [bots, setBots] = useState<BotListItem[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [profile, setProfile] = useState<BotProfileView | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [importText, setImportText] = useState("");
  const [draft, setDraft] = useState<BotProfileView | null>(null);
  const [editingExisting, setEditingExisting] = useState(false);
  const [triggerKind, setTriggerKind] = useState<TriggerKind>("cron");
  const [triggerValue, setTriggerValue] = useState("");

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const rows = await listBots(config);
      setBots(rows);
      setSelectedId((current) =>
        current && rows.some((row) => row.id === current) ? current : (rows[0]?.id ?? null),
      );
    } catch (value) {
      setError(value instanceof Error ? value.message : "Bots could not be loaded");
    }
  }, [config]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!selectedId) {
      setProfile(null);
      return;
    }
    let cancelled = false;
    void getBot(config, selectedId)
      .then((next) => {
        if (!cancelled) setProfile(next);
      })
      .catch((value) => {
        if (!cancelled)
          setError(value instanceof Error ? value.message : "Bot could not be loaded");
      });
    return () => {
      cancelled = true;
    };
  }, [config, selectedId]);

  const perform = async (label: string, action: () => Promise<void>) => {
    setBusy(label);
    setError(null);
    setNotice(null);
    try {
      await action();
    } catch (value) {
      setError(value instanceof Error ? value.message : `${label} failed`);
    } finally {
      setBusy(null);
    }
  };

  const saveDraft = () =>
    void perform(editingExisting ? "Save bot" : "Create bot", async () => {
      if (!draft) return;
      if (!draft.id.trim() || !draft.name.trim() || !draft.rolePack.trim() || !draft.model.trim()) {
        throw new Error("ID, name, role pack & model are required");
      }
      const next = { ...draft, id: draft.id.trim(), name: draft.name.trim() };
      const saved = editingExisting
        ? await updateBot(config, next, workspaceRoot)
        : await createBot(config, next, workspaceRoot);
      setDraft(null);
      await refresh();
      setSelectedId(saved.id);
      setProfile(saved);
      setNotice(
        editingExisting
          ? "Bot profile & trigger schedules saved"
          : "Bot created with isolated runs",
      );
    });

  const addTrigger = () => {
    if (!draft || !triggerValue.trim()) return;
    const trigger = {
      id: `${triggerKind}-${crypto.randomUUID()}`,
      schedule: triggerKind,
      expression: triggerExpression(triggerKind, triggerValue),
      paused: false,
    };
    setDraft({ ...draft, triggers: [...(draft.triggers ?? []), trigger] });
    setTriggerValue("");
  };

  return (
    <section className="flex h-full min-h-0 flex-col bg-panel" aria-label="Bots">
      <header className="flex flex-none items-center gap-2 border-b border-line px-3 py-2">
        <div className="min-w-0 flex-1">
          <h2 className="text-[13px] font-semibold text-text">Bots</h2>
          <p className="text-[10px] text-text-3">Versioned profiles · fresh isolated runs</p>
        </div>
        <button
          type="button"
          disabled={Boolean(busy)}
          onClick={() => void refresh()}
          className="rounded border border-line px-2 py-1 text-[10px] text-text-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-40"
        >
          Refresh
        </button>
        <button
          type="button"
          disabled={Boolean(busy)}
          onClick={() => {
            setDraft(blankBot());
            setEditingExisting(false);
            setTriggerValue("");
          }}
          className="rounded bg-accent px-2 py-1 text-[10px] font-semibold text-bg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus disabled:opacity-40"
        >
          Create bot
        </button>
      </header>

      {error ? (
        <p role="alert" className="m-3 rounded border border-danger/50 p-2 text-[11px] text-danger">
          {error}
        </p>
      ) : null}
      {notice ? (
        <p
          role="status"
          className="m-3 rounded border border-accent-2/50 p-2 text-[11px] text-accent-2"
        >
          {notice}
        </p>
      ) : null}

      <div className="grid min-h-0 flex-1 grid-cols-[minmax(120px,0.7fr)_minmax(0,1.3fr)]">
        <nav aria-label="Bot profiles" className="min-h-0 overflow-y-auto border-r border-line p-2">
          {bots.length === 0 ? (
            <p className="p-2 text-[11px] text-text-3">No bots installed.</p>
          ) : (
            <ul className="space-y-1">
              {bots.map((bot) => (
                <li key={bot.id}>
                  <button
                    type="button"
                    aria-current={selectedId === bot.id ? "true" : undefined}
                    onClick={() => setSelectedId(bot.id)}
                    className={`w-full rounded p-2 text-left focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus ${selectedId === bot.id ? "bg-panel-2 text-text" : "text-text-2 hover:bg-panel-2"}`}
                  >
                    <span className="block truncate text-[11px] font-medium">{bot.name}</span>
                    <code className="block truncate text-[9px] text-text-3">{bot.id}</code>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </nav>

        <div className="min-h-0 overflow-y-auto p-3">
          {profile ? (
            <article>
              <div className="flex items-start gap-2">
                <div className="min-w-0 flex-1">
                  <h3 className="text-[13px] font-semibold text-text">{profile.name}</h3>
                  <p className="text-[10px] text-text-3">
                    {profile.rolePack} · {profile.model} · v{profile.version}
                  </p>
                </div>
                <span
                  className={`rounded-full border px-2 py-0.5 text-[9px] ${profile.enabled ? "border-accent-2/50 text-accent-2" : "border-warning/50 text-warning"}`}
                >
                  {profile.enabled ? "Enabled" : "Paused"}
                </span>
              </div>
              {profile.description || profile.instructions ? (
                <p className="mt-3 whitespace-pre-wrap text-[11px] leading-5 text-text-2">
                  {profile.description || profile.instructions}
                </p>
              ) : null}
              <dl className="mt-3 grid grid-cols-2 gap-2 text-[10px]">
                <div className="rounded border border-line p-2">
                  <dt className="text-text-3">Concurrency</dt>
                  <dd className="mt-0.5 text-text">
                    {profile.concurrency} · {profile.concurrencyCap}
                  </dd>
                </div>
                <div className="rounded border border-line p-2">
                  <dt className="text-text-3">Browser</dt>
                  <dd className="mt-0.5 text-text">{profile.browser?.mode ?? "disabled"}</dd>
                </div>
                <div className="rounded border border-line p-2">
                  <dt className="text-text-3">Tools</dt>
                  <dd className="mt-0.5 break-words text-text">
                    {profile.tools?.join(", ") || "None"}
                  </dd>
                </div>
                <div className="rounded border border-line p-2">
                  <dt className="text-text-3">Triggers</dt>
                  <dd className="mt-0.5 text-text">{profile.triggers?.length ?? 0}</dd>
                </div>
              </dl>
              <div className="mt-3 flex flex-wrap gap-2">
                <button
                  type="button"
                  disabled={!profile.enabled || Boolean(busy)}
                  onClick={() =>
                    void perform("Run", async () => {
                      const result = await runBot(config, profile.id, workspaceRoot);
                      setNotice(`Submitted ${result.scheduleId}`);
                    })
                  }
                  className="rounded bg-accent px-3 py-1.5 text-[11px] font-semibold text-bg focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-focus disabled:opacity-40"
                >
                  Run now
                </button>
                <button
                  type="button"
                  disabled={Boolean(busy)}
                  onClick={() =>
                    void perform(profile.enabled ? "Pause" : "Enable", async () => {
                      const next = await updateBot(
                        config,
                        { ...profile, enabled: !profile.enabled },
                        workspaceRoot,
                      );
                      setProfile(next);
                      setNotice(next.enabled ? "Bot enabled" : "Bot paused");
                    })
                  }
                  className="rounded border border-line px-3 py-1.5 text-[11px] text-text-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-40"
                >
                  {profile.enabled ? "Pause" : "Enable"}
                </button>
                <button
                  type="button"
                  disabled={Boolean(busy)}
                  onClick={() =>
                    void perform("Export", async () => {
                      const value = await exportBot(config, profile.id);
                      await navigator.clipboard.writeText(JSON.stringify(value, null, 2));
                      setNotice("Authority-free package copied");
                    })
                  }
                  className="rounded border border-line px-3 py-1.5 text-[11px] text-text-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-40"
                >
                  Copy export
                </button>
                <button
                  type="button"
                  disabled={Boolean(busy)}
                  onClick={() => {
                    setDraft(profile);
                    setEditingExisting(true);
                    setTriggerValue("");
                  }}
                  className="rounded border border-line px-3 py-1.5 text-[11px] text-text-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-40"
                >
                  Edit profile
                </button>
                <button
                  type="button"
                  disabled={Boolean(busy)}
                  onClick={() =>
                    void perform("Delete bot", async () => {
                      await deleteBot(config, profile.id, workspaceRoot);
                      setProfile(null);
                      setSelectedId(null);
                      await refresh();
                      setNotice("Bot & its trigger schedules deleted");
                    })
                  }
                  className="rounded border border-danger/50 px-3 py-1.5 text-[11px] text-danger focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-40"
                >
                  Delete bot
                </button>
              </div>
            </article>
          ) : (
            <p className="text-[11px] text-text-3">Select a bot profile.</p>
          )}

          {draft ? (
            <form
              className="mt-5 space-y-3 border-t border-line pt-3"
              onSubmit={(event) => {
                event.preventDefault();
                saveDraft();
              }}
            >
              <h3 className="text-[12px] font-semibold text-text">
                {editingExisting ? "Edit bot profile" : "Create bot profile"}
              </h3>
              <div className="grid gap-2 sm:grid-cols-2">
                {(
                  [
                    ["ID", "id"],
                    ["Name", "name"],
                    ["Role pack", "rolePack"],
                    ["Model", "model"],
                  ] as const
                ).map(([label, key]) => (
                  <label key={key} className="text-[10px] text-text-3">
                    {label}
                    <input
                      required
                      value={draft[key]}
                      disabled={editingExisting && key === "id"}
                      onChange={(event) => setDraft({ ...draft, [key]: event.target.value })}
                      className="mt-1 min-h-7 w-full rounded border border-line bg-bg px-2 text-[11px] text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-50"
                    />
                  </label>
                ))}
                <label className="text-[10px] text-text-3">
                  Concurrency
                  <Select value={draft.concurrency} options={[{ value: "skip", label: "Skip overlapping" }, { value: "queue", label: "Queue" }, { value: "parallel", label: "Parallel" }]} onChange={(value) => setDraft({ ...draft, concurrency: value })} label="Concurrency" className="mt-1 min-h-7 w-full rounded border border-line bg-bg px-2 text-[11px] text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus" />
                </label>
                <label className="text-[10px] text-text-3">
                  Concurrent runs
                  <input
                    type="number"
                    min="1"
                    value={draft.concurrencyCap}
                    onChange={(event) =>
                      setDraft({
                        ...draft,
                        concurrencyCap: Math.max(1, Number(event.target.value) || 1),
                      })
                    }
                    className="mt-1 min-h-7 w-full rounded border border-line bg-bg px-2 text-[11px] text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
                  />
                </label>
              </div>
              <label className="block text-[10px] text-text-3">
                Instructions
                <textarea
                  value={draft.instructions ?? ""}
                  onChange={(event) => setDraft({ ...draft, instructions: event.target.value })}
                  className="mt-1 min-h-20 w-full rounded border border-line bg-bg p-2 text-[11px] text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
                />
              </label>
              <fieldset className="rounded border border-line p-2">
                <legend className="px-1 text-[10px] text-text-3">Trigger schedules</legend>
                {(draft.triggers ?? []).length === 0 ? (
                  <p className="text-[10px] text-text-3">No automatic triggers.</p>
                ) : (
                  <ul className="space-y-1">
                    {draft.triggers?.map((trigger) => {
                      const parsed = splitTrigger(trigger.expression);
                      return (
                        <li
                          key={trigger.id}
                          className="flex items-center gap-2 text-[10px] text-text-2"
                        >
                          <code className="min-w-0 flex-1 truncate">
                            {parsed.kind}: {parsed.expression}
                          </code>
                          <button
                            type="button"
                            onClick={() =>
                              setDraft({
                                ...draft,
                                triggers: draft.triggers?.map((item) =>
                                  item.id === trigger.id ? { ...item, paused: !item.paused } : item,
                                ),
                              })
                            }
                            className="rounded border border-line px-2 py-1 text-[10px]"
                          >
                            {trigger.paused ? "Resume" : "Pause"}
                          </button>
                          <button
                            type="button"
                            onClick={() =>
                              setDraft({
                                ...draft,
                                triggers: draft.triggers?.filter((item) => item.id !== trigger.id),
                              })
                            }
                            className="rounded border border-danger/50 px-2 py-1 text-[10px] text-danger"
                          >
                            Remove
                          </button>
                        </li>
                      );
                    })}
                  </ul>
                )}
                <div className="mt-2 grid gap-2 sm:grid-cols-[100px_1fr_auto]">
                  <Select value={triggerKind} options={[{ value: "cron", label: "Cron" }, { value: "once", label: "One-shot epoch ms" }, { value: "interval", label: "Interval ms" }, { value: "monitor", label: "Monitor" }]} onChange={(value) => setTriggerKind(value as TriggerKind)} label="Trigger type" className="min-h-7 rounded border border-line bg-bg px-2 text-[10px] text-text" />
                  <input
                    aria-label="Trigger expression"
                    value={triggerValue}
                    onChange={(event) => setTriggerValue(event.target.value)}
                    placeholder="Expression"
                    className="min-h-7 rounded border border-line bg-bg px-2 text-[10px] text-text"
                  />
                  <button
                    type="button"
                    onClick={addTrigger}
                    disabled={!triggerValue.trim()}
                    className="rounded border border-line px-2 py-1 text-[10px] text-text-2 disabled:opacity-40"
                  >
                    Add trigger
                  </button>
                </div>
              </fieldset>
              <div className="flex gap-2">
                <button
                  type="submit"
                  disabled={Boolean(busy)}
                  className="rounded bg-accent px-3 py-1.5 text-[11px] font-semibold text-bg disabled:opacity-40"
                >
                  {editingExisting ? "Save profile" : "Create bot"}
                </button>
                <button
                  type="button"
                  onClick={() => setDraft(null)}
                  className="rounded border border-line px-3 py-1.5 text-[11px] text-text-2"
                >
                  Cancel
                </button>
              </div>
            </form>
          ) : null}

          <details className="mt-5 border-t border-line pt-3">
            <summary className="cursor-pointer text-[11px] font-medium text-text-2">
              Import authority-free package
            </summary>
            <label className="mt-2 block text-[10px] text-text-3" htmlFor="bot-import-json">
              Package JSON
            </label>
            <textarea
              id="bot-import-json"
              aria-label="Package JSON"
              value={importText}
              onChange={(event) => setImportText(event.target.value)}
              className="mt-1 min-h-24 w-full rounded border border-line bg-bg p-2 font-mono text-[10px] text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
            />
            <button
              type="button"
              disabled={!importText.trim() || Boolean(busy)}
              onClick={() =>
                void perform("Import", async () => {
                  const imported = await importBot(config, importText);
                  setImportText("");
                  await refresh();
                  setSelectedId(imported.id);
                  setNotice("Imported paused for review");
                })
              }
              className="mt-2 rounded border border-line px-3 py-1.5 text-[11px] text-text-2 focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus disabled:opacity-40"
            >
              Import paused
            </button>
          </details>
        </div>
      </div>
    </section>
  );
}
