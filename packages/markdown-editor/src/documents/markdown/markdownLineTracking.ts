/** Returns the first source block that reaches the preview's reading line. */
export function firstVisibleMarkdownBlock(
  blocks: readonly HTMLElement[],
  top: number,
): HTMLElement | undefined {
  return blocks.find((block) => block.getBoundingClientRect().bottom >= top) ?? blocks[0];
}
