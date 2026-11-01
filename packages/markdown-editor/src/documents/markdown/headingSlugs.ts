import { createContext, type Context } from "react";
import type { buildHeadingSlugs } from "@rightkit/doc-render";

export { buildHeadingSlugs } from "@rightkit/doc-render";
export type HeadingSlugMap = ReturnType<typeof buildHeadingSlugs>;

export const HeadingSlugContext: Context<HeadingSlugMap | null> = createContext<HeadingSlugMap | null>(null);
