import { TBtn, Sep, Group, Dropdown, Item } from "@rightkit/document-core/documents/toolbarPrimitives";
import { DIAGRAM_TEMPLATES, insertTableTemplate, mermaidBlock } from "@rightkit/markdown-editor/documents/markdown/markdownTools";
import { shortcutLabel } from "@rightkit/document-core/platform";

const HEADINGS = [
  { label: "Paragraph", level: 0 },
  { label: "Heading 1", level: 1 },
  { label: "Heading 2", level: 2 },
  { label: "Heading 3", level: 3 },
  { label: "Heading 4", level: 4 },
  { label: "Heading 5", level: 5 },
  { label: "Heading 6", level: 6 },
];

export function MarkdownSourceToolbar({
  setHeading,
  wrapInline,
  linePrefix,
  openLink,
  insertBlock,
  formatTables,
}: {
  setHeading: (level: number) => void;
  wrapInline: (before: string, after: string) => void;
  linePrefix: (prefix: string) => void;
  openLink: (prefix: "[" | "![") => void;
  insertBlock: (block: string, caretOffset?: number) => void;
  formatTables: () => void;
}) {
  return (
    <>
      <Dropdown label="Heading" title="Paragraph style" minWidth={92} panelWidth={130}>
        {(close) =>
          HEADINGS.map((h) => (
            <Item
              key={h.level}
              onRun={() => {
                setHeading(h.level);
                close();
              }}
            >
              {h.label}
            </Item>
          ))
        }
      </Dropdown>
      <Sep />
      <Group>
        <TBtn
          label="B"
          title={`Bold (${shortcutLabel("mod+b")})`}
          bold
          onRun={() => wrapInline("**", "**")}
        />
        <TBtn
          label="I"
          title={`Italic (${shortcutLabel("mod+i")})`}
          italic
          onRun={() => wrapInline("*", "*")}
        />
        <TBtn label="S" title="Strikethrough" strike onRun={() => wrapInline("~~", "~~")} />
        <TBtn label="H" title="Highlight" onRun={() => wrapInline("==", "==")} />
        <TBtn label="<" title="Inline code" width={32} onRun={() => wrapInline("`", "`")} />
      </Group>
      <Sep />
      <Group>
        <TBtn label="•" title="Bulleted list" onRun={() => linePrefix("- ")} />
        <TBtn label="1." title="Numbered list" onRun={() => linePrefix("1. ")} />
        <TBtn label="☑" title="Task list" onRun={() => linePrefix("- [ ] ")} />
        <TBtn label="❝" title="Blockquote" onRun={() => linePrefix("> ")} />
      </Group>
      <Sep />
      <Group>
        <TBtn
          label="link"
          title={`Link (${shortcutLabel("mod+k")})`}
          width={40}
          onRun={() => openLink("[")}
        />
        <TBtn label="[[" title="Wiki link" width={32} onRun={() => wrapInline("[[", "]]")} />
        <TBtn label="img" title="Image" width={36} onRun={() => openLink("![")} />
      </Group>
      <Sep />
      <Group>
        <TBtn
          label="▦"
          title="Insert table"
          onRun={() => {
            const table = insertTableTemplate(3, 3);
            insertBlock(table, table.length);
          }}
        />
        <TBtn label="fmt|" title="Format tables" width={42} onRun={formatTables} />
        <TBtn label="―" title="Horizontal rule" onRun={() => insertBlock("---\n")} />
        <TBtn
          label="{ }"
          title="Code block"
          width={32}
          onRun={() => insertBlock("```\n\n```\n", 4)}
        />
      </Group>
      <Sep />
      <Group>
        <TBtn
          label="toc"
          title="Insert table of contents"
          width={38}
          onRun={() => insertBlock("[toc]\n\n")}
        />
        <Dropdown label="diagram" title="Insert Mermaid diagram" minWidth={74} panelWidth={158}>
          {(close) =>
            DIAGRAM_TEMPLATES.map((template) => (
              <Item
                key={template.label}
                onRun={() => {
                  insertBlock(mermaidBlock(template.code));
                  close();
                }}
              >
                {template.label}
              </Item>
            ))
          }
        </Dropdown>
      </Group>
    </>
  );
}
