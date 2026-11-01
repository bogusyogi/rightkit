import { useCallback, useEffect, useState } from "react";
import {
  accountHomeConflict,
  fetchAccounts,
  type Account,
  type AccountHomeConflict,
} from "@rightkit/agent-ui-core/lib/accounts/accounts";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";
import { useAccountSelectionStore } from "@rightkit/agent-ui-core/state/accountSelectionStore";

/**
 * Vendor accounts plus the selection for this session and project (issue #34).
 * The daemon is the source of truth; the local store is a cache refreshed on
 * session change. Nothing here switches accounts automatically.
 */
export function useAccounts(
  config: DaemonConfig | null,
  sessionId: string | null,
  project: string | null,
) {
  const [accounts, setAccounts] = useState<Account[]>([]);
  // Same-home rule (#343): set when the engine refuses to move a started chat to an account with a
  // different config home. The UI offers a new session on that account instead.
  const [homeConflict, setHomeConflict] = useState<AccountHomeConflict | null>(null);
  const chatAccount = useAccountSelectionStore((s) => (sessionId ? s.chat[sessionId] : undefined));
  const projectAccount = useAccountSelectionStore((s) => (project ? s.project[project] : undefined));

  useEffect(() => {
    if (!config) {
      setAccounts([]);
      return;
    }
    let cancelled = false;
    void fetchAccounts(config)
      .then((list) => {
        if (!cancelled) setAccounts(list);
      })
      .catch(() => {
        if (!cancelled) setAccounts([]);
      });
    void useAccountSelectionStore
      .getState()
      .syncFromDaemon(config, sessionId, project)
      .catch(() => {
        /* keep the cached selection when the daemon is unreachable */
      });
    return () => {
      cancelled = true;
    };
  }, [config, sessionId, project]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: sessionId is the trigger, not a value read here: a same-home conflict belongs to one session and must clear when the session changes.
  useEffect(() => {
    setHomeConflict(null);
  }, [sessionId]);

  const selectAccount = useCallback(
    (accountId: string, options?: { threadStarted?: boolean }) => {
      if (!config || !sessionId) return;
      setHomeConflict(null);
      void useAccountSelectionStore
        .getState()
        .selectChatAccount(config, sessionId, accountId, options)
        .catch((error: unknown) => {
          const conflict = accountHomeConflict(error);
          if (conflict) {
            setHomeConflict({ ...conflict, requestedAccountId: conflict.requestedAccountId ?? accountId });
            return;
          }
          console.warn("Account selection failed", error);
        });
    },
    [config, sessionId],
  );

  const dismissHomeConflict = useCallback(() => setHomeConflict(null), []);

  const setProjectDefault = useCallback(
    (accountId: string) => {
      if (!config || !project) return;
      void useAccountSelectionStore
        .getState()
        .selectProjectDefault(config, project, accountId)
        .catch((error: unknown) => console.warn("Project default failed", error));
    },
    [config, project],
  );

  return {
    accounts,
    selectedAccountId: chatAccount ?? projectAccount ?? null,
    projectDefaultAccountId: projectAccount ?? null,
    selectAccount,
    setProjectDefault,
    homeConflict,
    dismissHomeConflict,
  };
}
