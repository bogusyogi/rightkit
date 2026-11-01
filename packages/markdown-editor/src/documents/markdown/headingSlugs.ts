import { createContext } from "react";
import type { buildHeadingSlugs } from "@rightkit/doc-render";

export { buildHeadingSlugs } from "@rightkit/doc-render";
export type HeadingSlugMap = ReturnType<typeof buildHeadingSlugs>;

export const HeadingSlugContext = createContext<HeadingSlugMap | null>(null);
