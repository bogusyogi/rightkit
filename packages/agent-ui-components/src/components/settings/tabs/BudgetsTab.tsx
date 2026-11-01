import type { BudgetConfig } from "@rightkit/agent-ui-core/lib/types";
import type { SettingsDraft } from "../types";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";

export default function BudgetsTab({
  settingsDraft,
  onSettingsDraft,
}: {
  settingsDraft: SettingsDraft | null;
  onSettingsDraft: (draft: SettingsDraft) => void;
}) {
  function patchBudget<K extends keyof BudgetConfig>(key: K, value: number) {
    if (!settingsDraft) return;
    onSettingsDraft({
      ...settingsDraft,
      budget: { ...settingsDraft.budget, [key]: value },
    });
  }

  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex flex-col gap-1">
        <h3 className="text-[16px] font-bold text-text">Budgets</h3>
        <p className="text-[13px] text-text-2">
          Budget controls should be plain, visible, and hard to misread.
        </p>
      </div>
      {settingsDraft ? (
        <div className="flex flex-col max-w-[520px]">
          <label className="grid grid-cols-[minmax(0,1fr)_240px] items-center gap-3 px-4 h-9 border-b border-line/60">
            <span className="text-[13px] text-text-2">Per task $</span>
            <input
              aria-label="Per task $"
              className="h-8 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              type="number"
              step="0.01"
              value={
                Number.isFinite(settingsDraft.budget.perTaskMaxUsd)
                  ? settingsDraft.budget.perTaskMaxUsd
                  : 0
              }
              onChange={(e) => patchBudget("perTaskMaxUsd", Number(e.target.value))}
            />
          </label>
          <label className="grid grid-cols-[minmax(0,1fr)_240px] items-center gap-3 px-4 h-9 border-b border-line/60">
            <span className="text-[13px] text-text-2">Per session $</span>
            <input
              aria-label="Per session $"
              className="h-8 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              type="number"
              step="0.01"
              value={
                Number.isFinite(settingsDraft.budget.perSessionMaxUsd)
                  ? settingsDraft.budget.perSessionMaxUsd
                  : 0
              }
              onChange={(e) => patchBudget("perSessionMaxUsd", Number(e.target.value))}
            />
          </label>
          <label className="grid grid-cols-[minmax(0,1fr)_240px] items-center gap-3 px-4 h-9 border-b border-line/60">
            <span className="text-[13px] text-text-2">Per call $</span>
            <input
              aria-label="Per call $"
              className="h-8 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              type="number"
              step="0.01"
              value={
                Number.isFinite(settingsDraft.budget.perAgentCallMaxUsd)
                  ? settingsDraft.budget.perAgentCallMaxUsd
                  : 0
              }
              onChange={(e) => patchBudget("perAgentCallMaxUsd", Number(e.target.value))}
            />
          </label>
        </div>
      ) : (
        <OfflinePanel
          title="Budget settings will load after a workspace is connected."
          action="Open a workspace, then return here."
        />
      )}
    </section>
  );
}
