import { Button, EmptyState } from "@rightkit/react-shell/react";
import { addAccount, syncAll } from "../lib/ipc";
import { FOLDERS } from "../lib/mail";

export function InboxEmptyState({
  accountsLength,
  busy,
  credsSet,
  folder,
  query,
  run,
  setUnreadOnly,
  setView,
  threadsLength,
  unreadOnly,
}: {
  accountsLength: number;
  busy: string | null;
  credsSet: boolean;
  folder: string;
  query: string;
  run: (key: string, fn: () => Promise<unknown>) => void;
  setUnreadOnly: (value: boolean) => void;
  setView: (view: "settings") => void;
  threadsLength: number;
  unreadOnly: boolean;
}) {
  const trimmedQuery = query.trim();
  const message =
    trimmedQuery
          ? `No results for "${trimmedQuery}".`
          : unreadOnly && threadsLength > 0
            ? "No unread messages here."
            : folder === "all" || folder === "inbox"
              ? credsSet
                ? accountsLength === 0
                  ? "No account connected yet."
                  : "No mail yet - sync to pull your inbox."
                : "Add your Google credentials to get started."
              : `No messages in ${FOLDERS.find((f) => f.key === folder)?.name ?? "this folder"}.`;
  const actions = (
      unreadOnly && threadsLength > 0 && !trimmedQuery ? (
        <>
          <Button variant="primary" onClick={() => setUnreadOnly(false)}>
            Show all mail
          </Button>
        </>
      ) : (
        !trimmedQuery &&
        (folder === "all" || folder === "inbox") && (
          <>
            {!credsSet && (
              <Button variant="primary" onClick={() => setView("settings")}>
                Open Settings
              </Button>
            )}
            {credsSet && accountsLength === 0 && (
              <Button
                variant="primary"
                disabled={!!busy}
                onClick={() => run("connect", addAccount)}
              >
                {busy === "connect" ? "Waiting for browser..." : "Connect Google"}
              </Button>
            )}
            {credsSet && accountsLength > 0 && (
              <Button
                variant="primary"
                disabled={!!busy}
                onClick={() => run("sync", syncAll)}
              >
                {busy === "sync" ? "Syncing..." : "Sync now"}
              </Button>
            )}
          </>
        )
      )
  );
  return <EmptyState title={message} action={actions} />;
}
