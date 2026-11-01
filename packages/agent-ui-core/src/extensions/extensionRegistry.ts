/**
 * ExtensionRegistry — runtime registry for renderer extensions.
 *
 * Ported from Aethon's `src/extensions/ExtensionRegistry.ts`: extensions are
 * named bundles, components resolve by semantic key, and template/layout
 * snapshots may be replaced during hot reload. the host app keeps the registry
 * renderer-only; daemon execution and protocol ownership remain elsewhere.
 */

import { createContext, useContext } from "react";
import type { ComponentType } from "react";

export interface ExtensionComponentProps {
  extensionName: string;
  slot: string;
  data?: Record<string, unknown>;
}

export type ExtensionComponent = ComponentType<ExtensionComponentProps>;
export type ExtensionLayout = Record<string, unknown>;

export interface ExtensionDefinition {
  name: string;
  components?: Record<string, ExtensionComponent>;
  layout?: ExtensionLayout;
}

export class ExtensionRegistry {
  private readonly extensions = new Map<string, ExtensionDefinition>();
  private readonly components = new Map<string, ExtensionComponent>();
  private readonly componentOwners = new Map<string, string>();
  private readonly templates = new Map<string, unknown>();
  private readonly listeners = new Set<() => void>();

  register(extension: ExtensionDefinition): void {
    if (!extension || typeof extension.name !== "string" || extension.name.length === 0) {
      throw new Error("ExtensionRegistry.register requires a non-empty extension name");
    }

    // Re-registration is replacement, which is the hot-reload behavior. Remove
    // only the prior owner's bindings so another extension's collision survives.
    const previous = this.extensions.get(extension.name);
    if (previous?.components) {
      for (const [slot, component] of Object.entries(previous.components)) {
        if (this.components.get(slot) === component) {
          this.components.delete(slot);
          this.componentOwners.delete(slot);
        }
      }
    }
    this.extensions.set(extension.name, extension);
    for (const [slot, component] of Object.entries(extension.components ?? {})) {
      this.components.set(slot, component);
      this.componentOwners.set(slot, extension.name);
    }
    this.notify();
  }

  unregister(name: string): void {
    const extension = this.extensions.get(name);
    if (!extension) return;
    for (const [slot, component] of Object.entries(extension.components ?? {})) {
      if (this.components.get(slot) === component) {
        this.components.delete(slot);
        this.componentOwners.delete(slot);
      }
    }
    this.extensions.delete(name);
    this.notify();
  }

  resolve(slot: string): ExtensionComponent | undefined {
    return this.components.get(slot);
  }

  resolveTemplate(slot: string): unknown | undefined {
    return this.templates.get(slot);
  }

  ownerOf(slot: string): string | undefined {
    return this.componentOwners.get(slot);
  }

  /** Replace the declarative extension snapshot wholesale. */
  setTemplates(templates: Record<string, unknown>): void {
    this.templates.clear();
    for (const [slot, template] of Object.entries(templates)) {
      this.templates.set(slot, template);
    }
    this.notify();
  }

  /** Subscribe to component/template changes for slot re-rendering. */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  // A named alias makes the intent clear at call sites handling template deltas.
  onTemplatesChanged(listener: () => void): () => void {
    return this.subscribe(listener);
  }

  list(): ExtensionDefinition[] {
    return [...this.extensions.values()];
  }

  /** The most recently registered extension layout, if one was supplied. */
  primaryLayout(): ExtensionLayout | undefined {
    let last: ExtensionLayout | undefined;
    for (const extension of this.extensions.values()) {
      if (extension.layout) last = extension.layout;
    }
    return last;
  }

  private notify(): void {
    for (const listener of this.listeners) listener();
  }
}

export const ExtensionRegistryContext = createContext<ExtensionRegistry | null>(null);

export function useExtensionRegistry(): ExtensionRegistry {
  const registry = useContext(ExtensionRegistryContext);
  if (!registry) {
    throw new Error("useExtensionRegistry called outside ExtensionRegistryProvider");
  }
  return registry;
}
