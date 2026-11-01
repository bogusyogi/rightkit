import { Checkbox } from "@rightkit/react-shell/react";
// D4 — AttentionPanel: typed unread / working / needs-input / failed /
// complete / snoozed state, transcript density modes that preserve durable
// history, project notification policy, quiet hours, deep links.
//
// Component contract:
//   - props: workspaceRoot + config
//   - state from `useAttentionStore` (list + grouped + counts + policy + quiet hours)
//   - density mode toggles Summary / Normal / Verbose, durable history preserved
//   - snooze / unsnooze mutate state through typed attention calls
//   - banner surfaces the highest-priority state (needsInput or failed)
//
// This component owns only D4 surface area. It does NOT edit AppBody,
// WorkbenchContent, TitleBar, or Settings.

import { useEffect, useState } from "react";

import { useAttentionStore } from "@rightkit/agent-ui-core/state/attentionStore";
import type {
  AttentionState,
  DaemonConfig,
  QuietHoursWindow,
  TranscriptDensity,
} from "@rightkit/agent-ui-core/lib/types";

interface AttentionPanelProps {
  config: DaemonConfig | null;
  workspaceRoot: string;
}

const STATE_ORDER: AttentionState[] = [
  "needsInput",
  "failed",
  "working",
  "unread",
  "snoozed",
  "complete",
];

const DENSITY_OPTIONS: Array<{ value: TranscriptDensity; label: string }> = [
  { value: "summary", label: "Summary" },
  { value: "normal", label: "Normal" },
  { value: "verbose", label: "Verbose" },
];

function stateLabel(state: AttentionState): string {
  switch (state) {
    case "unread":
      return "Unread";
    case "working":
      return "Working";
    case "needsInput":
      return "Needs input";
    case "failed":
      return "Failed";
    case "complete":
      return "Complete";
    case "snoozed":
      return "Snoozed";
  }
}

export default function AttentionPanel({ config, workspaceRoot }: AttentionPanelProps) {
  const list = useAttentionStore((state) => state.list);
  const banner = useAttentionStore((state) => state.banner);
  const density = useAttentionStore((state) => state.density);
  const policy = useAttentionStore((state) => state.policy);
  const quietHours = useAttentionStore((state) => state.quietHours);
  const pending = useAttentionStore((state) => state.pending);
  const error = useAttentionStore((state) => state.error);
  const loadItems = useAttentionStore((state) => state.loadItems);
  const setItemState = useAttentionStore((state) => state.setItemState);
  const snoozeItem = useAttentionStore((state) => state.snoozeItem);
  const unsnoozeItem = useAttentionStore((state) => state.unsnoozeItem);
  const loadDensity = useAttentionStore((state) => state.loadDensity);
  const setDensityMode = useAttentionStore((state) => state.setDensityMode);
  const loadPolicy = useAttentionStore((state) => state.loadPolicy);
  const savePolicy = useAttentionStore((state) => state.savePolicy);
  const loadQuietHours = useAttentionStore((state) => state.loadQuietHours);
  const saveQuietHours = useAttentionStore((state) => state.saveQuietHours);
  const resolveDeeplinkTarget = useAttentionStore((state) => state.resolveDeeplinkTarget);
  const loadBanner = useAttentionStore((state) => state.loadBanner);

  const [resolvedPath, setResolvedPath] = useState<string | null>(null);
  const [quietHoursEnabled, setQuietHoursEnabled] = useState(false);
  const [quietHoursText, setQuietHoursText] = useState("");
  const [policyEnabled, setPolicyEnabled] = useState(true);
  const [policyQuietSpam, setPolicyQuietSpam] = useState(true);
  const [policyMaxItems, setPolicyMaxItems] = useState(50);
  const [policyPersistHours, setPolicyPersistHours] = useState(24);

  useEffect(() => {
    if (!config || !workspaceRoot.trim()) return;
    void loadItems(config, workspaceRoot);
    void loadDensity(config, workspaceRoot);
    void loadPolicy(config, workspaceRoot);
    void loadQuietHours(config, workspaceRoot);
    void loadBanner(config, workspaceRoot);
  }, [config, workspaceRoot, loadItems, loadDensity, loadPolicy, loadQuietHours, loadBanner]);

  useEffect(() => {
    if (!policy) return;
    setPolicyEnabled(policy.enabled);
    setPolicyQuietSpam(policy.quietProgressSpam);
    setPolicyMaxItems(policy.maxItemsPerSession);
    setPolicyPersistHours(policy.persistCompleteForHours);
  }, [policy]);

  useEffect(() => {
    if (!quietHours) return;
    setQuietHoursEnabled(quietHours.enabled);
    setQuietHoursText(quietHours.windows.map((w) => `${w.start}-${w.end}`).join(", "));
  }, [quietHours]);

  const counts = list?.counts;
  const currentDensity = density?.density ?? list?.density ?? "normal";

  return (
    <section
      aria-label="Attention"
      className="grid gap-3 rounded-xl border border-line bg-panel p-3"
    >
      <header className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <h2 className="text-sm font-semibold text-text">Attention</h2>
          <p className="mt-0.5 text-[12px] text-text-3">
            {counts && counts.needsInput + counts.failed > 0
              ? `${counts.needsInput + counts.failed} item${counts.needsInput + counts.failed === 1 ? "" : "s"} need a decision.`
              : "Nothing needs a decision right now."}
          </p>
        </div>
        <fieldset className="flex flex-wrap gap-1 border-0 p-0 m-0" aria-label="Transcript density">
          {DENSITY_OPTIONS.map((option) => {
            const selected = currentDensity === option.value;
            return (
              <button
                key={option.value}
                type="button"
                aria-pressed={selected}
                disabled={!config || pending === "set-density"}
                onClick={() => config && void setDensityMode(config, workspaceRoot, option.value)}
                className={
                  selected
                    ? "h-7 shrink-0 whitespace-nowrap rounded-md border border-accent bg-accent px-2 text-[11px] font-semibold text-on-accent"
                    : "h-7 shrink-0 whitespace-nowrap rounded-md border border-line bg-bg px-2 text-[11px] text-text-2 hover:text-text"
                }
                data-testid={`density-${option.value}`}
              >
                {option.label}
              </button>
            );
          })}
        </fieldset>
      </header>

      {banner ? (
        <div
          className="grid gap-1 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] p-3 text-[12px]"
          data-testid="attention-banner"
        >
          <div className="flex items-center gap-2">
            <span className="font-semibold uppercase text-text-2">{stateLabel(banner.state)}</span>
            <span className="text-text-3">{banner.generatedAt}</span>
          </div>
          <h3 className="text-[13px] font-semibold text-text">{banner.title}</h3>
          <p className="text-text-2">{banner.body}</p>
          {banner.deeplink ? (
            <button
              type="button"
              disabled={!config || pending === "deeplink"}
              onClick={async () => {
                const deeplink = banner.deeplink;
                if (!config || !deeplink) return;
                const result = await resolveDeeplinkTarget(
                  config,
                  workspaceRoot,
                  deeplink.target,
                  deeplink.params,
                );
                setResolvedPath(result?.path ?? null);
              }}
              className="h-8 w-fit rounded-lg border border-line bg-bg px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-40"
            >
              Open
            </button>
          ) : null}
          {resolvedPath ? (
            <p className="text-[11px] text-text-3">
              resolved path: <code>{resolvedPath}</code>
            </p>
          ) : null}
        </div>
      ) : null}

      {counts ? (
        <div
          className="flex flex-wrap gap-2 text-[11px] text-text-3"
          data-testid="attention-counts"
        >
          {STATE_ORDER.map((state) => (
            <span
              key={state}
              className="rounded-full border border-line bg-bg px-2 py-1"
              data-testid={`attention-count-${state}`}
            >
              <span className="font-medium text-text-2">{stateLabel(state)}</span>
              <span className="ml-1">{counts[state]}</span>
            </span>
          ))}
        </div>
      ) : null}

      <div className="grid gap-2">
        <h3 className="text-[12px] font-semibold text-text-2">Items</h3>
        <ul className="grid gap-2" data-testid="attention-items">
          {list?.items.length ? (
            list.items.map((item) => (
              <li
                key={item.id}
                className="grid gap-1 rounded-lg border border-line bg-bg p-3 text-[12px]"
                data-testid={`attention-item-${item.id}`}
              >
                <div className="flex flex-wrap items-center gap-2">
                  <span className="font-semibold uppercase text-text-2">
                    {stateLabel(item.state)}
                  </span>
                  <span className="text-text-3">{item.source}</span>
                  {item.snoozedUntil ? (
                    <span className="text-text-3">snoozed until {item.snoozedUntil}</span>
                  ) : null}
                </div>
                <h4 className="text-[13px] font-semibold text-text">{item.title}</h4>
                <p className="text-text-2">{item.body}</p>
                <details className="mt-1">
                  <summary className="cursor-pointer text-[11px] font-medium text-text-2">
                    Update status
                  </summary>
                  <div className="mt-2 flex flex-wrap items-center gap-2">
                    {(
                      ["unread", "working", "needsInput", "failed", "complete"] as AttentionState[]
                    ).map((target) => (
                      <button
                        key={target}
                        type="button"
                        disabled={!config || pending === `state:${item.id}`}
                        onClick={() =>
                          config && void setItemState(config, workspaceRoot, item.id, target)
                        }
                        className="h-7 rounded-md border border-line bg-panel px-2 text-[11px] text-text-2 hover:text-text disabled:opacity-40"
                      >
                        Mark {stateLabel(target).toLowerCase()}
                      </button>
                    ))}
                    {item.state === "snoozed" ? (
                      <button
                        type="button"
                        disabled={!config || pending === `unsnooze:${item.id}`}
                        onClick={() => config && void unsnoozeItem(config, workspaceRoot, item.id)}
                        className="h-7 rounded-md border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-2 text-[11px] font-semibold text-text disabled:opacity-40"
                      >
                        Unsnooze
                      </button>
                    ) : (
                      <button
                        type="button"
                        disabled={!config || pending === `snooze:${item.id}`}
                        onClick={() =>
                          config &&
                          void snoozeItem(
                            config,
                            workspaceRoot,
                            item.id,
                            new Date(Date.now() + 60 * 60 * 1000).toISOString(),
                          )
                        }
                        className="h-7 rounded-md border border-line bg-panel px-2 text-[11px] text-text-2 hover:text-text disabled:opacity-40"
                      >
                        Snooze 1h
                      </button>
                    )}
                  </div>
                </details>
              </li>
            ))
          ) : (
            <li className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text-3">
              No attention items. Needs-input, failed, working, and unread items will land here as
              agents run.
            </li>
          )}
        </ul>
      </div>

      <details className="rounded-lg border border-line bg-bg" data-testid="attention-settings">
        <summary className="cursor-pointer px-3 py-2 text-[12px] font-semibold text-text-2">
          Notification settings{" "}
          <span className="ml-1 font-normal text-text-3">
            {list?.quietHoursActive
              ? "quiet hours active"
              : density?.durableHistory
                ? "history preserved"
                : ""}
          </span>
        </summary>
        <div className="grid gap-4 border-t border-line p-3">
          <div className="grid gap-2">
            <h3 className="text-[12px] font-semibold text-text-2">Project policy</h3>
            <div className="flex flex-wrap items-center gap-2">
              <Checkbox checked={policyEnabled} onChange={(checked) => setPolicyEnabled(checked)} label="Notifications enabled" className="flex items-center gap-2 text-[12px] text-text-2" />
              <Checkbox checked={policyQuietSpam} onChange={(checked) => setPolicyQuietSpam(checked)} label="Suppress progress spam" className="flex items-center gap-2 text-[12px] text-text-2" />
              <label className="flex items-center gap-2 text-[12px] text-text-2">
                Max items/session
                <input
                  type="number"
                  min={1}
                  value={policyMaxItems}
                  onChange={(event) => setPolicyMaxItems(Number(event.target.value) || 1)}
                  className="h-8 w-20 rounded-lg border border-line bg-bg px-2 text-[12px] text-text"
                />
              </label>
              <label className="flex items-center gap-2 text-[12px] text-text-2">
                Persist complete (hours)
                <input
                  type="number"
                  min={0}
                  value={policyPersistHours}
                  onChange={(event) => setPolicyPersistHours(Number(event.target.value) || 0)}
                  className="h-8 w-20 rounded-lg border border-line bg-bg px-2 text-[12px] text-text"
                />
              </label>
              <button
                type="button"
                disabled={!config || pending === "save-policy"}
                onClick={() =>
                  config &&
                  void savePolicy(config, {
                    workspaceRoot,
                    enabled: policyEnabled,
                    quietProgressSpam: policyQuietSpam,
                    maxItemsPerSession: policyMaxItems,
                    persistCompleteForHours: policyPersistHours,
                  })
                }
                className="h-8 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
              >
                Save policy
              </button>
            </div>
          </div>

          <div className="grid gap-2">
            <h3 className="text-[12px] font-semibold text-text-2">Quiet hours</h3>
            <div className="flex flex-wrap items-center gap-2">
              <Checkbox checked={quietHoursEnabled} onChange={(checked) => setQuietHoursEnabled(checked)} label="Enable quiet hours" className="flex items-center gap-2 text-[12px] text-text-2" />
              <input
                type="text"
                aria-label="Quiet hours"
                placeholder="22:00-07:00, 12:30-13:30"
                value={quietHoursText}
                onChange={(event) => setQuietHoursText(event.target.value)}
                className="h-8 min-w-[20ch] rounded-lg border border-line bg-bg px-2 text-[12px] text-text"
              />
              <button
                type="button"
                disabled={!config || pending === "save-quiet-hours"}
                onClick={() =>
                  config &&
                  void saveQuietHours(config, {
                    workspaceRoot,
                    enabled: quietHoursEnabled,
                    windows: parseQuietHoursWindows(quietHoursText),
                  })
                }
                className="h-8 rounded-lg border border-accent bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] px-3 text-[12px] font-semibold text-text disabled:opacity-40"
              >
                Save quiet hours
              </button>
            </div>
          </div>
        </div>
      </details>

      {error ? (
        <p
          role="alert"
          className="rounded-lg border border-line bg-bg px-3 py-2 text-[12px] text-text"
        >
          {error}
        </p>
      ) : null}
    </section>
  );
}

function parseQuietHoursWindows(input: string): QuietHoursWindow[] {
  return input
    .split(/[,\n]/)
    .map((segment) => segment.trim())
    .filter(Boolean)
    .map((segment) => {
      const match = segment.match(/^(\d{2}:\d{2})\s*-\s*(\d{2}:\d{2})$/);
      if (!match) return null;
      return { start: match[1], end: match[2] };
    })
    .filter((window): window is QuietHoursWindow => window !== null);
}
