import { aboutSection, apiKeysSection, appearanceSection, diagnosticsSection, legalSection, licenseSection, modelsSection, updatesSection } from "./sections.js";
import type { AboutOptions, ApiKeysOptions, AppearanceOptions, DiagnosticsOptions, LegalOptions, LicenseOptions, ModelsOptions, UpdatesOptions } from "./sections.js";
import type { SettingsSection } from "./types.js";

export interface StandardSectionsConfig {
  /** Always present. */
  about: AboutOptions;
  appearance?: AppearanceOptions | false;
  updates?: UpdatesOptions | false;
  license?: LicenseOptions | false;
  legal?: LegalOptions | false;
  diagnostics?: DiagnosticsOptions | false;
  models?: ModelsOptions | false;
  apiKeys?: ApiKeysOptions | false;
}

/**
 * The standard section set. A section whose config is omitted or `false` is left out (an app without models has no Models
 * section); appearance defaults on. Pair with `mergeSections` to add app sections.
 */
export function standardSections(config: StandardSectionsConfig): SettingsSection[] {
  const out: SettingsSection[] = [];
  if (config.appearance !== false) out.push(appearanceSection(config.appearance ?? {}));
  if (config.updates) out.push(updatesSection(config.updates));
  if (config.models) out.push(modelsSection(config.models));
  if (config.license) out.push(licenseSection(config.license));
  if (config.legal) out.push(legalSection(config.legal));
  if (config.apiKeys) out.push(apiKeysSection(config.apiKeys));
  if (config.diagnostics) out.push(diagnosticsSection(config.diagnostics));
  out.push(aboutSection(config.about));
  return out;
}
