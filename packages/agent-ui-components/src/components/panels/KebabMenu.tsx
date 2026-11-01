import { ContextMenu, type MenuItem } from "@rightkit/react-shell/react";
import { type MouseEvent, useMemo, useRef, useState } from "react";
import Icon, { type IconName } from "@rightkit/agent-ui-core/components/Icon";

// KebabMenu — the kebab (⋮) popover anchored to the top-right of the chat
// column. Mirrors the Claude Code side-panel menu: Artifacts · Files
// (highlighted when active) · Background tasks · Open in Terminal · Rename ·
// Fork · Archive · Delete (red). Closes on click
// outside, Escape, or selecting an item.
//
// The trigger button is rendered separately (`KebabTrigger`) so the menu
// can be opened from another control (e.g. the titlebar's existing kebab
// icon) without forcing this file to know about that wiring.

export type KebabAction =
  | "open-artifacts"
  | "open-files"
  | "open-background-tasks"
  | "rename"
  | "fork"
  | "archive"
  | "delete"
  | "open-in-terminal"
  | "open-review"
  | "open-browser"
  | "open-delivery"
  | "open-environments"
  | "open-attention"
  | "open-diagnostics"
  | "open-settings";

export interface KebabMenuProps {
  activeTab:
    | "background-tasks"
    | "files"
    | "artifacts"
    | "review"
    | "execution"
    | "bots"
    | "browser"
    | "delivery"
    | "environments"
    | "attention"
    | "diagnostics"
    | null;
  onSelect: (action: KebabAction) => void;
  /** Anchor the menu to a button (click-to-toggle) */
  triggerLabel?: string;
  className?: string;
}

interface KebabItem {
  key: KebabAction;
  label: string;
  group: "workspace" | "tools" | "session" | "danger";
  shortcut?: string;
  icon?: IconName;
  danger?: boolean;
}

// No shortcut labels: none of these actions have a global key binding
// (bindings live in AppBody's shortcut table) — advertising phantom chords
// like "Ctrl+F" is a false affordance.
const MENU_ITEMS: KebabItem[] = [
  { key: "open-artifacts", label: "Artifacts", group: "workspace", icon: "file" },
  {
    key: "open-files",
    label: "Files",
    group: "workspace",
    icon: "folder",
  },
  { key: "open-review", label: "Review", group: "workspace", icon: "diff" },
  { key: "open-browser", label: "Browser", group: "workspace", icon: "panel-right" },
  { key: "open-delivery", label: "Delivery", group: "workspace", icon: "git" },
  { key: "open-environments", label: "Environments", group: "workspace", icon: "server" },
  { key: "open-attention", label: "Attention", group: "workspace", icon: "alert" },
  {
    key: "open-diagnostics",
    label: "Diagnostics",
    group: "workspace",
    icon: "alert",
  },
  {
    key: "open-background-tasks",
    label: "Agents",
    group: "workspace",
    icon: "activity",
  },
  {
    key: "open-in-terminal",
    label: "Open in Terminal",
    group: "tools",
    icon: "terminal",
  },
  { key: "open-settings", label: "Settings", group: "tools", icon: "settings" },
  { key: "rename", label: "Rename", group: "session", icon: "edit" },
  { key: "fork", label: "Fork", group: "session", icon: "branch" },
  { key: "archive", label: "Archive", group: "session", icon: "download" },
  {
    key: "delete",
    label: "Delete",
    group: "danger",
    icon: "trash",
    danger: true,
  },
];

const MENU_GROUPS = ["workspace", "tools", "session", "danger"] as const;

export default function KebabMenu({
  activeTab,
  onSelect,
  triggerLabel = "Chat actions",
  className,
}: KebabMenuProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement | null>(null);

  // The anchored menu (focus, arrows, Home/End, Escape, outside press, placement) is RightKit's
  // `ContextMenu` (@rightkit/react-shell) opened from the trigger button.
  const items = useMemo<MenuItem[]>(
    () =>
      MENU_ITEMS.map((item) => ({
        id: item.key,
        label: item.label,
        danger: item.danger,
        separatorBefore:
          MENU_GROUPS.indexOf(item.group) > 0 &&
          MENU_ITEMS.find((other) => other.group === item.group) === item,
        run: () => onSelect(item.key),
      })),
    [onSelect],
  );

  return (
    <div className={`relative inline-block ${className ?? ""}`}>
      <button
        ref={triggerRef}
        type="button"
        aria-label={triggerLabel}
        aria-haspopup="menu"
        aria-expanded={open}
        title="Chat actions"
        data-testid="kebab-trigger"
        onClick={(event: MouseEvent<HTMLButtonElement>) => {
          event.stopPropagation();
          setOpen((current) => !current);
        }}
        className={`cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent bg-transparent text-text-3 cursor-pointer hover:bg-panel-2 hover:text-text ${
          open
            ? "border-accent/40 bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] text-accent"
            : ""
        }`}
      >
        <Icon name="more" size={14} />
      </button>

      {open && triggerRef.current ? (
        <ContextMenu
          anchor={triggerRef.current}
          placement="bottom-end"
          label={triggerLabel}
          items={items}
          itemAs="button"
          width={220}
          onClose={() => setOpen(false)}
          renderItem={(item) => {
            const meta = MENU_ITEMS.find((entry) => entry.key === item.id);
            const active = activeTab !== null && item.id === `open-${activeTab}`;
            return (
              <span
                data-testid={`kebab-item-${item.id}`}
                aria-current={active ? "true" : undefined}
                className={`flex w-full items-center gap-2 ${active ? "text-accent" : ""}`}
              >
                {meta?.icon ? <Icon name={meta.icon} size={13} /> : null}
                <span className="flex-1">{item.label}</span>
              </span>
            );
          }}
        />
      ) : null}
    </div>
  );
}
