import { useState } from "react";
import type { SettingsTab } from "../components/settings/types.js";
import type { Pane } from "../components/WorkbenchContent.js";

export type RunTarget = "local" | "worktree";

export function useShellUiState() {
  const query = new URLSearchParams(window.location.search);
  const [pane, setPane] = useState<Pane>(toPane(query.get("pane")) ?? "session");
  const [runTarget, setRunTarget] = useState<RunTarget>(toRunTarget(query.get("run")) ?? "local");
  const [settingsTab, setSettingsTab] = useState<SettingsTab>(
    toSettingsTab(query.get("settings")) ?? "models",
  );
  const [showWorkspaceMenu, setShowWorkspaceMenu] = useState(query.get("workspaceMenu") === "1");
  const [showRunMenu, setShowRunMenu] = useState(query.get("runMenu") === "1");
  const [showBranchMenu, setShowBranchMenu] = useState(query.get("branchMenu") === "1");

  return {
    pane,
    setPane,
    runTarget,
    setRunTarget,
    settingsTab,
    setSettingsTab,
    showWorkspaceMenu,
    setShowWorkspaceMenu,
    showRunMenu,
    setShowRunMenu,
    showBranchMenu,
    setShowBranchMenu,
  };
}

function toPane(value: string | null): Pane | null {
  return value === "session" ||
    value === "tasks" ||
    value === "runtime" ||
    value === "terminal" ||
    value === "memory"
    ? value
    : null;
}

function toRunTarget(value: string | null): RunTarget | null {
  return value === "local" || value === "worktree" ? value : null;
}

function toSettingsTab(value: string | null): SettingsTab | null {
  return value === "appearance" ||
    value === "models" ||
    value === "providers" ||
    value === "mcp" ||
    value === "lsp" ||
    value === "plugins" ||
    value === "skills" ||
    value === "tools" ||
    value === "runtime" ||
    value === "middleware" ||
    value === "terminal" ||
    value === "tasks" ||
    value === "permissions" ||
    value === "execution" ||
    value === "remote" ||
    value === "diagnostics" ||
    value === "license" ||
    value === "budgets" ||
    value === "workspace"
    ? value
    : null;
}
