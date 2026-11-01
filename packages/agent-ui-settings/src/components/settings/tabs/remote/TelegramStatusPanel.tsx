import { OfflinePanel } from "../shared";
import type { TelegramRemoteStatus, TelegramRemoteTest } from "@rightkit/agent-ui-core/lib/types";
import { RemoteStatusCard } from "./RemoteStatusCard";

function telegramStatusLabel(state: string): string {
  switch (state) {
    case "long_poll_requested":
      return "Configured";
    case "configured_without_allowlist":
      return "Missing allowlist";
    case "not_configured":
      return "Not configured";
    default:
      return state;
  }
}

export function TelegramStatusPanel({
  telegramStatus,
  telegramTest,
  error,
}: {
  telegramStatus: TelegramRemoteStatus | null;
  telegramTest: TelegramRemoteTest | null;
  error: string | null;
}) {
  const runtime = telegramStatus?.runtime;
  const cursorRows = runtime?.chatCursors ?? [];
  return (
    <>
      {telegramStatus ? (
        <div className="grid grid-cols-1 xl:grid-cols-2 gap-3">
          <RemoteStatusCard
            title="Telegram bot"
            value={telegramStatusLabel(telegramStatus.state)}
            status={
              telegramStatus.tokenConfigured
                ? telegramStatus.allowedUserCount > 0
                  ? "pass"
                  : "warn"
                : "block"
            }
            detail={
              telegramStatus.tokenConfigured
                ? "Bot token is configured and redacted from this view."
                : "No Telegram bot token is configured."
            }
          />
          <RemoteStatusCard
            title="Allowed users"
            value={String(telegramStatus.allowedUserCount)}
            status={telegramStatus.allowedUserCount > 0 ? "pass" : "warn"}
            detail="Only configured Telegram user ids can interact with the bot."
          />
          <RemoteStatusCard
            title="Long-poll task"
            value={telegramStatus.longPollRequested ? "Requested" : "Not requested"}
            status={telegramStatus.longPollRequested ? "pass" : "warn"}
            detail={
              runtime?.lastPollAt
                ? `Last poll ${runtime.lastPollAt}`
                : "No poll heartbeat has been recorded for this daemon run."
            }
          />
          <RemoteStatusCard
            title="Chat bindings"
            value={`${telegramStatus.bindingCount} bound`}
            status={telegramStatus.bindingStoreReadable ? "pass" : "warn"}
            detail={
              telegramStatus.bindingStoreReadable
                ? telegramStatus.bindingStorePath
                : telegramStatus.bindingStoreError || telegramStatus.bindingStorePath
            }
          />
          <RemoteStatusCard
            title="Poll success"
            value={runtime?.lastSuccessAt ?? "No successful poll yet"}
            status={
              runtime?.lastSuccessAt ? "pass" : telegramStatus.longPollRequested ? "warn" : "block"
            }
            detail={
              runtime?.lastError
                ? `Last error ${runtime.lastErrorAt ?? "unknown time"}: ${runtime.lastError}`
                : `Update offset ${runtime?.updateOffset ?? 0}`
            }
          />
          <RemoteStatusCard
            title="Active chats"
            value={String(runtime?.activeChatCount ?? 0)}
            status={(runtime?.activeChatCount ?? 0) > 0 ? "pass" : "warn"}
            detail={
              (runtime?.activeChatCount ?? 0) > 0
                ? "An allowed Telegram chat has been observed in this daemon run."
                : "No allowed Telegram chat has been observed yet."
            }
          />
        </div>
      ) : !error ? (
        <OfflinePanel
          title="No remote status loaded."
          action="Refresh to read Telegram setup state."
        />
      ) : null}

      {telegramTest ? (
        <div className="rounded-xl border border-line bg-panel overflow-hidden">
          <div className="flex items-center justify-between gap-3 px-4 py-3 border-b border-line">
            <div className="min-w-0">
              <h4 className="text-[13px] font-semibold text-text">Telegram readiness</h4>
              <p className="text-[12px] text-text-3">
                Secret-redacted checks from the daemon remote test endpoint.
              </p>
            </div>
            <span
              className={[
                "shrink-0 rounded-full px-2 py-0.5 text-[11px] font-semibold uppercase tracking-wide",
                telegramTest.verdict === "pass"
                  ? "bg-accent-2/15 text-accent-2"
                  : telegramTest.verdict === "warn"
                    ? "bg-accent/15 text-accent"
                    : "bg-danger/15 text-danger",
              ].join(" ")}
            >
              {telegramTest.verdict}
            </span>
          </div>
          <div className="grid grid-cols-1 xl:grid-cols-2 gap-3 p-3">
            {telegramTest.checks.map((check) => (
              <RemoteStatusCard
                key={check.id}
                title={check.label}
                value={check.status}
                status={check.status}
                detail={check.fix ? `${check.detail} - ${check.fix}` : check.detail}
              />
            ))}
          </div>
          {telegramTest.notes.length ? (
            <div className="border-t border-line px-4 py-3 text-[12px] text-text-3 leading-relaxed">
              {telegramTest.notes.map((note) => (
                <p key={note}>{note}</p>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}

      {cursorRows.length ? (
        <div className="rounded-xl border border-line bg-panel overflow-hidden">
          <div className="flex items-center justify-between gap-3 px-4 py-3 border-b border-line">
            <div className="min-w-0">
              <h4 className="text-[13px] font-semibold text-text">Chat cursors</h4>
              <p className="text-[12px] text-text-3">
                Redacted remote sessions and last event sequence pushed to Telegram.
              </p>
            </div>
          </div>
          <div className="divide-y divide-line">
            {cursorRows.map((row) => (
              <div
                className="grid grid-cols-[110px_1fr_120px] gap-3 px-4 py-3 text-[12px]"
                key={`${row.chat}-${row.sessionId}`}
              >
                <span className="text-text-2">{row.chat}</span>
                <span className="text-text truncate">{row.sessionId}</span>
                <span className="text-text-3 text-right">seq {row.lastPushedSeq}</span>
              </div>
            ))}
          </div>
        </div>
      ) : null}

      {telegramStatus?.notes.length ? (
        <div className="rounded-xl border border-line bg-panel p-4 text-[12px] text-text-3 leading-relaxed">
          {telegramStatus.notes.map((note) => (
            <p key={note}>{note}</p>
          ))}
        </div>
      ) : null}
    </>
  );
}
