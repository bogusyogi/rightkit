import { Button } from "@rightkit/react-shell/react";
import Icon from "../components/Icons";
import { CATEGORY_TABS } from "../lib/mail";

const CAT_ICON: Record<string, string> = {
  PRIMARY: "inbox",
  CATEGORY_PROMOTIONS: "tag",
  CATEGORY_SOCIAL: "people",
  CATEGORY_UPDATES: "info",
};

export function InboxCategoryTabs({
  category,
  labelById,
  setCategory,
  setOpenThread,
}: {
  category: string | null;
  labelById: Map<string, { unread?: number }>;
  setCategory: (category: string) => void;
  setOpenThread: (threadId: null) => void;
}) {
  return (
    <div className="cat-tabs sticky top-10 z-[5] mb-0 flex min-h-[50px] items-end gap-1 bg-mr-panel-2 px-2 max-[1100px]:min-h-[58px]">
      {CATEGORY_TABS.map((tab) => {
        const unread = labelById.get(tab.key)?.unread ?? 0;
        return (
          <Button variant="ghost"
            key={tab.key}
            className={[
              "cat-tab flex flex-1 basis-0 cursor-pointer items-center justify-start gap-4 rounded-t-[10px] border-0 font-mr-body text-[15px] font-semibold transition-colors duration-100 focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-mr-copper max-[1100px]:justify-center max-[1100px]:gap-2 max-[1100px]:text-sm max-[1100px]:[&_svg]:h-4 max-[1100px]:[&_svg]:w-4 max-[760px]:gap-1.5 max-[760px]:text-[12.5px]",
              category === tab.key
                ? "active relative z-[1] -mb-px bg-mr-panel-2 text-mr-paper"
                : "cat-tab-idle bg-[color-mix(in_srgb,var(--panel-2)_40%,var(--ink)_60%)] text-mr-muted-2 hover:bg-[color-mix(in_srgb,var(--panel-2)_72%,var(--ink)_28%)] hover:text-mr-paper",
            ]
              .filter(Boolean)
              .join(" ")}
            aria-current={category === tab.key ? "page" : undefined}
            aria-label={`${tab.name} category${unread > 0 ? `, ${unread} unread` : ""}`}
            onClick={() => {
              setCategory(tab.key);
              setOpenThread(null);
            }}
          >
            <Icon name={CAT_ICON[tab.key]} size={18} />
            <span>{tab.name}</span>
            {unread > 0 && (
              <span className="cat-unread min-w-5 rounded-mr-pill bg-[color-mix(in_srgb,var(--copper)_24%,transparent)] px-[7px] py-0.5 text-center font-mr-mono text-xs tabular-nums text-mr-copper-hi max-[760px]:min-w-[17px] max-[760px]:px-[5px] max-[760px]:text-[11px]">
                {unread}
              </span>
            )}
          </Button>
        );
      })}
    </div>
  );
}
