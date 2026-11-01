import { groupPickerAccounts, type Account } from "@rightkit/agent-ui-core/lib/accounts/accounts";

export interface AccountModelPickerProps {
  accounts: Account[];
  models: { id: string; label: string }[];
  /** Account chosen for this chat. */
  selectedAccountId: string | null;
  selectedModelId: string | null;
  /** Project default account, if any. */
  projectDefaultAccountId?: string | null;
  onSelect: (accountId: string, modelId: string) => void;
  onSetProjectDefault?: (accountId: string) => void;
}

/** provider > account > models. The account is a per-chat choice; nothing switches automatically. */
export default function AccountModelPicker({
  accounts,
  models,
  selectedAccountId,
  selectedModelId,
  projectDefaultAccountId = null,
  onSelect,
  onSetProjectDefault,
}: AccountModelPickerProps) {
  const groups = groupPickerAccounts(accounts, models);
  if (groups.length === 0) {
    return <p className="text-[12px] text-text-3">No accounts connected. Add one in Settings.</p>;
  }
  return (
    <div className="flex flex-col gap-3" data-testid="account-model-picker">
      {groups.map((provider) => (
        <section key={provider.provider} aria-label={provider.providerLabel}>
          <h3 className="px-1 pb-1 text-[10px] font-semibold uppercase tracking-wide text-text-3">
            {provider.providerLabel}
          </h3>
          {provider.accounts.map(({ account, heading, models: accountModels }) => {
            const isDefault = projectDefaultAccountId === account.id;
            return (
              <fieldset
                key={account.id}
                aria-label={`Account ${heading}`}
                className="m-0 mb-2 rounded-lg border border-line p-1"
              >
                <div className="flex items-center justify-between gap-2 px-2 py-1">
                  <strong className="truncate text-[12px] text-text">{heading}</strong>
                  {onSetProjectDefault ? (
                    <button
                      type="button"
                      aria-pressed={isDefault}
                      onClick={() => onSetProjectDefault(account.id)}
                      className="text-[11px] text-accent hover:underline focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60"
                    >
                      {isDefault ? "Project default" : "Set as project default"}
                    </button>
                  ) : null}
                </div>
                <ul className="flex flex-col gap-1">
                  {accountModels.map((model) => {
                    const active = account.id === selectedAccountId && model.id === selectedModelId;
                    return (
                      <li key={model.id}>
                        <button
                          type="button"
                          aria-label={`${model.label} on ${account.label}`}
                          aria-pressed={active}
                          onClick={() => onSelect(account.id, model.id)}
                          className={`flex w-full items-center gap-2 rounded-md border px-2.5 py-1.5 text-left text-[12px] focus:outline-none focus-visible:ring-2 focus-visible:ring-accent/60 ${active ? "border-accent bg-panel-2" : "border-transparent hover:border-line hover:bg-panel-2"}`}
                        >
                          <span className="min-w-0 flex-1 truncate text-text">{model.label}</span>
                          {active ? (
                            <span aria-hidden="true" className="text-accent">
                              ✓
                            </span>
                          ) : null}
                        </button>
                      </li>
                    );
                  })}
                </ul>
              </fieldset>
            );
          })}
        </section>
      ))}
    </div>
  );
}
