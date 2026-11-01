/**
 * Deterministic Markdown corpus used by M5 performance and dogfood checks.
 *
 * The corpus is generated instead of checked in as three large blobs so tests
 * keep one source of truth while still exercising realistic Markdown syntax.
 */

export const MARKDOWN_PERFORMANCE_SIZES = [10_000, 50_000, 250_000] as const;

export type MarkdownPerformanceSize = (typeof MARKDOWN_PERFORMANCE_SIZES)[number];

const FRONTMATTER = `---
title: Markdown performance fixture
status: stable
tags:
  - performance
  - dogfood
---

`;

function renderSection(index: number): string {
  return `## Section ${index}: local-first editing

This paragraph repeats a realistic **marked** sentence with an [editable link](https://example.com/docs/${index}?q=space%20value) and an image reference (![diagram ${index}](assets/diagram-${index}.png "Diagram ${index}")).

- checklist item ${index}.1
- checklist item ${index}.2 with *emphasis* and ~~deleted~~ text
  - nested item ${index}.3

1. ordered item ${index}.1
2. ordered item ${index}.2

> A blockquote keeps source ranges meaningful while a user types near a protected block.

| Name | Value | Notes |
| :--- | ---: | :--- |
| row ${index}.1 | ${index} | escaped pipe \\| stays in one cell |
| row ${index}.2 | ${index + 1} | [linked cell](#section-${index}-local-first-editing) |

\`\`\`ts
export function fixture${index}(input: string): string {
  return input.trim();
}
\`\`\`

<!-- protected fixture block ${index} -->

---

`;
}

/** Create an exact-size, syntax-rich logical Markdown source. */
export function createMarkdownPerformanceFixture(size: MarkdownPerformanceSize): string {
  let source = FRONTMATTER;
  let index = 0;
  while (source.length < size) {
    source += renderSection(index);
    index += 1;
  }
  return source.slice(0, size);
}
