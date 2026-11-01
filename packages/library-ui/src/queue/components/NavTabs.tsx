import { cx } from "@rightkit/react-shell/react";

export type Tab = "queue" | "library" | "settings";

// Settings is reached via a gear icon in the title bar, not a text tab.
const TABS: Tab[] = ["queue", "library"];

type NavTabsProps = {
  activeTab: Tab;
  onChange: (tab: Tab) => void;
};

export function NavTabs({ activeTab, onChange }: NavTabsProps) {
  return (
    <div className="flex items-center gap-2" role="tablist" aria-label="Main views">
      {TABS.map((tab) => (
        <button
          type="button"
          key={tab}
          role="tab"
          aria-selected={activeTab === tab}
          className={cx(
            "h-7 rounded-[6px] border px-3 text-xs text-[var(--muted)] transition-colors hover:border-[var(--line)] hover:bg-[var(--panel-2)] hover:text-[var(--paper)] focus:outline-none focus:ring-2 focus:ring-[var(--accent)]/35",
            activeTab === tab
              ? "border-[var(--line)] bg-[var(--panel-2)] text-[var(--paper)] shadow-[inset_0_0_0_1px_color-mix(in_srgb,var(--line)_55%,transparent)]"
              : "border-transparent bg-transparent",
          )}
          onClick={() => onChange(tab)}
        >
          {tab === "queue" ? "Workspace" : "Saved files"}
        </button>
      ))}
    </div>
  );
}
