import type Sidebar from "../../components/Sidebar.js";
import type {
  DaemonConfig,
  ExecutionBackendRegistry,
  ExecutionWorkspaceRecord,
  ModelCatalogEntry,
} from "@rightkit/agent-ui-core/lib/types";
import { projectName } from "@rightkit/agent-ui-core/lib/utils";
import { buildConnectionLabel } from "./shared.js";

type SetStatus = (value: string) => void;

export type SidebarExecution = {
  backendRegistry: ExecutionBackendRegistry | null;
  workspaces: ExecutionWorkspaceRecord[];
  workspacePending: boolean;
  createWorkspace: (
    config: DaemonConfig | null,
    workspaceRoot: string,
    backendId: string,
    name: string,
    setStatus: SetStatus,
    onCreated: (workspaceId: string) => void,
  ) => Promise<void>;
};

export type BuildSidebarPropsParams = {
  sessions: Parameters<typeof Sidebar>[0]["sessions"];
  selectedId: string | null;
  workspaceRoot: string;
  config: DaemonConfig | null;
  modelOptions: ModelCatalogEntry[];
  execution: SidebarExecution;
  newSessionControls: Parameters<typeof Sidebar>[0]["newSessionControls"];
  search: string;
  searchPending: boolean;
  searchSnippets: Parameters<typeof Sidebar>[0]["searchSnippets"];
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
  onNewSession: (
    driverKind?: Parameters<typeof Sidebar>[0]["newSessionControls"]["driverKind"],
  ) => void;
  onForkCurrentSession: () => void;
  onNewSessionControls: (next: Parameters<typeof Sidebar>[0]["newSessionControls"]) => void;
  onSearch: (next: string) => void;
  setStatus: SetStatus;
};

export function buildSidebarProps(params: BuildSidebarPropsParams): Parameters<typeof Sidebar>[0] {
  return {
    sessions: params.sessions,
    selectedId: params.selectedId,
    workspaceRoot: params.workspaceRoot,
    config: params.config,
    connectionLabel: buildConnectionLabel(params.config),
    modelOptions: params.modelOptions,
    executionBackendRegistry: params.execution.backendRegistry,
    executionWorkspaces: params.execution.workspaces,
    executionWorkspacePending: params.execution.workspacePending,
    newSessionControls: params.newSessionControls,
    search: params.search,
    searchPending: params.searchPending,
    searchSnippets: params.searchSnippets,
    onSelect: params.onSelect,
    onDelete: params.onDelete,
    onNewSession: params.onNewSession,
    onForkCurrentSession: params.onForkCurrentSession,
    onNewSessionControls: params.onNewSessionControls,
    onSearch: params.onSearch,
    onCreateExecutionWorkspace: () => {
      const backendId =
        params.newSessionControls.executionBackend.trim() ||
        params.execution.backendRegistry?.defaultBackend ||
        "local";
      void params.execution.createWorkspace(
        params.config,
        params.workspaceRoot,
        backendId,
        `${projectName(params.workspaceRoot)} ${backendId}`,
        params.setStatus,
        (workspaceId: string) =>
          params.onNewSessionControls({
            ...params.newSessionControls,
            executionWorkspaceId: workspaceId,
          }),
      );
    },
  };
}
