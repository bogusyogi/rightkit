// Renderer-side plugin contribution contracts, ported from Hermes IDE's
// packages/plugin-sdk/src/api.ts. The daemon owns installation and execution;
// these types describe the safe metadata a React host may project.

import type { ComponentType } from "react";

export interface Disposable {
  dispose(): void;
}

export interface PluginPanelProps {
  pluginId: string;
  panelId: string;
}

export type ActivationEvent =
  | { type: "onStartup" }
  | { type: "onCommand"; command: string }
  | { type: "onView"; viewId: string };

export interface PluginManifest {
  id: string;
  name: string;
  version: string;
  description: string;
  author: string;
  activationEvents: ActivationEvent[];
  contributes: PluginContributions;
  permissions?: PluginPermission[];
}

export interface PluginContributions {
  commands?: PluginCommandContribution[];
  panels?: PluginPanelContribution[];
  statusBarItems?: PluginStatusBarItem[];
  sessionActions?: PluginSessionActionContribution[];
  settings?: PluginSettingsSchema;
}

export interface PluginSessionActionContribution {
  id: string;
  panelId: string;
  name: string;
  icon: string;
}

export interface PluginCommandContribution {
  command: string;
  title: string;
  category?: string;
  keybinding?: string;
}

export interface PluginPanelContribution {
  id: string;
  name: string;
  side: "left" | "right";
  icon: string;
}

export interface PluginStatusBarItem {
  id: string;
  text: string;
  tooltip?: string;
  alignment: "left" | "right";
  priority?: number;
  command?: string;
}

export type PluginPermission =
  | "clipboard.read"
  | "clipboard.write"
  | "storage"
  | "terminal.read"
  | "terminal.write"
  | "sessions.read"
  | "notifications"
  | "network"
  | "shell.exec";

export interface PluginSettingsSchema {
  [key: string]: PluginSettingDefinition;
}

export type PluginSettingDefinition =
  | PluginSettingString
  | PluginSettingNumber
  | PluginSettingBoolean
  | PluginSettingSelect;

interface PluginSettingBase {
  title: string;
  description?: string;
  order?: number;
}

export interface PluginSettingString extends PluginSettingBase {
  type: "string";
  default: string;
  placeholder?: string;
  maxLength?: number;
}

export interface PluginSettingNumber extends PluginSettingBase {
  type: "number";
  default: number;
  min?: number;
  max?: number;
  step?: number;
}

export interface PluginSettingBoolean extends PluginSettingBase {
  type: "boolean";
  default: boolean;
}

export interface PluginSettingSelect extends PluginSettingBase {
  type: "select";
  default: string;
  options: { value: string; label: string }[];
}

/** A panel component supplied by an activated plugin module. */
export type PluginPanelComponent = ComponentType<PluginPanelProps>;

/** Permission values are intentionally closed: unknown declarations fail closed. */
export const PLUGIN_PERMISSIONS: readonly PluginPermission[] = [
  "clipboard.read",
  "clipboard.write",
  "storage",
  "terminal.read",
  "terminal.write",
  "sessions.read",
  "notifications",
  "network",
  "shell.exec",
] as const;

export function isPluginPermission(value: string): value is PluginPermission {
  return (PLUGIN_PERMISSIONS as readonly string[]).includes(value);
}
