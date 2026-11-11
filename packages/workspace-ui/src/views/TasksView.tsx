// Tasks view (Google Tasks). Extracted from App.tsx (M5) — a prop-less,
// memoized component that reads its state from the host state and owns its handlers
// + load-on-mount, so it re-renders only when its own slices change.
import { memo, useEffect } from "react";
import { Icon } from "../icons.js";
import { Button, IconButton, SkeletonRows, Input, Select, Checkbox } from "@rightkit/react-shell/react";
import { useWorkspaceClient, useWorkspaceHost, useWorkspaceState } from "../host.js";
import type { TaskItem, TaskListInfo } from "../client.js";

function TasksViewInner() {
  const { listTasks, listTaskLists, createTaskList, renameTaskList, deleteTaskList, clearCompletedTasks, setTaskStatus, createTaskIn, deleteTask } = useWorkspaceClient();
  const showDialog = useWorkspaceHost().dialog;
  const activeAccount = useWorkspaceState((s) => s.activeAccount);
  const setErr = useWorkspaceHost().reportError;
  const showUndo = useWorkspaceHost().notify;
  const gtasks = useWorkspaceState((s) => s.gtasks);
  const setGtasks = useWorkspaceState((s) => s.setGtasks);
  const taskLists = useWorkspaceState((s) => s.taskLists);
  const setTaskLists = useWorkspaceState((s) => s.setTaskLists);
  const taskListId = useWorkspaceState((s) => s.taskListId);
  const setTaskListId = useWorkspaceState((s) => s.setTaskListId);
  const taskDue = useWorkspaceState((s) => s.taskDue);
  const setTaskDue = useWorkspaceState((s) => s.setTaskDue);
  const showCompletedTasks = useWorkspaceState((s) => s.showCompletedTasks);
  const setShowCompletedTasks = useWorkspaceState((s) => s.setShowCompletedTasks);
  const listMgrOpen = useWorkspaceState((s) => s.listMgrOpen);
  const setListMgrOpen = useWorkspaceState((s) => s.setListMgrOpen);
  const taskFormOpen = useWorkspaceState((s) => s.taskFormOpen);
  const setTaskFormOpen = useWorkspaceState((s) => s.setTaskFormOpen);
  const taskCreate = useWorkspaceState((s) => s.taskCreate);
  const setTaskCreate = useWorkspaceState((s) => s.setTaskCreate);
  const tasksBusy = useWorkspaceState((s) => s.tasksBusy);
  const setTasksBusy = useWorkspaceState((s) => s.setTasksBusy);

  async function reloadTasks(showCompleted = showCompletedTasks) {
    if (!activeAccount) return;
    try {
      setGtasks(await listTasks(activeAccount, showCompleted));
    } catch (e) {
      setErr(String(e));
    }
  }
  /** Create / rename / delete task lists (Tasks list manager). */
  async function newTaskList() {
    if (!activeAccount) return;
    const title = await showDialog({ kind: "prompt", message: "New list name" });
    if (!title || !title.trim()) return;
    try {
      await createTaskList(activeAccount, title.trim());
      setTaskLists(await listTaskLists(activeAccount));
      showUndo("List created");
    } catch (e) {
      setErr(String(e));
    }
  }
  async function renameTaskListRow(l: TaskListInfo) {
    if (!activeAccount) return;
    const title = await showDialog({ kind: "prompt", message: "Rename list", initial: l.title });
    if (!title || !title.trim() || title.trim() === l.title) return;
    try {
      await renameTaskList(activeAccount, l.id, title.trim());
      setTaskLists(await listTaskLists(activeAccount));
      reloadTasks();
    } catch (e) {
      setErr(String(e));
    }
  }
  async function deleteTaskListRow(l: TaskListInfo) {
    if (!activeAccount) return;
    if ((await showDialog({ kind: "confirm", message: `Delete the list "${l.title}" and all its tasks? This cannot be undone.` })) !== "ok")
      return;
    try {
      await deleteTaskList(activeAccount, l.id);
      setTaskLists(await listTaskLists(activeAccount));
      reloadTasks();
      showUndo("List deleted");
    } catch (e) {
      setErr(String(e));
    }
  }
  async function clearCompletedRow(l: TaskListInfo) {
    if (!activeAccount) return;
    try {
      await clearCompletedTasks(activeAccount, l.id);
      reloadTasks();
      showUndo(`Cleared completed in ${l.title}`);
    } catch (e) {
      setErr(String(e));
    }
  }
  function toggleTask(t: TaskItem) {
    if (!activeAccount) return;
    const done = t.status === "completed";
    setGtasks((prev) =>
      prev.map((x) => (x.id === t.id ? { ...x, status: done ? "needsAction" : "completed" } : x))
    );
    setTaskStatus(activeAccount, t.list_id, t.id, !done).catch((e) => setErr(String(e)));
  }
  function deleteTaskRow(t: TaskItem) {
    if (!activeAccount) return;
    deleteTask(activeAccount, t.list_id, t.id)
      .then(() => setGtasks((prev) => prev.filter((x) => x.id !== t.id)))
      .catch((e) => setErr(String(e)));
  }
  async function submitTask() {
    if (!activeAccount || !taskCreate.title.trim()) {
      setErr("Task needs a title.");
      return;
    }
    const list = taskListId || taskLists[0]?.id || "@default";
    const due = taskDue ? new Date(taskDue).toISOString() : "";
    try {
      await createTaskIn(activeAccount, list, taskCreate.title, taskCreate.notes, due);
      setTaskFormOpen(false);
      setTaskCreate({ title: "", notes: "" });
      setTaskDue("");
      await reloadTasks();
    } catch (e) {
      setErr(String(e));
    }
  }

  // Load lists + tasks on mount / account change (was the `view === "tasks"`
  // branch of App's workspace effect).
  useEffect(() => {
    setTaskFormOpen(false);
    if (!activeAccount) return;
    setTasksBusy(true);
    listTaskLists(activeAccount).then(setTaskLists).catch(() => {});
    listTasks(activeAccount)
      .then(setGtasks)
      .catch((e) => setErr(String(e)))
      .finally(() => setTasksBusy(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- setTaskFormOpen/setTasksBusy/setGtasks/setErr are store setters with unstable identity; tracking them would reload task lists on every render instead of only on account change.
  }, [activeAccount]);

  return (
    <div className="ws-panel">
      <div className="ws-head">
        <h2>Tasks</h2>
        <div className="ws-inline-toggle" title="Include completed tasks">
          <Checkbox
            label="Show completed"
            checked={showCompletedTasks}
            onChange={(on) => {
              setShowCompletedTasks(on);
              reloadTasks(on);
            }}
          />
        </div>
        <Button variant="secondary" size="sm" onClick={() => setListMgrOpen((o) => !o)}>
          Lists
        </Button>
        <Button
          variant="secondary"
          size="sm"
          onClick={() => {
            setTaskCreate({ title: "", notes: "" });
            setTaskDue("");
            setTaskFormOpen((o) => !o);
          }}
        >
          + New task
        </Button>
      </div>
      {listMgrOpen && (
        <div className="ws-form list-mgr">
          <div className="list-mgr-head">
            <span>Task lists</span>
            <Button variant="primary" size="sm" onClick={newTaskList}>
              + New list
            </Button>
          </div>
          {taskLists.map((l) => (
            <div className="list-mgr-row" key={l.id}>
              <span className="ws-title">{l.title}</span>
              <Button variant="ghost" size="sm" onClick={() => clearCompletedRow(l)}>
                Clear completed
              </Button>
              <Button variant="ghost" size="sm" onClick={() => renameTaskListRow(l)}>
                Rename
              </Button>
              <Button variant="danger" size="sm" onClick={() => deleteTaskListRow(l)}>
                Delete
              </Button>
            </div>
          ))}
          {taskLists.length === 0 && <p className="hint">No lists yet.</p>}
        </div>
      )}
      {taskFormOpen && (
        <div className="ws-form">
          <Input
            placeholder="Task title"
            value={taskCreate.title}
            onChange={(e) => setTaskCreate({ ...taskCreate, title: e.target.value })}
          />
          <textarea
            placeholder="Notes (optional)"
            value={taskCreate.notes}
            onChange={(e) => setTaskCreate({ ...taskCreate, notes: e.target.value })}
          />
          <label className="ws-form-row">
            <span>Due</span>
            <Input type="date" value={taskDue} onChange={(e) => setTaskDue(e.target.value)} />
          </label>
          {taskLists.length > 1 && (
            <Select
              label="Task list"
              value={taskListId || taskLists[0]?.id || ""}
              options={taskLists.map((l) => ({ value: l.id, label: l.title }))}
              onChange={setTaskListId}
            />
          )}
          <div className="ws-form-foot">
            <Button variant="primary" onClick={submitTask}>
              Create
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setTaskFormOpen(false)}>
              Cancel
            </Button>
          </div>
        </div>
      )}
      {tasksBusy ? (
        <SkeletonRows />
      ) : gtasks.length === 0 ? (
        <p className="hint">No open tasks.</p>
      ) : (
        <div className="ws-list">
          {gtasks.map((t) => (
            <div className="ws-row task-row" key={t.id}>
              <Checkbox
                label={<span className="sr-only">Complete</span>}
                checked={t.status === "completed"}
                onChange={() => toggleTask(t)}
              />
              <span className={`ws-title ${t.status === "completed" ? "task-done" : ""}`}>
                {t.title || "(untitled)"}
              </span>
              <span className="ws-sub">
                {t.list_title}
                {t.due ? ` · due ${t.due.slice(0, 10)}` : ""}
              </span>
              <IconButton label="Delete task" onClick={() => deleteTaskRow(t)}>
                <Icon name="trash" size={14} />
              </IconButton>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

export const TasksView = memo(TasksViewInner);
