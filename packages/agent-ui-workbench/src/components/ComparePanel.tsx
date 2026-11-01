// Blind model comparison panel. Run a prompt across 2+ models, judge the outputs
// blind (identities hidden), then reveal the winner. The pick is logged to the
// audit trail as model-selection evidence.

import { useState } from "react";
import type { DaemonConfig, ModelCatalogEntry } from "@rightkit/agent-ui-core/lib/types";
import { type BlindEntry, type JudgeResult, judgeCompare, runCompare } from "@rightkit/agent-ui-core/lib/daemon";

export default function ComparePanel({
  config,
  models,
}: {
  config: DaemonConfig | null;
  models: ModelCatalogEntry[];
}) {
  const [prompt, setPrompt] = useState("");
  const [selected, setSelected] = useState<string[]>([]);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<{ id: string; entries: BlindEntry[] } | null>(null);
  const [reveal, setReveal] = useState<JudgeResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [picking, setPicking] = useState<string | null>(null);

  const enabledModels = models.filter((m) => m.enabled);

  const modelForLabel = (label: string) =>
    reveal?.mapping.find((m) => m.label === label)?.model ?? null;

  function toggle(id: string) {
    setSelected((s) => (s.includes(id) ? s.filter((m) => m !== id) : [...s, id]));
  }

  async function run() {
    if (!config || !prompt.trim() || selected.length < 2) return;
    setRunning(true);
    setError(null);
    setResult(null);
    setReveal(null);
    try {
      const r = await runCompare(config, prompt.trim(), selected);
      setResult({ id: r.id, entries: r.entries });
    } catch (e) {
      setError(e instanceof Error ? e.message : "compare failed");
    } finally {
      setRunning(false);
    }
  }

  async function pick(label: string) {
    if (!config || !result || picking) return;
    setPicking(label);
    setError(null);
    try {
      setReveal(await judgeCompare(config, result.id, label));
    } catch (e) {
      setError(e instanceof Error ? e.message : "judge failed");
    } finally {
      setPicking(null);
    }
  }

  const slug = (id: string) => {
    const m = models.find((x) => x.id === id);
    return m?.displayName || m?.modelSlug || id;
  };

  return (
    <div className="flex-1 min-h-0 overflow-y-auto p-5 flex flex-col gap-4">
      <div className="flex flex-col gap-2">
        <p className="text-[11px] uppercase tracking-widest text-accent font-semibold">
          Blind model comparison
        </p>
        <p className="text-[13px] text-text-2 leading-relaxed">
          Run the same prompt across 2+ models, judge the answers without seeing which is which,
          then reveal. The winner is logged as model-selection evidence.
        </p>
      </div>

      <textarea
        aria-label="Prompt to compare across models"
        className="min-h-[80px] resize-y rounded-xl border border-line bg-panel p-3 text-[13px] text-text outline-none focus:border-focus placeholder:text-text-3"
        placeholder="Prompt to compare across models…"
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
      />

      <div className="flex flex-col gap-2">
        <span className="text-[11px] uppercase tracking-wider text-text-3">
          Models ({selected.length} selected)
        </span>
        {enabledModels.length === 0 ? (
          <div className="rounded-xl border border-line bg-panel/60 p-4 text-[12px] text-text-3">
            No models enabled. Enable models in{" "}
            <span className="text-text-2">Settings → Models</span> to run a comparison.
          </div>
        ) : (
          <div className="flex flex-wrap gap-2">
            {enabledModels.map((m) => (
              <button
                type="button"
                key={m.id}
                onClick={() => toggle(m.id)}
                className={`h-7 px-3 rounded-full border text-[12px] cursor-pointer transition-colors ${
                  selected.includes(m.id)
                    ? "border-accent-2/50 bg-accent-2/10 text-accent-2"
                    : "border-line bg-transparent text-text-3 hover:text-text-2 hover:border-line-2"
                }`}
              >
                {m.displayName || m.modelSlug}
              </button>
            ))}
          </div>
        )}
      </div>

      <button
        type="button"
        disabled={running || !prompt.trim() || selected.length < 2 || !config}
        onClick={run}
        className="self-start h-9 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90 disabled:opacity-40"
      >
        {running ? "Running…" : `Run comparison (${selected.length})`}
      </button>

      {error ? <p className="text-[12px] text-danger">{error}</p> : null}

      {result ? (
        <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
          {result.entries.map((e) => {
            const isWinner = reveal?.winnerLabel === e.label;
            const revealedModel = modelForLabel(e.label);
            return (
              <div
                key={e.label}
                className={`rounded-xl border p-4 flex flex-col gap-3 ${
                  isWinner ? "border-accent-2/60 bg-accent-2/5" : "border-line bg-panel"
                }`}
              >
                <div className="flex items-center gap-2">
                  <span className="w-6 h-6 grid place-items-center rounded-md bg-panel-2 text-[12px] font-bold text-text">
                    {e.label}
                  </span>
                  {revealedModel ? (
                    <span className="text-[11px] font-mono text-text-3">
                      {isWinner ? "🏆 " : ""}
                      {slug(revealedModel)}
                    </span>
                  ) : null}
                  <span className="flex-1" />
                  {!reveal ? (
                    <button
                      type="button"
                      onClick={() => pick(e.label)}
                      disabled={picking !== null}
                      className="h-6 px-3 rounded-md border border-accent/50 bg-accent/10 text-accent text-[11px] font-semibold cursor-pointer hover:bg-accent/20 disabled:opacity-50"
                    >
                      {picking === e.label ? "Judging…" : `Pick ${e.label}`}
                    </button>
                  ) : null}
                </div>
                <pre className="text-[12px] text-text-2 leading-relaxed whitespace-pre-wrap font-sans max-h-[260px] overflow-y-auto">
                  {e.output}
                </pre>
              </div>
            );
          })}
        </div>
      ) : null}

      {reveal ? (
        <div className="rounded-xl border border-accent-2/40 bg-accent-2/5 p-4 text-[13px] text-text-2">
          Winner: <strong className="text-accent-2">{slug(reveal.winnerModel)}</strong> over{" "}
          {reveal.field.map(slug).join(", ")}. Logged to the audit trail.
        </div>
      ) : null}
    </div>
  );
}
