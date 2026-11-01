import { MarkdownRenderer, type MarkdownClassNames, type MarkdownComponents } from "@rightkit/doc-render";
import React from "react";
import { openExternal } from "@rightkit/agent-ui-core/lib/secureStore";
import { githubRepositoryIdentity } from "@rightkit/agent-ui-core/lib/types/references";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { renderInlineReference } from "./references/referenceRegistry";

// Parsing, GFM, block/inline rendering, autolinks, URL and HTML safety are all RightKit's
// `MarkdownRenderer` (@rightkit/doc-render), rendered with its published structure. This module only
// supplies the host app styling classes plus three hooks: external links open through `openExternal`
// (GitHub repository chips), plain text resolves file-path / @mention inline references, and task
// list items keep their accessible checkbox label.

// File-path-shaped tokens and @mentions are only looked for inside plain text
// nodes, so code spans, links and URLs stay inert.
const TEXT_REFERENCE =
  /((?<!\S)(?:\/|[A-Za-z]:\|\.\.?[\/])\S+\.\w{1,6}\b|\b[A-Za-z0-9_.-]+\.\w{1,6}\b)|(?<![\w@])@([A-Za-z][A-Za-z0-9_-]{1,63})\b/g;

const LINK_CLASS =
  "text-accent underline decoration-line-2 hover:decoration-accent break-words cursor-pointer";

const CLASS_NAMES: MarkdownClassNames = {
  paragraph: "m-0 whitespace-pre-wrap break-words",
  heading: "m-0 font-semibold text-text",
  blockquote: "m-0 flex flex-col gap-2 border-l-2 border-line pl-3 text-text-2",
  list: "m-0 flex flex-col gap-1 pl-5",
  listItem: "text-text",
  inlineCode: "rounded-sm bg-panel px-1 py-px font-mono text-[0.92em] text-text",
  code: "m-0 max-w-full overflow-x-auto rounded-md border border-line bg-panel-2 px-3 py-2 font-mono text-[13px] leading-normal text-text",
  table: "w-max min-w-full border-collapse text-left text-[13px] leading-normal",
};

function renderText(value: string, kp: string): React.ReactNode | undefined {
  const out: React.ReactNode[] = [];
  let last = 0;
  let index = 0;
  for (const match of value.matchAll(TEXT_REFERENCE)) {
    if (match.index > last) {
      out.push(
        <React.Fragment key={`${kp}-${index}t`}>{value.slice(last, match.index)}</React.Fragment>,
      );
    }
    if (match[1] != null) {
      // A known workspace path or document becomes a typed affordance; an
      // unknown path keeps the inert dotted-underline treatment.
      const known = renderInlineReference(match[1], match[1]);
      out.push(
        known !== undefined ? (
          <React.Fragment key={`${kp}-${index}r`}>{known}</React.Fragment>
        ) : (
          <span
            key={`${kp}-${index}p`}
            className="font-mono text-text underline decoration-dotted decoration-line-2"
          >
            {match[1]}
          </span>
        ),
      );
    } else if (match[2] != null) {
      const known = renderInlineReference(`@${match[2]}`);
      out.push(
        <React.Fragment key={`${kp}-${index}s`}>
          {known !== undefined ? known : `@${match[2]}`}
        </React.Fragment>,
      );
    }
    last = match.index + match[0].length;
    index += 1;
  }
  if (out.length === 0) return undefined;
  if (last < value.length) {
    out.push(<React.Fragment key={`${kp}-${index}t`}>{value.slice(last)}</React.Fragment>);
  }
  return out;
}

const COMPONENTS: MarkdownComponents = {
  link: ({ href, children, explicit }) => {
    // Only the explicit [label](mailto:...) form links an address; a bare
    // address that GFM autolinks stays plain text.
    if (href.startsWith("mailto:") && !explicit) return <>{children}</>;
    const repository = githubRepositoryIdentity(href);
    return (
      <a
        href={href}
        title={href}
        data-rich-link={repository ? "repository" : "external"}
        className={
          repository
            ? "inline-flex items-center gap-1.5 rounded-md px-0.5 font-medium text-accent hover:underline"
            : LINK_CLASS
        }
        rel="noopener noreferrer"
        onClick={(event) => {
          event.preventDefault();
          void openExternal(href);
        }}
      >
        {repository ? <Icon name="github" size={15} /> : null}
        {repository?.label ?? children}
      </a>
    );
  },
  listItem: ({ checked, children }) => (
    <li className="text-text">
      {typeof checked === "boolean" ? (
        <span
          role="img"
          aria-label={checked ? "Completed task" : "Incomplete task"}
          className="mr-1"
        >
          {checked ? "☑" : "☐"}
        </span>
      ) : null}
      {children}
    </li>
  ),
};

export default function MarkdownText({ text }: { text: string }) {
  // Normalise line endings; trailing blank lines carry no content.
  const source = text.replace(/\r\n?/g, "\n").replace(/(?:\n[ \t]*)+$/, "");
  return (
    <MarkdownRenderer
      source={source}
      className="flex flex-col gap-2"
      classNames={CLASS_NAMES}
      components={COMPONENTS}
      onText={renderText}
    />
  );
}
