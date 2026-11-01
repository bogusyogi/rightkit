import { type Node as PmNode, type NodeSpec, Schema } from "prosemirror-model";
import { markdownTableNodeSpecs } from "./tables/markdownTableSchema";

export const markdownHybridSchema = new Schema({
  nodes: {
    doc: { content: "block+", attrs: { trailing: { default: "" } } },
    paragraph: blockSpec("p"),
    heading: {
      ...blockSpec("h1"),
      attrs: {
        level: { default: 1 },
        prefix: { default: "" },
        rawSource: { default: "" },
        rawText: { default: "" },
        ...sourceRangeAttrs(),
      },
      toDOM(node) {
        return [`h${node.attrs.level}`, 0];
      },
    },
    blockquote: {
      ...containerBlockSpec("blockquote"),
      content: "block+",
      toDOM: () => ["blockquote", 0],
    },
    bullet_list: {
      ...containerBlockSpec("ul"),
      content: "list_item+",
      toDOM: () => ["ul", 0],
    },
    ordered_list: {
      ...containerBlockSpec("ol"),
      content: "list_item+",
      attrs: {
        ...containerAttrs(),
        start: { default: 1 },
      },
      toDOM(node) {
        return node.attrs.start > 1 ? ["ol", { start: node.attrs.start }, 0] : ["ol", 0];
      },
    },
    list_item: {
      content: "paragraph block*",
      attrs: {
        rawText: { default: "" },
      },
      toDOM: () => ["li", 0],
    },
    description_list: {
      group: "block",
      content: "description_item+",
      attrs: containerAttrs(),
      toDOM: () => ["dl", 0],
    },
    description_item: {
      group: "block",
      content: "description_term+ description_details+",
      toDOM: () => ["div", { class: "vr-dl-item" }, 0],
    },
    description_term: {
      inline: false,
      group: "block",
      content: "inline*",
      toDOM: () => ["dt", 0],
    },
    description_details: {
      inline: false,
      group: "block",
      content: "inline*",
      toDOM: () => ["dd", 0],
    },
    table: markdownTableNodeSpecs.table,
    table_row: markdownTableNodeSpecs.table_row,
    table_cell: markdownTableNodeSpecs.table_cell,
    table_header: markdownTableNodeSpecs.table_header,
    code_fence: {
      group: "block",
      content: "text*",
      marks: "",
      code: true,
      defining: true,
      isolating: true,
      selectable: true,
      attrs: {
        prefix: { default: "" },
        rawSource: { default: "" },
        rawText: { default: "" },
        ...sourceRangeAttrs(),
        language: { default: "" },
        meta: { default: "" },
        fenceChar: { default: "`" },
        fenceLength: { default: 3 },
        fenceIndent: { default: "" },
        contentIndent: { default: "" },
        openingSuffix: { default: "\n" },
        trailingNewline: { default: true },
        infoFrom: { default: -1 },
        infoTo: { default: -1 },
        contentFrom: { default: -1 },
        contentTo: { default: -1 },
      },
      toDOM: () => [
        "div",
        { class: "vr-pm-code-fence" },
        [
          "pre",
          { class: "vr-pm-code-fence__pre" },
          ["code", { class: "vr-pm-code-fence__code" }, 0],
        ],
      ],
    },
    raw_block: {
      group: "block",
      atom: true,
      selectable: true,
      attrs: {
        prefix: { default: "" },
        rawSource: { default: "" },
        mdastType: { default: "unknown" },
        sourceLine: { default: 1 },
        ...sourceRangeAttrs(),
      },
      toDOM(node) {
        return [
          "pre",
          { "data-vr-raw-block": node.attrs.mdastType, contenteditable: "false" },
          ["code", node.attrs.rawSource],
        ];
      },
    },
    text: { group: "inline" },
    wiki_link: {
      group: "inline",
      inline: true,
      atom: true,
      selectable: true,
      attrs: {
        target: { default: "" },
        hash: { default: "" },
        display: { default: "" },
      },
      toDOM(node) {
        const href = node.attrs.target
          ? node.attrs.hash
            ? `${node.attrs.target}#${node.attrs.hash}`
            : node.attrs.target
          : `#${node.attrs.hash}`;
        return ["a", { href, rel: "noreferrer" }, node.attrs.display || href];
      },
    },
    citation: {
      group: "inline",
      inline: true,
      atom: true,
      selectable: true,
      attrs: {
        key: { default: "" },
        locator: { default: "" },
      },
      toDOM(node) {
        return ["span", { "data-vr-citation": node.attrs.key }, `[${node.attrs.key}]`];
      },
    },
    image: {
      group: "inline",
      inline: true,
      atom: true,
      selectable: true,
      draggable: true,
      attrs: {
        src: { default: "" },
        alt: { default: "" },
        title: { default: null },
        rawSource: { default: "" },
        sourceFrom: { default: -1 },
        sourceTo: { default: -1 },
        prefixFrom: { default: -1 },
        prefixTo: { default: -1 },
      },
      toDOM(node: PmNode) {
        const attrs: Record<string, string> = {
          src: String(node.attrs.src ?? ""),
          alt: String(node.attrs.alt ?? ""),
        };
        const title = node.attrs.title as string | null;
        if (title) attrs.title = String(title);
        return ["img", attrs];
      },
    },
  },
  marks: {
    strong: {
      toDOM: () => ["strong", 0],
    },
    em: {
      toDOM: () => ["em", 0],
    },
    code: {
      code: true,
      inclusive: false,
      toDOM: () => ["code", 0],
    },
    abbr: {
      inclusive: false,
      toDOM(node) {
        return ["abbr", { title: node.attrs.title }, 0];
      },
      attrs: {
        title: { default: "" },
      },
    },
    delete: {
      toDOM: () => ["s", 0],
    },
    mark: {
      toDOM: () => ["mark", 0],
    },
    link: {
      attrs: {
        href: { default: "" },
        title: { default: null },
      },
      inclusive: false,
      toDOM(node) {
        const attrs: Record<string, string> = { href: node.attrs.href, rel: "noreferrer" };
        if (node.attrs.title) attrs.title = node.attrs.title;
        return ["a", attrs, 0];
      },
    },
  },
});

function blockSpec(tag: "p" | "h1"): NodeSpec {
  return {
    content: "inline*",
    group: "block",
    attrs: {
      prefix: { default: "" },
      rawSource: { default: "" },
      rawText: { default: "" },
      ...sourceRangeAttrs(),
    },
    toDOM: () => [tag, 0],
  };
}

function containerAttrs() {
  return {
    prefix: { default: "" },
    rawSource: { default: "" },
    rawText: { default: "" },
    ...sourceRangeAttrs(),
  };
}

function sourceRangeAttrs() {
  return {
    sourceFrom: { default: -1 },
    sourceTo: { default: -1 },
    prefixFrom: { default: -1 },
    prefixTo: { default: -1 },
  };
}

function containerBlockSpec(tag: "blockquote" | "ul" | "ol"): NodeSpec {
  return {
    group: "block",
    attrs: containerAttrs(),
    toDOM: () => [tag, 0],
  };
}
