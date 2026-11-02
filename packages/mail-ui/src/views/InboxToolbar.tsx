import { Button, IconButton, Select, Checkbox } from "@rightkit/react-shell/react";
import Icon from "../components/Icons.js";
import { setPref } from "../lib/preferences.js";
import { syncAll, deleteDrafts, unsnoozeThread } from "../lib/ipc.js";
import {
  type ThreadAction,
  getListToolbarActions,
  invokeAction,
} from "../lib/folderActions.js";
import type { Folder } from "../lib/mail.js";
import type { BulkAction } from "../store/types.js";
import { clickTrace } from "../lib/telemetry.js";

type SelectedRef = { account_id: string; thread_id: string };

/** One toolbar button. Pure render — receives the dispatch closures from
 *  the parent so we don't double-thread `run` / `runBulk` here. */
function ToolbarButton({
  title,
  icon,
  onClick,
  danger,
}: {
  title: string;
  icon: string;
  onClick: () => void;
  danger?: boolean;
}) {
  return (
    <IconButton
      className={danger ? "text-mr-danger" : undefined}
      label={title}
      onClick={onClick}
    >
      <Icon name={icon} size={17} />
    </IconButton>
  );
}

export function InboxToolbar({
  activeScopeTotal,
  allSelected,
  busy,
  deleteForeverFlow,
  destructiveOn,
  folder,
  listLength,
  loadOlderMessages,
  loadingOlder,
  noMoreOlder,
  pageEnd,
  run,
  runBulk,
  selectedRefs,
  selectedSize,
  setSelected,
  setSortMode,
  setUnreadOnly,
  someSelected,
  sortMode,
  toggleSelectAll,
  unreadOnly,
}: {
  activeScopeTotal: number;
  allSelected: boolean;
  busy: string | null;
  deleteForeverFlow: (refs: SelectedRef[]) => void;
  destructiveOn: boolean;
  folder: string;
  listLength: number;
  loadOlderMessages: () => void;
  loadingOlder: boolean;
  noMoreOlder: boolean;
  pageEnd: number;
  run: (key: string, fn: () => Promise<unknown>) => void;
  runBulk: (action: BulkAction, refs: SelectedRef[]) => void;
  selectedRefs: SelectedRef[];
  selectedSize: number;
  setSelected: (selected: Set<string>) => void;
  setSortMode: (mode: "newest" | "oldest") => void;
  setUnreadOnly: (updater: (value: boolean) => boolean) => void;
  someSelected: boolean;
  sortMode: "newest" | "oldest";
  toggleSelectAll: () => void;
  unreadOnly: boolean;
}) {
  // Gmail 1:1 mapping. The list of buttons + their order is derived from
  // the current folder (see lib/folderActions.ts). Drafts get only Discard;
  // Sent/Drafts/Trash/Spam/Snoozed/All Mail hide Archive + Report Spam (those
  // verbs silently no-op there because the threads don't carry INBOX).
  const f = folder as Folder;
  const actions = getListToolbarActions(f);
  const inTrashOrSpam = f === "trash" || f === "spam";

  function renderAction(a: ThreadAction): React.ReactNode {
    if (a === "delete_drafts") {
      return (
        <ToolbarButton
          key={a}
          title="Discard drafts (permanently delete)"
          icon="trash"
          danger
          onClick={() => {
            clickTrace(`toolbar.${a}`, {
              surface: "list",
              folder,
              slot: a,
              selected: selectedRefs.length,
            });
            run("delete_drafts", () => deleteDrafts(selectedRefs));
          }}
        />
      );
    }
    if (a === "untrash") {
      return (
        <ToolbarButton
          key={a}
          title="Restore to Inbox"
          icon="inbox"
          onClick={() => {
            clickTrace(`toolbar.${a}`, {
              surface: "list",
              folder,
              slot: a,
              selected: selectedRefs.length,
            });
            runBulk("untrash", selectedRefs);
          }}
        />
      );
    }
    if (a === "unspam") {
      return (
        <ToolbarButton
          key={a}
          title="Not spam (move to Inbox)"
          icon="inbox"
          onClick={() => {
            clickTrace(`toolbar.${a}`, {
              surface: "list",
              folder,
              slot: a,
              selected: selectedRefs.length,
            });
            runBulk("unspam", selectedRefs);
          }}
        />
      );
    }
    if (a === "unsnooze") {
      return (
        <ToolbarButton
          key={a}
          title="Move to Inbox (unsnooze)"
          icon="inbox"
          onClick={() => {
            clickTrace(`toolbar.${a}`, {
              surface: "list",
              folder,
              slot: a,
              selected: selectedRefs.length,
            });
            // Snoozed threads need the dedicated unsnooze IPC, not a label
            // flip — the snooze record lives in the `snoozes` table, not as
            // a Gmail label.
            for (const ref of selectedRefs) {
              run("unsnooze", () =>
                unsnoozeThread(ref.account_id, ref.thread_id),
              );
            }
          }}
        />
      );
    }
    if (a === "unarchive") {
      // All-Mail toolbar: "Move to Inbox" = unarchive
      return (
        <ToolbarButton
          key={a}
          title="Move to Inbox"
          icon="inbox"
          onClick={() => {
            clickTrace(`toolbar.${a}`, {
              surface: "list",
              folder,
              slot: a,
              selected: selectedRefs.length,
            });
            runBulk("unarchive", selectedRefs);
          }}
        />
      );
    }
    const route = invokeAction(a, selectedRefs);
    if (route.kind === "bulk") {
      const titleByAction: Record<string, string> = {
        archive: "Archive",
        spam: "Report spam",
        trash: "Delete (move to Trash)",
        read: "Mark read",
        unread: "Mark unread",
        star: "Star",
        unstar: "Remove star",
        mute: "Mute conversation",
      };
      return (
        <ToolbarButton
          key={a}
          title={titleByAction[a] ?? a}
          icon={
            ({ archive: "archive", spam: "spam", trash: "trash",
               read: "mailOpen", unread: "mailClosed",
               star: "star", unstar: "star",
               mute: "mailClosed" } as Record<string, string>)[a] ?? "trash"
          }
          onClick={() => {
            clickTrace(`toolbar.${a}`, {
              surface: "list",
              folder,
              slot: a,
              selected: selectedRefs.length,
            });
            runBulk(route.action as BulkAction, selectedRefs);
          }}
        />
      );
    }
    return null;
  }

  return (
    <div className="list-head sticky top-0 z-[6] flex min-h-10 items-center gap-[3px] border-b border-mr-line bg-mr-panel-2 px-3 py-1">
      <Checkbox
        className="mr-1.5"
        label={<span className="sr-only">{allSelected ? "Deselect all" : "Select all"}</span>}
        checked={allSelected}
        indeterminate={someSelected && !allSelected}
        onChange={() => {
          clickTrace("toolbar.selectAll", {
            surface: "list",
            folder,
            on: !allSelected,
          });
          toggleSelectAll();
        }}
      />
      <IconButton
        label="Refresh"
        disabled={!!busy}
        onClick={() => {
          clickTrace("toolbar.refresh", { surface: "list", folder });
          run("sync", syncAll);
        }}
      >
        <Icon name="refresh" size={17} />
      </IconButton>
      <span className="ibtn-sep" />
      {someSelected && (
        <>
          {actions.map(renderAction)}
          {/* Trash / Spam also get the disabled "Delete forever" affordance
              (matches Gmail's "more destructive options live in the trash" UX).
              The actual delete is gated by the safety flag and surfaces a
              "not built yet" message — see PERMANENT_DELETE_DISABLED. */}
          {inTrashOrSpam && (
            <IconButton className="text-mr-danger"
              label={
                destructiveOn
                  ? "Delete forever"
                  : "Delete forever - off by safety flag, will be refused"
              }
              onClick={() => {
                clickTrace("toolbar.deleteForever", {
                  surface: "list",
                  folder,
                  selected: selectedRefs.length,
                  on: destructiveOn,
                });
                deleteForeverFlow(selectedRefs);
              }}
            >
              <Icon name="trash" size={17} />
            </IconButton>
          )}
        </>
      )}
      {someSelected && (
        <>
          <span className="bulk-count">{selectedSize} selected</span>
          <Button variant="ghost" size="sm"
            onClick={() => {
              clickTrace("toolbar.clearSelection", { surface: "list", folder });
              setSelected(new Set());
            }}
          >
            Clear
          </Button>
        </>
      )}
      <div className="list-head-right ml-auto flex items-center gap-1.5">
        <Button
          variant="ghost"
          size="sm"
          className={unreadOnly ? "unread-toggle font-semibold text-mr-copper-hi" : "unread-toggle"}
          title="Show only unread"
          onClick={() => setUnreadOnly((v) => !v)}
        >
          {unreadOnly ? "Unread ✓" : "Unread"}
        </Button>
        <Select
          className="sort-select"
          value={sortMode}
          label="Sort"
          options={[
            { value: "newest", label: "Newest first" },
            { value: "oldest", label: "Oldest first" },
          ]}
          onChange={(v) => {
            setSortMode(v);
            setPref("sort", v);
          }}
        />
        <span className="page-count ml-2.5 min-w-[86px] whitespace-nowrap text-right font-mr-mono text-xs tabular-nums text-mr-muted-2">
          {listLength ? `1-${pageEnd}` : "0"} of {activeScopeTotal}
        </span>
        <IconButton label="Previous page" disabled>
          <Icon name="chevron-left" size={17} />
        </IconButton>
        <IconButton
          label="Load older"
          disabled={loadingOlder || noMoreOlder}
          onClick={loadOlderMessages}
        >
          <Icon name="chevron-right" size={17} />
        </IconButton>
      </div>
    </div>
  );
}