// Guard legacy app-side WebDriver (WDIO) features even though this package no longer drives them.
// Named legacy on purpose: the current native QA path is the in-app control server, not WDIO.
export interface LegacyWdioQaContractInput {
  cargoToml: string;
  rustSource: string;
  packageScripts: Record<string, string>;
}

/** @deprecated Legacy name for {@link LegacyWdioQaContractInput}; kept for existing callers. */
export type NativeQaContractInput = LegacyWdioQaContractInput;

export function validateLegacyWdioQaContract(input: LegacyWdioQaContractInput): string[] {
  const issues: string[] = [];
  const dependency = input.cargoToml.match(/tauri-plugin-wdio-webdriver\s*=\s*\{([^}]*)\}/s)?.[1] ?? '';
  if (!dependency) {
    issues.push('Cargo.toml must declare tauri-plugin-wdio-webdriver');
  } else if (!/optional\s*=\s*true/.test(dependency)) {
    issues.push('tauri-plugin-wdio-webdriver must be optional');
  }

  const qaFeature = input.cargoToml.match(/qa-native\s*=\s*\[([^\]]*)\]/s)?.[1] ?? '';
  if (!/dep:tauri-plugin-wdio-webdriver/.test(qaFeature)) {
    issues.push('qa-native must enable dep:tauri-plugin-wdio-webdriver');
  }

  const compactRust = input.rustSource.replace(/\s+/g, ' ');
  if (!/cfg\s*\(\s*all\s*\(\s*not\s*\(\s*debug_assertions\s*\)\s*,\s*feature\s*=\s*"qa-native"\s*\)\s*\)/.test(compactRust)
      || !/compile_error!/.test(compactRust)) {
    issues.push('Rust must compile_error when qa-native is enabled outside debug builds');
  }
  if (!/cfg\s*\(\s*all\s*\(\s*debug_assertions\s*,\s*feature\s*=\s*"qa-native"\s*\)\s*\)/.test(compactRust)
      || !/tauri_plugin_wdio_webdriver::init/.test(compactRust)) {
    issues.push('WebDriver plugin registration must require debug_assertions and qa-native');
  }
  if (!/RIGHTKIT_QA_NATIVE/.test(compactRust)) {
    issues.push('WebDriver plugin registration must also require the RIGHTKIT_QA_NATIVE runtime flag');
  }

  for (const [name, command] of Object.entries(input.packageScripts)) {
    if (!/qa-native/.test(command)) continue;
    if (/^(qa|test|dev)(:|$)/.test(name)) continue;
    if (/(release|publish|build|dmg|installer|package)/i.test(name) || /\btauri\s+build\b/.test(command)) {
      issues.push(`package script ${name} must not enable qa-native`);
    }
  }
  return issues;
}

/** @deprecated Use {@link validateLegacyWdioQaContract}; this legacy WDIO guard keeps its old name as an alias. */
export function validateNativeQaContract(input: LegacyWdioQaContractInput): string[] {
  return validateLegacyWdioQaContract(input);
}

export function assertLegacyWdioQaContract(input: LegacyWdioQaContractInput): void {
  const issues = validateLegacyWdioQaContract(input);
  if (issues.length > 0) throw new Error(`legacy WDIO QA contract failed:\n- ${issues.join('\n- ')}`);
}

/** @deprecated Use {@link assertLegacyWdioQaContract}; kept for existing callers. */
export function assertNativeQaContract(input: LegacyWdioQaContractInput): void {
  assertLegacyWdioQaContract(input);
}
