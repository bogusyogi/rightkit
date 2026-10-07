import { aboutSection, apiKeysSection, appearanceSection, diagnosticsSection, legalSection, licenseSection, modelsSection, updatesSection } from "./sections.js";
import type { AboutOptions, ApiKeysOptions, AppearanceOptions, DiagnosticsOptions, LegalOptions, LicenseOptions, ModelsOptions, UpdatesOptions } from "./sections.js";
import type { SettingsSection } from "./types.js";

/** Ids of the built-in sections, as used by `include`, `omit` and `override`. */
export type StandardSectionId = "appearance" | "updates" | "models" | "license" | "legal" | "api-keys" | "diagnostics" | "about";

export interface StandardSectionsConfig {
  /** Omit (or `false`) to leave the About section out. */
  about?: AboutOptions | false;
  appearance?: AppearanceOptions | false;
  updates?: UpdatesOptions | false;
  license?: LicenseOptions | false;
  legal?: LegalOptions | false;
  diagnostics?: DiagnosticsOptions | false;
  models?: ModelsOptions | false;
  apiKeys?: ApiKeysOptions | false;
  /** Keep only these built-in ids (after the per-section config above is applied). */
  include?: readonly StandardSectionId[];
  /** Drop these built-in ids. */
  omit?: readonly StandardSectionId[];
  /** Shallow-merge fields (label, badge, heading, icon, group, order, render, ...) over a built-in section. */
  override?: Partial<Record<StandardSectionId, Partial<SettingsSection>>>;
}

/**
 * The standard section set. A section whose config is omitted or `false` is left out (an app without models has no Models
 * section); appearance defaults on. `include`, `omit` and `override` select and adjust built-ins by id without replacing
 * the list. Pair with `mergeSections` to add app sections.
 */
export function standardSections(config: StandardSectionsConfig = {}): SettingsSection[] {
  const out: SettingsSection[] = [];
  if (config.appearance !== false) out.push(appearanceSection(config.appearance ?? {}));
  if (config.updates) out.push(updatesSection(config.updates));
  if (config.models) out.push(modelsSection(config.models));
  if (config.license) out.push(licenseSection(config.license));
  if (config.legal) out.push(legalSection(config.legal));
  if (config.apiKeys) out.push(apiKeysSection(config.apiKeys));
  if (config.diagnostics) out.push(diagnosticsSection(config.diagnostics));
  if (config.about) out.push(aboutSection(config.about));
  const include = config.include ? new Set<string>(config.include) : null;
  const omit = new Set<string>(config.omit ?? []);
  return out
    .filter((s) => (!include || include.has(s.id)) && !omit.has(s.id))
    .map((s) => {
      const o = (config.override as Record<string, Partial<SettingsSection> | undefined> | undefined)?.[s.id];
      return o ? { ...s, ...o, id: s.id } : s;
    });
}
