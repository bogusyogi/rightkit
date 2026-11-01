import RenderedMessageContent from "@rightkit/agent-ui-workbench/components/workbench/RenderedMessageContent";

// UserMessage — right-aligned bubble with a slightly lighter surface than
// the chat canvas. Matches the gray pill in the reference screenshots.

export interface UserMessageProps {
  content: string;
  maxWidthClass?: string;
}

export default function UserMessage({ content, maxWidthClass = "max-w-[80%]" }: UserMessageProps) {
  return (
    <article
      data-testid="user-message"
      className={`self-end ${maxWidthClass} rounded-2xl rounded-br-sm border border-line bg-panel-2 px-3.5 py-2.5 text-[14px] leading-relaxed text-text`}
    >
      <RenderedMessageContent content={content} />
    </article>
  );
}
