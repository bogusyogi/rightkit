#!/usr/bin/env node
// The default NSIS in-place-upgrade template (rightkit#753): `right-release nsis-template --from <Tauri stock installer.nsi> --out <file>`.
//
// nsis-upgrade-contract.mjs refuses a Windows release whose NSIS template has no automatic in-place upgrade. Every app used to copy the
// same two edits into its own copy of Tauri's stock `installer.nsi`. This derives that template from the stock file instead, with
// exactly those two edits, and fails closed (writes nothing) when the stock file no longer has the text the edits anchor on:
//   1. `!define RIGHTKIT_AUTOMATIC_IN_PLACE_UPGRADE` ahead of the first `!include MUI2.nsh`;
//   2. in `Function PageReinstall`, right after `nsis_tauri_utils::SemverCompare "${VERSION}" $R0` / `Pop $R0`, the skip block that aborts the
//      reinstall page for a same-version repair and an in-place upgrade (the `$WixMode = 0` and `$R0 >= 0` shape the contract accepts).
// The derived text is then judged by the contract's own regular expressions (`templateContractShape`, the same two the contract uses), so a
// template that does not satisfy it is never written.
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";

export const IN_PLACE_DEFINE = "!define RIGHTKIT_AUTOMATIC_IN_PLACE_UPGRADE";
const SKIP_BLOCK = [
  "  ; RightKit policy: repairs and upgrades replace in place without uninstall/reinstall UI.",
  "  ${If} $WixMode = 0",
  "  ${AndIf} $R0 >= 0",
  "    Abort",
  "  ${EndIf}",
];

export class NsisTemplateError extends Error {
  constructor(message) { super(message); this.name = "NsisTemplateError"; this.code = "RK_NSIS_TEMPLATE"; }
}

/** `legacy-skip-page`, `no-maintenance-page`, or null when the template does not satisfy the in-place upgrade contract. */
export function templateContractShape(source) {
  if (!/!define\s+RIGHTKIT_AUTOMATIC_IN_PLACE_UPGRADE\b/.test(source)) return null;
  const legacySkipLogic = /\$\{If\}\s+\$WixMode\s*=\s*0[\s\S]{0,160}\$\{AndIf\}\s+\$R0\s*>=\s*0[\s\S]{0,80}\bAbort\b/.test(source);
  const maintenanceSurface = /\bPageReinstall\b|MUI_PAGE_COMPONENTS|^\s*Page\s+custom\b/m.test(source);
  if (!legacySkipLogic && maintenanceSurface) return null;
  return legacySkipLogic ? "legacy-skip-page" : "no-maintenance-page";
}

/** Derive the in-place-upgrade template from a stock Tauri `installer.nsi`. Throws NsisTemplateError (and returns nothing) on any anchor that is not found exactly once. */
export function deriveInPlaceUpgradeTemplate(stock) {
  if (typeof stock !== "string" || !stock.trim()) throw new NsisTemplateError("the stock installer.nsi is empty");
  if (templateContractShape(stock)) throw new NsisTemplateError("the template already satisfies the in-place upgrade contract; nothing to derive");
  const eol = stock.includes("\r\n") ? "\r\n" : "\n";
  const lines = stock.split(/\r?\n/);
  const include = lines.flatMap((line, index) => (/^\s*!include\s+MUI2\.nsh\s*$/.test(line) ? [index] : []));
  if (include.length !== 1) throw new NsisTemplateError(`expected exactly one \`!include MUI2.nsh\` line to anchor the define, found ${include.length}`);
  const pageStart = lines.flatMap((line, index) => (/^\s*Function\s+PageReinstall\s*$/.test(line) ? [index] : []));
  if (pageStart.length !== 1) throw new NsisTemplateError(`expected exactly one \`Function PageReinstall\`, found ${pageStart.length}`);
  const compare = lines.flatMap((line, index) => (index > pageStart[0] && /^\s*nsis_tauri_utils::SemverCompare\s+"\$\{VERSION\}"\s+\$R0\s*$/.test(line) ? [index] : []));
  if (!compare.length || !/^\s*Pop\s+\$R0\s*$/.test(lines[compare[0] + 1] ?? "")) throw new NsisTemplateError("PageReinstall has no `SemverCompare \"${VERSION}\" $R0` followed by `Pop $R0` to anchor the skip block");
  const pageEnd = lines.findIndex((line, index) => index > pageStart[0] && /^\s*FunctionEnd\s*$/.test(line));
  if (pageEnd < 0 || compare[0] > pageEnd) throw new NsisTemplateError("the SemverCompare anchor is not inside Function PageReinstall");
  const out = [...lines];
  out.splice(compare[0] + 2, 0, ...SKIP_BLOCK); // after Pop $R0 (the later index first, so the define's index stays valid)
  out.splice(include[0], 0, IN_PLACE_DEFINE, "");
  const derived = out.join(eol);
  if (templateContractShape(derived) !== "legacy-skip-page") throw new NsisTemplateError("the derived template does not satisfy the in-place upgrade contract; nothing written");
  return derived;
}

/** Read `from`, derive, and write `out` (atomically, never over the stock file). Returns { out, shape }. */
export function writeInPlaceUpgradeTemplate({ from, out, fs = {} }) {
  const exists = fs.existsSync ?? existsSync;
  const read = fs.readFileSync ?? readFileSync;
  const write = fs.writeFileSync ?? writeFileSync;
  const rename = fs.renameSync ?? renameSync;
  const makeDir = fs.mkdirSync ?? mkdirSync;
  if (!exists(from)) throw new NsisTemplateError(`stock template not found: ${from}`);
  if (path.resolve(from) === path.resolve(out)) throw new NsisTemplateError("--out must not be the stock template");
  const derived = deriveInPlaceUpgradeTemplate(String(read(from, "utf8")));
  makeDir(path.dirname(path.resolve(out)), { recursive: true });
  const temporary = `${out}.tmp-${process.pid}`;
  write(temporary, derived);
  rename(temporary, out);
  return { out, shape: templateContractShape(derived) };
}

export function main(argv = process.argv.slice(2)) {
  let from; let out;
  const usage = "usage: right-release nsis-template --from <Tauri stock installer.nsi> --out <template.nsi>";
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--from" && argv[i + 1]) { from = argv[i + 1]; i += 1; }
    else if (argv[i] === "--out" && argv[i + 1]) { out = argv[i + 1]; i += 1; }
    else { console.error(`right-release nsis-template: unknown or incomplete argument ${argv[i]}\n${usage}`); return 2; }
  }
  if (!from || !out) { console.error(usage); return 2; }
  try {
    const result = writeInPlaceUpgradeTemplate({ from: path.resolve(from), out: path.resolve(out) });
    console.log(`right-release nsis-template: wrote ${result.out} (${result.shape}); declare it as bundle.windows.nsis.template`);
    return 0;
  } catch (error) { console.error(`right-release nsis-template: ${error.message}`); return 1; }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) process.exitCode = main();
