import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";

import type { TaskRow } from "@rightkit/agent-ui-core/lib/types";
import TasksPane from "./TasksPane";

function task(state: TaskRow["state"]): TaskRow {
  return {
    id: `task-${state}`,
    title: `Task ${state}`,
    deps: [],
    doneWhen: "verified",
    doneWhenPredicates: [],
    state,
    category: "test",
    estimatedDuration: "1m",
    assignedRole: "worker",
    escalationPath: "root",
    openQuestions: [],
    createdAt: "2026-08-11T00:00:00Z",
    updatedAt: "2026-08-11T00:00:00Z",
    taskStorePath: "/private/workspace/.coderight/tasks.json",
  };
}

describe("TasksPane", () => {
  it("shows one calm empty state without lifecycle columns", () => {
    const html = renderToStaticMarkup(<TasksPane tasks={[]} />);
    expect(html).toContain("No tasks yet");
    expect(html).not.toContain("Nothing here");
    expect(html).not.toContain("IN PROGRESS");
  });

  it("renders one task list with status badges & hides internal absolute task path", () => {
    const html = renderToStaticMarkup(<TasksPane tasks={[task("open"), task("inProgress")]} />);
    expect(html).toContain("Task open");
    expect(html).toContain("Task inProgress");
    expect(html).toContain("In progress");
    expect(html).toContain("2 tasks");
    expect(html).toContain("Workspace task records");
    expect(html).not.toContain(">/private/workspace/.coderight/tasks.json<");
    expect(html).not.toContain("Nothing here");
  });
});
