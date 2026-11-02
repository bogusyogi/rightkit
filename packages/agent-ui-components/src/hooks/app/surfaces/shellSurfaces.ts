// Shell surfaces — header, sidebar, and workspace-trust prop assembly.
// Pure forwarding into the lib/appSurfaceProps builders; no effects.

import {
  buildHeaderProps,
  buildSidebarProps,
  buildTrustGateProps,
} from "../../../lib/appSurfaceProps/index.js";
import { forkSession, patchSession } from "@rightkit/agent-ui-core/lib/daemon";
import { buildHandoffSummary } from "@rightkit/agent-ui-core/lib/accounts/accounts";
import { useAccountSelectionStore } from "@rightkit/agent-ui-core/state/accountSelectionStore";
import { useComposerStore } from "@rightkit/agent-ui-core/state/composerStore";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";
import type WorkspaceTrustGate from "@rightkit/agent-ui-core/components/app/WorkspaceTrustGate";
import type { AppSurfaceInputs } from "./types.js";

export interface ShellSurfaces {
  headerProps: ReturnType<typeof buildHeaderProps>;
  sidebarProps: ReturnType<typeof buildSidebarProps>;
  trustGateProps: Parameters<typeof WorkspaceTrustGate>[0] | null;
  sessionActions: {
    renameSession: (id: string, title: string) => void;
    archiveSession: (id: string, title: string) => void;
    forkSelectedSession: () => void;
    deleteSelectedSession: (id: string) => void;
    continueOnAccount: (accountId: string, cappedLabel: string) => void;
  };
}

export function buildShellSurfaces(a: AppSurfaceInputs): ShellSurfaces {
  const {
    pane,
    selected,
    workspaceRoot,
    engineReady,
    status,
    health,
    messages,
    isWorking,
    compact,
    sidebarSessions,
    config,
    enabledModelOptions,
    execution,
    newSessionControls,
    sessionSearch,
    sessionSearchPending,
    sessionSearchSnippets,
    setSelectedId,
    setPane,
    deleteSessionById,
    newSession,
    setNewSessionControls,
    setSessionSearch,
    setStatus,
    refresh,
    permissions,
  } = a;

  const headerProps = buildHeaderProps({
    pane,
    selectedTitle: selected?.title ?? null,
    selectedWorkspaceRoot: selected?.workspaceRoot ?? null,
    workspaceRoot,
    engineReady,
    status,
    health,
    selectedId: selected?.id ?? null,
    messagesLength: messages.length,
    isWorking,
    onCompact: () => void compact(),
  });

  const forkSelectedSession = () => {
    if (!config || !selected?.id) {
      setStatus("Select a session before forking");
      return;
    }
    void forkSession(config, selected.id)
      .then(async (forked) => {
        await refresh(config);
        setSelectedId(forked.id);
        setPane("session");
        setStatus(`Forked session: ${forked.title}`);
      })
      .catch((err: unknown) => {
        setStatus(err instanceof Error ? err.message : "Fork session failed");
      });
  };

  const sessionActions: ShellSurfaces["sessionActions"] = {
    renameSession: (id: string, title: string) => {
      if (!config || !id) {
        setStatus("Select a session before renaming");
        return;
      }
      const nextTitle = title.trim();
      if (!nextTitle) {
        setStatus("Session title cannot be empty");
        return;
      }
      void patchSession(config, id, { title: nextTitle })
        .then(async () => {
          await refresh(config);
          setStatus(`Renamed session: ${nextTitle}`);
        })
        .catch((err: unknown) => {
          setStatus(err instanceof Error ? err.message : "Rename session failed");
        });
    },
    archiveSession: (id: string, title: string) => {
      if (!config || !id) {
        setStatus("Select a session before archiving");
        return;
      }
      void patchSession(config, id, { status: "archived" })
        .then(async () => {
          await refresh(config);
          setSelectedId(null);
          setStatus(`Archived session: ${title}`);
        })
        .catch((err: unknown) => {
          setStatus(err instanceof Error ? err.message : "Archive session failed");
        });
    },
    forkSelectedSession,
    deleteSelectedSession: (id: string) => void deleteSessionById(id),
    continueOnAccount: (accountId: string, cappedLabel: string) => {
      if (!config || !selected?.id) return;
      // Manual only: a new session on the chosen account, seeded with a handoff
      // the user reviews and sends. The capped session is left untouched.
      const handoff = buildHandoffSummary(messages, cappedLabel);
      void newSession(selected.workspaceRoot || undefined)
        .then(async (id) => {
          if (!id) return;
          await useAccountSelectionStore.getState().selectChatAccount(config, id, accountId, { threadStarted: false });
          useComposerStore.getState().setComposer(handoff);
          setStatus("Continuing on the selected account. Review the handoff and send.");
        })
        .catch((err: unknown) => {
          setStatus(err instanceof Error ? err.message : "Continue on account failed");
        });
    },
  };

  const sidebarProps = buildSidebarProps({
    sessions: sidebarSessions,
    selectedId: selected?.id ?? null,
    workspaceRoot,
    config,
    modelOptions: enabledModelOptions,
    execution: {
      backendRegistry: execution.backendRegistry,
      workspaces: execution.workspaces,
      workspacePending: execution.workspacePending,
      createWorkspace: (
        config: DaemonConfig | null,
        workspaceRoot: string,
        backendId: string,
        name: string,
        setStatus: (value: string) => void,
        onCreated: (workspaceId: string) => void,
      ) => execution.createWorkspace(config, workspaceRoot, backendId, name, setStatus, onCreated),
    },
    newSessionControls,
    search: sessionSearch,
    searchPending: sessionSearchPending,
    searchSnippets: sessionSearchSnippets,
    onSelect: (id: string) => {
      setSelectedId(id);
      setPane("session");
    },
    onDelete: (id: string) => void deleteSessionById(id),
    onNewSession: (driverKind) => void newSession(undefined, [], driverKind),
    onForkCurrentSession: forkSelectedSession,
    onNewSessionControls: setNewSessionControls,
    onSearch: setSessionSearch,
    setStatus,
  });

  const workspaceTrustRequiresDecision = Boolean(permissions.workspaceTrust?.requiresTrust);

  const trustGateProps: Parameters<typeof WorkspaceTrustGate>[0] | null =
    workspaceRoot.trim() && workspaceTrustRequiresDecision && permissions.workspaceTrust
      ? buildTrustGateProps({
          workspaceRoot,
          trust: permissions.workspaceTrust,
          pending: permissions.workspaceTrustPending,
          onSetWorkspaceTrust: (trust) =>
            void permissions.applyWorkspaceTrust(config, workspaceRoot, trust, setStatus),
        })
      : null;

  return { headerProps, sidebarProps, trustGateProps, sessionActions };
}
