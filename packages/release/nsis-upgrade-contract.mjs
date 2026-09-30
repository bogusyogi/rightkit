import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

export function assertNsisInPlaceUpgradeContract(appRoot, options = {}) {
  const baseConfigPath = path.resolve(appRoot, options.tauriConfig ?? "src-tauri/tauri.conf.json");
  const windowsConfigPath = path.resolve(
    appRoot,
    options.windowsTauriConfig ?? "src-tauri/tauri.windows.conf.json",
  );
  const base = readJson(baseConfigPath, "base Tauri config");
  const windows = existsSync(windowsConfigPath)
    ? readJson(windowsConfigPath, "Windows Tauri config")
    : {};
  const template =
    windows.bundle?.windows?.nsis?.template ?? base.bundle?.windows?.nsis?.template;
  if (typeof template !== "string" || !template.trim()) {
    throw new Error(
      "Windows releases require a custom NSIS template with the RightKit automatic in-place upgrade contract; Tauri's stock maintenance choice page is not allowed",
    );
  }

  const declaringConfig = windows.bundle?.windows?.nsis?.template
    ? windowsConfigPath
    : baseConfigPath;
  const templatePath = path.resolve(path.dirname(declaringConfig), template);
  if (!within(appRoot, templatePath) || !existsSync(templatePath)) {
    throw new Error(`Windows NSIS template is missing or outside the app root: ${template}`);
  }

  const source = readFileSync(templatePath, "utf8");
  if (!/!define\s+RIGHTKIT_AUTOMATIC_IN_PLACE_UPGRADE\b/.test(source)) {
    throw new Error(
      "Windows NSIS template must declare the RightKit automatic in-place upgrade policy; uninstall/reinstall maintenance choices add unnecessary friction",
    );
  }
  // Two acceptable shapes:
  //  - legacy: Tauri's reinstall page is present but skipped for same-version
  //    repair and in-place upgrades (the $WixMode / $R0 skip logic);
  //  - no maintenance surface at all: the template declares no reinstall page,
  //    no components page and no custom page, so there is nothing to skip.
  const legacySkipLogic =
    /\$\{If\}\s+\$WixMode\s*=\s*0[\s\S]{0,160}\$\{AndIf\}\s+\$R0\s*>=\s*0[\s\S]{0,80}\bAbort\b/.test(
      source,
    );
  const maintenanceSurface = /\bPageReinstall\b|MUI_PAGE_COMPONENTS|^\s*Page\s+custom\b/m.test(source);
  if (!legacySkipLogic && maintenanceSurface) {
    throw new Error(
      "Windows NSIS template has a reinstall or maintenance page without skip-page logic for same-version repair and in-place upgrades",
    );
  }
  return { templatePath, shape: legacySkipLogic ? "legacy-skip-page" : "no-maintenance-page" };
}

function readJson(file, label) {
  if (!existsSync(file)) throw new Error(`missing ${label}: ${file}`);
  try {
    return JSON.parse(readFileSync(file, "utf8"));
  } catch (error) {
    throw new Error(`invalid ${label}: ${file} (${String(error)})`);
  }
}

function within(root, candidate) {
  const relative = path.relative(path.resolve(root), path.resolve(candidate));
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}
