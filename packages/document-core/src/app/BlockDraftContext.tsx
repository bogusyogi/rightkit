import { createContext, useContext, type ReactNode } from "react";
import type { BlockDraftRegistry } from "./blockDrafts";

const BlockDraftContext = createContext<BlockDraftRegistry | null>(null);

export function BlockDraftProvider({
  registry,
  children,
}: {
  registry: BlockDraftRegistry;
  children: ReactNode;
}) {
  return <BlockDraftContext.Provider value={registry}>{children}</BlockDraftContext.Provider>;
}

export function useBlockDrafts(): BlockDraftRegistry {
  const registry = useContext(BlockDraftContext);
  if (!registry) throw new Error("useBlockDrafts must be used within BlockDraftProvider");
  return registry;
}

export function useOptionalBlockDrafts(): BlockDraftRegistry | null {
  return useContext(BlockDraftContext);
}
