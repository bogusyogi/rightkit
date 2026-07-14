import { existsSync, readFileSync, readdirSync } from "node:fs";
import path from "node:path";

export function validateQaBackdoorContract(root, packageScripts = {}) {
  if (!existsSync(path.join(root, "right-qa.config.mjs"))) return [];

  const issues = [];
  const cargoPath = path.join(root, "src-tauri", "Cargo.toml");
  if (!existsSync(cargoPath)) return ["right-qa.config.mjs requires src-tauri/Cargo.toml"];

  const cargoToml = readFileSync(cargoPath, "utf8");
  const dependency = cargoToml.match(/tauri-plugin-wdio-webdriver\s*=\s*\{([^}]*)\}/s)?.[1] ?? "";
  if (!dependency) issues.push("Cargo.toml must declare tauri-plugin-wdio-webdriver");
  else if (!/optional\s*=\s*true/.test(dependency)) issues.push("tauri-plugin-wdio-webdriver must be optional");

  const qaFeature = cargoToml.match(/qa-native\s*=\s*\[([^\]]*)\]/s)?.[1] ?? "";
  if (!/dep:tauri-plugin-wdio-webdriver/.test(qaFeature)) {
    issues.push("qa-native must enable dep:tauri-plugin-wdio-webdriver");
  }

  const rustSource = readRustSources(path.join(root, "src-tauri", "src")).replace(/\s+/g, " ");
  if (!/cfg\s*\(\s*all\s*\(\s*not\s*\(\s*debug_assertions\s*\)\s*,\s*feature\s*=\s*"qa-native"\s*\)\s*\)/.test(rustSource)
      || !/compile_error!/.test(rustSource)) {
    issues.push("Rust must compile_error when qa-native is enabled outside debug builds");
  }
  if (!/cfg\s*\(\s*all\s*\(\s*debug_assertions\s*,\s*feature\s*=\s*"qa-native"\s*\)\s*\)/.test(rustSource)
      || !/tauri_plugin_wdio_webdriver::init/.test(rustSource)) {
    issues.push("WebDriver registration must require debug_assertions and qa-native");
  }
  if (!/RIGHTKIT_QA_NATIVE/.test(rustSource)) {
    issues.push("WebDriver registration must also require RIGHTKIT_QA_NATIVE");
  }

  for (const [name, command] of Object.entries(packageScripts)) {
    if (!/qa-native|RIGHTKIT_QA_NATIVE/.test(command)) continue;
    if (/^(qa|test|dev)(:|$)/.test(name)) continue;
    if (/(release|publish|build|dmg|installer|package)/i.test(name) || /\btauri\s+build\b/.test(command)) {
      issues.push(`package script ${name} must not enable native QA`);
    }
  }
  return issues;
}

export function assertQaBackdoorContract(root, packageScripts = {}) {
  const issues = validateQaBackdoorContract(root, packageScripts);
  if (issues.length) throw new Error(`production QA backdoor contract failed:\n- ${issues.join("\n- ")}`);
}

function readRustSources(root) {
  if (!existsSync(root)) return "";
  const files = [];
  const visit = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const absolute = path.join(dir, entry.name);
      if (entry.isDirectory()) visit(absolute);
      else if (entry.isFile() && entry.name.endsWith(".rs")) files.push(absolute);
    }
  };
  visit(root);
  return files.sort().map((file) => readFileSync(file, "utf8")).join("\n");
}
