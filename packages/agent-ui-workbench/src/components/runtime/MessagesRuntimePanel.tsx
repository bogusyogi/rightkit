import Icon from "@rightkit/agent-ui-core/components/Icon";
import { EmptyState } from "@rightkit/react-shell/react";
import type { MessageHistoryRow } from "@rightkit/agent-ui-core/lib/types";
import { formatIsoTime } from "./runtimeUtils";

export default function MessagesRuntimePanel({
  messages,
  messageStorePath,
}: {
  messages: MessageHistoryRow[];
  messageStorePath: string;
}) {
  if (messages.length === 0) {
    return (
      <EmptyState icon={<Icon name="send" size={18} />} title="No messages yet">{"Inter-session messages appear here after `send_message` runs. Use the tool to coordinate between parallel sessions."}</EmptyState>
    );
  }

  return (
    <div className="flex-1 min-h-0 overflow-y-auto">
      <div className="px-5 py-3 border-b border-line text-[12px] text-text-3 truncate">
        {messageStorePath}
      </div>

      <div className="flex flex-col divide-y divide-line">
        {messages.map((message) => (
          <div
            className="grid grid-cols-[minmax(0,1fr)_minmax(180px,0.7fr)] gap-4 px-5 py-3"
            key={message.id}
          >
            <div className="min-w-0">
              <strong className="block text-[13px] font-semibold text-text truncate">
                {message.fromSessionId}
                {" -> "}
                {message.targetSessionId || "broadcast"}
              </strong>
              <p className="text-[12px] text-text-2 truncate">{message.message}</p>
              {message.error ? (
                <p className="text-[11px] text-danger truncate">{message.error}</p>
              ) : null}
            </div>

            <div className="min-w-0 text-right">
              <code className="block text-[12px] font-mono text-text-2 truncate">
                {message.channel}
              </code>
              <span className="block text-[11px] text-text-3 truncate">
                {message.deliveryStatus} - {message.deliveredSessionIds.length} delivered
              </span>
              <span className="block text-[11px] text-text-3 truncate">
                {formatIsoTime(message.createdAt)}
              </span>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
