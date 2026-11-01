import { useState, type ReactNode } from "react";
import { ExtensionRegistryContext } from "./extensionRegistry.js";
import type { ExtensionRegistry } from "./extensionRegistry.js";

/**
 * Shares one registry instance with all descendant ExtensionSlot components.
 * The instance is pinned per mount, matching Aethon's provider contract and
 * preventing context consumers from changing registry identity on re-render.
 */
export function ExtensionRegistryProvider({
  registry,
  children,
}: {
  registry: ExtensionRegistry;
  children: ReactNode;
}) {
  const [pinned] = useState(() => registry);
  return (
    <ExtensionRegistryContext.Provider value={pinned}>
      {children}
    </ExtensionRegistryContext.Provider>
  );
}
