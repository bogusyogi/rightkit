// Guard legacy app-side WebDriver features even though this package no longer drives them.
export interface NativeQaContractInput {
  cargoToml: string;
  rustSource: string;
  packageScripts: Record<string, string>;
}

export function validateNativeQaContract(input: NativeQaContractInput): string[] {
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

export function assertNativeQaContract(input: NativeQaContractInput): void {
  const issues = validateNativeQaContract(input);
  if (issues.length > 0) throw new Error(`native QA contract failed:\n- ${issues.join('\n- ')}`);
}
