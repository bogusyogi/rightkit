// Tasks' contextual sidebar (interim): the task-list picker. The task items +
// add/complete live in TasksView; this nav just switches the active list via the
// shared store state.
import { NavList } from "@rightkit/react-shell/react";
import Icon from "../../components/Icons";
import { useAppStore } from "../../store";

export function TasksNav() {
  const taskLists = useAppStore((s) => s.taskLists);
  const taskListId = useAppStore((s) => s.taskListId);
  const setTaskListId = useAppStore((s) => s.setTaskListId);

  return (
    <div className="appnav">
      <div className="labels-head"><span>Lists</span></div>
      {taskLists.length === 0 ? (
        <p className="hint nav-hint">No task lists yet.</p>
      ) : (
        <NavList
          label="Task lists"
          groups={[
            {
              items: taskLists.map((l) => ({
                id: l.id,
                label: l.title,
                ariaLabel: l.title,
                icon: <Icon name="check" size={17} className="f-ic" />,
              })),
            },
          ]}
          activeId={taskListId ?? ""}
          onNavigate={setTaskListId}
          itemProps={(_item, { active }) => ({ className: `nav-shelf${active ? " active" : ""}` })}
        />
      )}
    </div>
  );
}
