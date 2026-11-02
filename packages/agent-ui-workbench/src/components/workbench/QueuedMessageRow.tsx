import { useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { QueuedMessage } from "@rightkit/agent-ui-hooks/hooks/useQueuedMessages";
import ReferenceChipStrip from "./references/ReferenceChipStrip.js";
import { performReferenceAction } from "./references/referenceActions.js";

export default function QueuedMessageRow({
  msg,
  onSteer,
  onEdit,
  onDelete,
}: {
  msg: QueuedMessage;
  onSteer: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  const [showMenu, setShowMenu] = useState(false);
  const attachmentCount = msg.attachments?.length ?? 0;
  const referenceCount = msg.references?.length ?? 0;

  return (
    <div className="group flex flex-col gap-2 px-3 py-2.5 rounded-xl border border-line bg-panel-2 hover:border-line-2 transition-colors">
      <div className="flex items-center gap-2">
        <div className="w-5 h-5 grid place-items-center rounded-md border border-line bg-panel text-text-3 flex-shrink-0">
          <Icon name="send" size={11} />
        </div>
        <p className="flex-1 text-[13px] text-text-2 truncate min-w-0">
          {msg.text || `${attachmentCount} image attachment${attachmentCount === 1 ? "" : "s"}`}
        </p>
        {attachmentCount > 0 ? (
          <span className="rounded-full border border-line bg-panel px-2 py-0.5 text-[11px] text-text-3">
            {attachmentCount} image{attachmentCount === 1 ? "" : "s"}
          </span>
        ) : null}
        {referenceCount > 0 ? (
          <span className="rounded-full border border-line bg-panel px-2 py-0.5 text-[11px] text-text-3">
            {referenceCount} reference{referenceCount === 1 ? "" : "s"}
          </span>
        ) : null}
        <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 group-focus-visible:opacity-100 transition-opacity flex-shrink-0">
          <button
            type="button"
            className="inline-flex items-center gap-1 h-6 px-2.5 rounded-lg border border-accent/50 bg-accent/10 text-accent text-[11px] font-semibold cursor-pointer hover:bg-accent/20 transition-colors"
            onClick={onSteer}
            title="Inject at next gap"
          >
            Steer
          </button>
          <button
            type="button"
            className="w-6 h-6 grid place-items-center rounded-md border border-line bg-transparent text-text-3 cursor-pointer hover:text-text hover:bg-panel transition-colors"
            onClick={onEdit}
            title="Edit — returns to composer"
            aria-label="Edit queued message"
          >
            <Icon name="edit" size={12} />
          </button>
          <button
            type="button"
            className="w-6 h-6 grid place-items-center rounded-md border border-line bg-transparent text-text-3 cursor-pointer hover:text-danger hover:border-danger transition-colors"
            onClick={onDelete}
            title="Remove from queue"
            aria-label="Remove from queue"
          >
            <Icon name="trash" size={12} />
          </button>
          <div className="relative">
            <button
              type="button"
              className="w-6 h-6 grid place-items-center rounded-md border border-line bg-transparent text-text-3 cursor-pointer hover:text-text hover:bg-panel transition-colors"
              onClick={() => setShowMenu((v) => !v)}
              aria-label="More message actions"
            >
              <Icon name="more" size={12} />
            </button>
            {showMenu ? (
              <div className="absolute bottom-[calc(100%+4px)] right-0 z-50 flex flex-col min-w-[168px] rounded-xl border border-line bg-panel shadow-app overflow-hidden">
                <button
                  type="button"
                  className="flex items-center gap-2 px-3 py-2 border-0 bg-transparent text-text-2 text-[13px] text-left cursor-pointer hover:bg-panel-2 hover:text-text"
                  onClick={() => {
                    onEdit();
                    setShowMenu(false);
                  }}
                >
                  <Icon name="edit" size={13} /> Edit message
                </button>
                <button
                  type="button"
                  className="flex items-center gap-2 px-3 py-2 border-0 bg-transparent text-text-2 text-[13px] text-left cursor-pointer hover:bg-panel-2 hover:text-text"
                  onClick={() => {
                    onSteer();
                    setShowMenu(false);
                  }}
                >
                  <Icon name="send" size={13} /> Send immediately
                </button>
                <hr className="border-0 border-t border-line my-1" />
                <button
                  type="button"
                  className="flex items-center gap-2 px-3 py-2 border-0 bg-transparent text-danger text-[13px] text-left cursor-pointer hover:bg-panel-2"
                  onClick={() => {
                    onDelete();
                    setShowMenu(false);
                  }}
                >
                  <Icon name="trash" size={13} /> Remove
                </button>
              </div>
            ) : null}
          </div>
        </div>
      </div>
      {referenceCount > 0 ? (
        <ReferenceChipStrip
          references={msg.references ?? []}
          compact={false}
          ariaLabel="Queued references"
          onAction={(id, action) => {
            const reference = msg.references?.find((item) => item.id === id);
            if (reference) void performReferenceAction(reference, action);
          }}
        />
      ) : null}
    </div>
  );
}
