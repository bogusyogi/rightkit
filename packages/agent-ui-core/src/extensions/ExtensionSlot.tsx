import { useEffect, useState, type ReactNode } from "react";
import { useExtensionRegistry } from "./extensionRegistry.js";

export interface ExtensionSlotProps {
  /** Semantic slot/component key registered by an extension. */
  name: string;
  data?: Record<string, unknown>;
  fallback?: ReactNode;
  className?: string;
  /** Render a declarative snapshot when no React component is registered. */
  renderTemplate?: (template: unknown, name: string) => ReactNode;
}

/**
 * Resolves one constrained the host app UI slot from the shared registry. The
 * subscription mirrors Aethon's nested-renderer behavior and makes live
 * registration/replacement visible without prop drilling.
 */
export function ExtensionSlot({
  name,
  data,
  fallback = null,
  className = "",
  renderTemplate,
}: ExtensionSlotProps) {
  const registry = useExtensionRegistry();
  const [, setRevision] = useState(0);

  useEffect(() => registry.subscribe(() => setRevision((value) => value + 1)), [registry]);

  const Component = registry.resolve(name);
  const content = Component ? (
    <Component extensionName={registry.ownerOf(name) ?? name} slot={name} data={data} />
  ) : (() => {
    const template = registry.resolveTemplate(name);
    return template !== undefined && renderTemplate
      ? renderTemplate(template, name)
      : fallback;
  })();

  return (
    <div className={className} data-extension-slot={name}>
      {content}
    </div>
  );
}

export default ExtensionSlot;
