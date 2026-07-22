import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { assertNsisInPlaceUpgradeContract } from "./nsis-upgrade-contract.mjs";

function fixture(template) {
  const root = mkdtempSync(path.join(os.tmpdir(), "right-nsis-upgrade-"));
  const tauriDir = path.join(root, "src-tauri");
  const windowsDir = path.join(tauriDir, "windows");
  mkdirSync(windowsDir, { recursive: true });
  writeFileSync(
    path.join(tauriDir, "tauri.conf.json"),
    `${JSON.stringify({ bundle: { windows: { nsis: { template: "windows/installer.nsi" } } } })}\n`,
  );
  writeFileSync(path.join(windowsDir, "installer.nsi"), template);
  return root;
}

test("rejects an NSIS template that exposes uninstall-or-reinstall maintenance choices", () => {
  const root = fixture(`
    Page custom PageReinstall PageLeaveReinstall
    Function PageReinstall
      nsDialogs::Show
    FunctionEnd
  `);
  assert.throws(
    () => assertNsisInPlaceUpgradeContract(root),
    /automatic in-place upgrade|maintenance choice/i,
  );
});

test("accepts a template that skips maintenance UI for same-version repair and upgrades", () => {
  const root = fixture(`
    !define RIGHTKIT_AUTOMATIC_IN_PLACE_UPGRADE
    Function PageReinstall
      $\{If\} $WixMode = 0
      $\{AndIf\} $R0 >= 0
        Abort
      $\{EndIf\}
      nsDialogs::Show
    FunctionEnd
  `);
  const result = assertNsisInPlaceUpgradeContract(root);
  assert.match(result.templatePath, /installer\.nsi$/);
});

test("rejects a marker without executable skip-page logic", () => {
  const root = fixture(`
    !define RIGHTKIT_AUTOMATIC_IN_PLACE_UPGRADE
    Function PageReinstall
      nsDialogs::Show
    FunctionEnd
  `);
  assert.throws(() => assertNsisInPlaceUpgradeContract(root), /skip.*logic/i);
});
