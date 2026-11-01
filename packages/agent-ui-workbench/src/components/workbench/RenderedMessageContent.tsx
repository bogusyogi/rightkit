import VirtualizedCodeBlock, { type CodeLine } from "../VirtualizedCodeBlock.js";
import MarkdownText from "./MarkdownText.js";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { parseBracketedNotice } from "@rightkit/agent-ui-core/lib/messageNotices";

type ContentSegment =
  | { kind: "text"; text: string }
  | { kind: "code"; code: string; language: string };

function splitFencedCodeBlocks(content: string): ContentSegment[] {
  const segments: ContentSegment[] = [];
  const fence = /```([^\n`]*)\n?([\s\S]*?)```/g;
  let lastIndex = 0;
  for (const match of content.matchAll(fence)) {
    if (match.index > lastIndex) {
      segments.push({
        kind: "text",
        text: content.slice(lastIndex, match.index),
      });
    }
    segments.push({
      kind: "code",
      language: match[1]?.trim() ?? "",
      code: match[2] ?? "",
    });
    lastIndex = fence.lastIndex;
  }
  if (lastIndex < content.length) {
    segments.push({ kind: "text", text: content.slice(lastIndex) });
  }
  return segments.length > 0 ? segments : [{ kind: "text", text: content }];
}

export default function RenderedMessageContent({ content }: { content: string }) {
  // A message whose whole body is a single bracketed machine notice (the
  // bounded-run loop-safety gate and friends) is status, not an answer — render
  // it as a dim, quiet line instead of styling it like prose (UI-ISSUES §13).
  const notice = parseBracketedNotice(content || "");
  if (notice) {
    return (
      <div className="flex items-start gap-2 text-[13px] leading-[1.5] text-text-3">
        <span className="mt-[2px] flex-shrink-0 opacity-70">
          <Icon name="alert" size={13} />
        </span>
        <span className="min-w-0">{notice}</span>
      </div>
    );
  }

  const segments = splitFencedCodeBlocks(content || "(empty message)");
  return (
    <div className="flex flex-col gap-3 text-[14px] text-text-2 leading-[1.55]">
      {segments.map((segment) => {
        const key = `${segment.kind}-${segment.kind === "code" ? segment.code : segment.text}`;
        if (segment.kind === "code") {
          return (
            <VirtualizedCodeBlock
              key={key}
              lines={segment.code.split("\n").map((text): CodeLine => ({ text }))}
              language={segment.language}
              showLineNumbers={false}
            />
          );
        }
        const text = segment.text;
        if (!text.trim()) return null;
        return <MarkdownText key={key} text={text} />;
      })}
    </div>
  );
}
