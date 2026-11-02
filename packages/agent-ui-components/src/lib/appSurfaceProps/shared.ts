import type { Pane } from "../../components/WorkbenchContent.js";
import type AppShellHeader from "@rightkit/agent-ui-core/components/app/AppShellHeader";
import { isTauriHost } from "@rightkit/agent-ui-core/lib/daemon";
import type { HealthResponse } from "@rightkit/agent-ui-core/lib/types";

export type AppShellHeaderProps = Parameters<typeof AppShellHeader>[0];

export type BuildHeaderPropsParams = {
  pane: Pane;
  selectedTitle: string | null;
  selectedWorkspaceRoot: string | null;
  workspaceRoot: string;
  engineReady: boolean;
  status: string;
  health: HealthResponse | null;
  selectedId: string | null;
  messagesLength: number;
  isWorking: boolean;
  onCompact: () => void;
};

export function buildHeaderTitle({
  pane,
  selectedTitle,
  workspaceRoot,
}: Pick<BuildHeaderPropsParams, "pane" | "selectedTitle" | "workspaceRoot">): string {
  if (pane === "terminal") return "Integrated Terminal";
  if (selectedTitle) return selectedTitle;
  if (workspaceRoot) return `Ready in ${workspaceRoot}`;
  return "Choose a workspace";
}

export function buildHeaderSubtitle({
  pane,
  selectedWorkspaceRoot,
  workspaceRoot,
  engineReady,
}: Pick<
  BuildHeaderPropsParams,
  "pane" | "selectedWorkspaceRoot" | "workspaceRoot" | "engineReady"
>): string {
  if (pane === "terminal") {
    return (
      selectedWorkspaceRoot ||
      workspaceRoot ||
      (engineReady ? "Open a terminal in the current project." : "Open a project folder to begin.")
    );
  }
  return (
    selectedWorkspaceRoot ||
    workspaceRoot ||
    (engineReady ? "Local engine ready" : "Open a project folder to begin.")
  );
}

export function buildHeaderProps(params: BuildHeaderPropsParams): AppShellHeaderProps {
  return {
    headerTitle: buildHeaderTitle(params),
    headerSubtitle: buildHeaderSubtitle(params),
    status: params.status,
    health: params.health,
    selected: params.selectedId ? { id: params.selectedId } : null,
    messagesLength: params.messagesLength,
    isWorking: params.isWorking,
    onCompact: params.onCompact,
  };
}

export function buildConnectionLabel(config: { url: string } | null): string {
  if (
    !isTauriHost() &&
    typeof window !== "undefined" &&
    new URLSearchParams(window.location.search).get("qa") === "1"
  ) {
    return "Preview mode";
  }
  if (!config) return "disconnected";
  const host = config.url.replace(/^https?:\/\//, "").split(":")[0];
  if (host === "127.0.0.1" || host === "localhost") return "local engine";
  return host;
}
