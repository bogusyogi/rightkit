import { Select } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import {
  addApiKeyAccount,
  fetchAccounts,
  removeApiKeyAccount,
  type Account,
} from "@rightkit/agent-ui-core/lib/accounts/accounts";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";

const PROVIDERS = [
  { id: "claude", label: "Claude (Anthropic API key)" },
  { id: "openai", label: "OpenAI" },
];

/**
 * API-key accounts (issue #34). The key goes to the daemon once, which stores it
 * in the OS credential store. It is never read back, so the field is write-only
 * and clears as soon as the request is sent.
 */
export default function ApiKeyAccounts({ config }: { config?: DaemonConfig | null }) {
  const [accounts, setAccounts] = useState<Account[]>([]);
  const [label, setLabel] = useState("");
  const [provider, setProvider] = useState(PROVIDERS[0].id);
  const [apiKey, setApiKey] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!config) return;
    try {
      setAccounts((await fetchAccounts(config)).filter((a) => a.kind === "api_key"));
    } catch {
      setAccounts([]);
    }
  }, [config]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!config || pending || !label.trim() || !apiKey.trim()) return;
    const key = apiKey;
    setApiKey("");
    setPending(true);
    setError(null);
    try {
      await addApiKeyAccount(config, { label: label.trim(), provider, apiKey: key });
      setLabel("");
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not add the API key");
    } finally {
      setPending(false);
    }
  };

  const remove = async (account: Account) => {
    if (!config) return;
    setError(null);
    try {
      await removeApiKeyAccount(config, account.id);
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Could not remove the API key");
    }
  };

  return (
    <section className="rounded-xl border border-line bg-panel p-4" aria-label="API key accounts">
      <h4 className="text-[13px] font-semibold text-text">API key accounts</h4>
      <p className="mb-3 text-[12px] text-text-3">
        Keys are stored in the system credential store and are never shown again.
      </p>
      <div className="mb-3 flex flex-col gap-2">
        {accounts.map((account) => (
          <article
            key={account.id}
            className="flex items-center gap-3 rounded-lg border border-line/70 px-3 py-2"
          >
            <div className="min-w-0 flex-1">
              <strong className="block truncate text-[12px] text-text">{account.label}</strong>
              <span className="block truncate text-[11px] text-text-3">
                {account.provider} · {account.ready ? "key stored" : "key missing"}
              </span>
            </div>
            <button
              type="button"
              aria-label={`Remove ${account.label}`}
              className="rounded border border-danger/40 px-2 py-1 text-[11px] text-danger"
              onClick={() => void remove(account)}
            >
              Remove
            </button>
          </article>
        ))}
      </div>
      <form className="flex flex-wrap items-end gap-2" onSubmit={(e) => void submit(e)}>
        <label className="flex flex-col gap-1 text-[11px] text-text-3">
          Label
          <input
            type="text"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            autoComplete="off"
            className="rounded border border-line bg-transparent px-2 py-1 text-[12px] text-text"
          />
        </label>
        <label className="flex flex-col gap-1 text-[11px] text-text-3">
          Provider
          <Select value={provider} options={[...PROVIDERS.map((p) => ({ value: p.id, label: p.label }))]} onChange={(value) => setProvider(value)} label="Provider" className="rounded border border-line bg-transparent px-2 py-1 text-[12px] text-text" />
        </label>
        <label className="flex min-w-[200px] flex-1 flex-col gap-1 text-[11px] text-text-3">
          API key
          <input
            type="password"
            value={apiKey}
            onChange={(e) => setApiKey(e.target.value)}
            autoComplete="off"
            spellCheck={false}
            className="rounded border border-line bg-transparent px-2 py-1 text-[12px] text-text"
          />
        </label>
        <button
          type="submit"
          disabled={pending || !config || !label.trim() || !apiKey.trim()}
          className="rounded-lg border border-line px-3 py-1.5 text-[12px] text-text hover:bg-panel-2 disabled:opacity-50"
        >
          Add API key
        </button>
      </form>
      {error ? (
        <p role="alert" className="mt-2 text-[11px] text-danger">
          {error}
        </p>
      ) : null}
    </section>
  );
}
