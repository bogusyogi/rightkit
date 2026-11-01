import { useRef } from "react";
import { Button, ContextMenu, IconButton, Popover, type MenuItem, Input } from "@rightkit/react-shell/react";
import Icon from "../components/Icons.js";
import { computeListSource, userLabelsOf } from "../lib/mail.js";
import { getReaderActions, isDraftThread, type ThreadAction } from "../lib/folderActions.js";
import { useThreadView } from "../hooks/useThreadView.js";
import { useAppStore } from "../store/index.js";
import { deleteDrafts, bulkAction } from "../lib/ipc.js";
import { clickTrace } from "../lib/telemetry.js";

export function ReaderToolbar() {
  const labelWrapRef = useRef<HTMLDivElement>(null);
  const moveWrapRef = useRef<HTMLDivElement>(null);
  const navTo = useAppStore((s) => s.navTo);
  const actOnOpen = useAppStore((s) => s.actOnOpen);
  const threads = useAppStore((s) => s.threads);
  const results = useAppStore((s) => s.results);
  const query = useAppStore((s) => s.query);
  const unreadOnly = useAppStore((s) => s.unreadOnly);
  const sortMode = useAppStore((s) => s.sortMode);
  const labels = useAppStore((s) => s.labels);
  const openLabels = useAppStore((s) => s.openLabels);
  const openThread = useAppStore((s) => s.openThread);
  const labelPickerOpen = useAppStore((s) => s.labelPickerOpen);
  const setLabelPickerOpen = useAppStore((s) => s.setLabelPickerOpen);
  const movePickerOpen = useAppStore((s) => s.movePickerOpen);
  const setMovePickerOpen = useAppStore((s) => s.setMovePickerOpen);
  const moveFilter = useAppStore((s) => s.moveFilter);
  const setMoveFilter = useAppStore((s) => s.setMoveFilter);
  const showImages = useAppStore((s) => s.showImages);
  const setShowImages = useAppStore((s) => s.setShowImages);
  const setOpenThread = useAppStore((s) => s.setOpenThread);
  const folder = useAppStore((s) => s.folder);
  const run = useAppStore((s) => s.run);
  const { muteOpen, unmuteOpen, addThreadToTask, toggleThreadLabel, moveToLabel, toggleImportant, unsnoozeOpen } =
    useThreadView();

  // Gmail 1:1: each folder renders a different toolbar set. Drafts → only
  // Discard + Important. Sent → Delete + Important. Trash → Restore +
  // Important. Snoozed → Unsnooze + Important. All Mail → Trash + Move to
  // Inbox + Important. Inbox/Starred → full set.
  const actions = getReaderActions(folder);

  function clickAction(a: ThreadAction) {
    if (!openThread) return;
    switch (a) {
      case "delete_drafts":
        // Defensive: only let delete_drafts fire if the open thread
        // actually carries DRAFT. If a stale UI shows the button on an
        // inbox thread (shouldn't happen — folder gating does this), we
        // refuse rather than call the wrong API.
        if (!isDraftThread(openLabels)) return;
        run("delete_drafts", () =>
          deleteDrafts([
            { account_id: openThread!.account_id, thread_id: openThread!.thread_id },
          ]),
        );
        return;
      case "trash":
        actOnOpen("trash");
        return;
      case "untrash":
        run("bulk_action", () =>
          bulkAction(
            [{ account_id: openThread!.account_id, thread_id: openThread!.thread_id }],
            "untrash",
          ),
        );
        return;
      case "unspam":
        run("bulk_action", () =>
          bulkAction(
            [{ account_id: openThread!.account_id, thread_id: openThread!.thread_id }],
            "unspam",
          ),
        );
        return;
      case "unsnooze":
        unsnoozeOpen();
        return;
      case "unarchive":
        run("bulk_action", () =>
          bulkAction(
            [{ account_id: openThread!.account_id, thread_id: openThread!.thread_id }],
            "unarchive",
          ),
        );
        return;
      case "archive":
        actOnOpen("archive");
        return;
      case "spam":
        actOnOpen("spam");
        return;
      case "mark_unread":
        actOnOpen("unread");
        return;
      case "mute":
        muteOpen();
        return;
      case "important":
        toggleImportant();
        return;
      default:
        // Other actions (snooze, mark_read, etc.) keep their existing
        // dedicated handler paths outside the toolbar.
        break;
    }
  }

  const userLabels = userLabelsOf(labels);
  const listSource = computeListSource(threads, results, query, unreadOnly, sortMode);
  const openIndex = openThread
    ? listSource.findIndex(
        (t) => t.account_id === openThread.account_id && t.thread_id === openThread.thread_id
      )
    : -1;

  return (
    <div className="reader-bar">
      <div className="reader-actions">
        <IconButton label="Back to list" onClick={() => setOpenThread(null)}>
          <Icon name="back" size={18} />
        </IconButton>
        <span className="ibtn-sep" />
        {actions.map((a) => {
          // Per-folder rendering. See lib/folderActions.ts for the 1:1 Gmail
          // mapping. Drafts get only Discard + Important; Sent gets Trash +
          // Important; Trash gets Restore + Important; etc.
          const traceClick = () => {
            clickTrace(`reader.${a}`, { surface: "reader", folder, slot: a });
            clickAction(a);
          };
          switch (a) {
            case "delete_drafts":
              return (
                <IconButton className="text-mr-danger"
                  key={a}
                  label="Discard draft (permanently delete)"
                  onClick={traceClick}
                >
                  <Icon name="trash" size={17} />
                </IconButton>
              );
            case "trash":
              return (
                <IconButton
                  key={a}
                  label="Delete (move to Trash)"
                  onClick={traceClick}
                >
                  <Icon name="trash" size={17} />
                </IconButton>
              );
            case "untrash":
              return (
                <IconButton
                  key={a}
                  label="Restore to Inbox"
                  onClick={traceClick}
                >
                  <Icon name="inbox" size={17} />
                </IconButton>
              );
            case "unspam":
              return (
                <IconButton
                  key={a}
                  label="Not spam (move to Inbox)"
                  onClick={traceClick}
                >
                  <Icon name="inbox" size={17} />
                </IconButton>
              );
            case "unsnooze":
              return (
                <IconButton
                  key={a}
                  label="Move to Inbox (unsnooze)"
                  onClick={traceClick}
                >
                  <Icon name="inbox" size={17} />
                </IconButton>
              );
            case "unarchive":
              return (
                <IconButton
                  key={a}
                  label="Move to Inbox"
                  onClick={traceClick}
                >
                  <Icon name="inbox" size={17} />
                </IconButton>
              );
            case "archive":
              return (
                <IconButton
                  key={a}
                  label="Archive"
                  onClick={traceClick}
                >
                  <Icon name="archive" size={17} />
                </IconButton>
              );
            case "spam":
              return (
                <IconButton
                  key={a}
                  label="Report spam"
                  onClick={traceClick}
                >
                  <Icon name="spam" size={17} />
                </IconButton>
              );
            case "mark_unread":
              return (
                <IconButton
                  key={a}
                  label="Mark unread"
                  onClick={traceClick}
                >
                  <Icon name="mailClosed" size={17} />
                </IconButton>
              );
            case "mute":
              return (
                <IconButton pressed={openLabels.includes("MUTED")}
                  key={a}
                  label={openLabels.includes("MUTED") ? "Muted (click to unmute)" : "Mute conversation"}
                  onClick={traceClick}
                >
                  <Icon name="mailClosed" size={17} />
                  <span className="ibtn-label">Mute</span>
                </IconButton>
              );
            case "important":
              return (
                <IconButton pressed={openLabels.includes("IMPORTANT")}
                  key={a}
                  label={openLabels.includes("IMPORTANT") ? "Marked important (click to toggle off)" : "Mark important"}
                  onClick={traceClick}
                >
                  <Icon name="tag" size={17} />
                </IconButton>
              );
            default:
              return null;
          }
        })}
        <IconButton pressed={showImages}
          label={showImages ? "Hide remote images" : "Display remote images"}
          onClick={() => setShowImages((v) => !v)}
        >
          <Icon name="mailOpen" size={17} />
          <span className="ibtn-label">Images</span>
        </IconButton>
        {openLabels.includes("MUTED") ? (
          <IconButton pressed label="Unmute conversation" onClick={unmuteOpen}>
            <Icon name="mailClosed" size={17} />
            <span className="ibtn-label">Muted</span>
          </IconButton>
        ) : (
          <IconButton label="Mute conversation" onClick={muteOpen}>
            <Icon name="mailClosed" size={17} />
            <span className="ibtn-label">Mute</span>
          </IconButton>
        )}
        <IconButton
          label="Add this conversation to Google Tasks"
          onClick={addThreadToTask}
        >
          <Icon name="check" size={17} />
          <span className="ibtn-label">Task</span>
        </IconButton>
        {userLabels.length > 0 && (
          <div className="more-wrap" ref={labelWrapRef}>
            <IconButton
              pressed={labelPickerOpen}
              label="Label"
              onClick={() => setLabelPickerOpen((o) => !o)}
            >
              <Icon name="tag" size={17} />
            </IconButton>
            {labelPickerOpen && labelWrapRef.current && (
              <ContextMenu
                anchor={labelWrapRef.current}
                items={userLabels.map<MenuItem>((l) => ({
                  id: l.id,
                  label: l.name,
                  checked: openLabels.includes(l.id),
                  run: () => toggleThreadLabel(l.id),
                }))}
                onClose={() => setLabelPickerOpen(false)}
                label="Label as"
              />
            )}
          </div>
        )}
        {userLabels.length > 0 && (
          <div className="more-wrap" ref={moveWrapRef}>
            <IconButton
              pressed={movePickerOpen}
              label="Move to"
              onClick={() => setMovePickerOpen((o) => !o)}
            >
              <Icon name="folder" size={17} />
              <span className="ibtn-label">Move</span>
            </IconButton>
            {movePickerOpen && moveWrapRef.current && (
              <Popover anchor={moveWrapRef.current} onClose={() => setMovePickerOpen(false)} label="Move to">
                <div className="px-2 py-1 text-xs font-semibold text-mr-muted">Move to</div>
                <Input
                  className="move-filter"
                  placeholder="Filter labels..."
                  value={moveFilter}
                  autoFocus
                  onChange={(e) => setMoveFilter(e.target.value)}
                />
                {userLabels
                  .filter((l) => l.name.toLowerCase().includes(moveFilter.toLowerCase()))
                  .map((l) => (
                    <Button variant="ghost" size="sm" key={l.id} onClick={() => moveToLabel(l.id)}>
                      {l.name}
                    </Button>
                  ))}
              </Popover>
            )}
          </div>
        )}
      </div>
      <div className="reader-nav">
        {openIndex >= 0 && (
          <span className="rnav-count">
            {openIndex + 1} of {listSource.length}
          </span>
        )}
        <IconButton label="Previous" disabled={openIndex <= 0} onClick={() => navTo(-1)}>
          ‹
        </IconButton>
        <IconButton
          label="Next"
          disabled={openIndex < 0 || openIndex >= listSource.length - 1}
          onClick={() => navTo(1)}
        >
          ›
        </IconButton>
      </div>
    </div>
  );
}
