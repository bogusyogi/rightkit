import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  CLI_PROVIDER_IDS,
  getProviderDisplay,
  type CliProviderId,
} from "@rightkit/agent-ui-core/lib/cliProviders";

export interface RepairTabProps {
  providerId?: CliProviderId | null;
  onCompleted?: (action: string) => void;
}

const ACTIONS = [
  {
    id: "recover_binding",
    label: "Recover binding",
    description: "Restore the active provider binding without changing credentials.",
  },
  {
    id: "re_qualify_harness",
    label: "Re-qualify harness",
    description: "Run the provider harness qualification probes again.",
  },
  {
    id: "clear_active_binding",
    label: "Clear active binding",
    description: "Stop using the current provider binding for new work.",
  },
] as const;

export default function RepairTab({ providerId, onCompleted }: RepairTabProps) {
  const [pending, setPending] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const target = providerId ?? null;

  const run = async (action: (typeof ACTIONS)[number]["id"]) => {
    setPending(action);
    setMessage(null);
    setError(null);
    try {
      await invoke(action, target ? { providerId: target } : undefined);
      setMessage(`${ACTIONS.find((item) => item.id === action)?.label} completed.`);
      onCompleted?.(action);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "The repair action is unavailable.");
    } finally {
      setPending(null);
    }
  };

  return (
    <section className="flex max-w-[720px] flex-col gap-4 p-6" data-testid="repair-tab">
      <header>
        <h3 className="text-[16px] font-bold text-text">Continuity and repair</h3>
        <p className="mt-1 text-[12px] text-text-3">
          Repair provider continuity without exposing or replacing vendor CLI credentials.
          {target ? ` Current provider: ${getProviderDisplay(target).displayName}.` : ""}
        </p>
      </header>
      {message ? (
        <p className="rounded-md border border-success/40 bg-success/10 p-2 text-[12px] text-success">
          {message}
        </p>
      ) : null}
      {error ? (
        <p className="rounded-md border border-danger/40 bg-danger/10 p-2 text-[12px] text-danger">
          {error}
        </p>
      ) : null}
      <div className="flex flex-col gap-2">
        {ACTIONS.map((item) => (
          <article
            key={item.id}
            className="flex items-center justify-between gap-4 rounded-xl border border-line bg-panel p-3"
          >
            <div className="min-w-0">
              <strong className="block text-[13px] text-text">{item.label}</strong>
              <p className="text-[11px] text-text-3">{item.description}</p>
            </div>
            <button
              type="button"
              onClick={() => void run(item.id)}
              disabled={pending !== null}
              className="shrink-0 rounded-md border border-line px-3 py-1.5 text-[12px] font-semibold text-text-2 hover:border-accent hover:text-text disabled:opacity-50"
            >
              {pending === item.id ? "Working…" : item.label}
            </button>
          </article>
        ))}
      </div>
      <p className="text-[11px] text-text-3">
        Supported providers:{" "}
        {CLI_PROVIDER_IDS.map((id) => getProviderDisplay(id).shortLabel).join(", ")}.
      </p>
    </section>
  );
}
