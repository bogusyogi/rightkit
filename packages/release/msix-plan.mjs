// Pure planning/verification helpers for MSIX packaging: signing order, runtime
// dependency inventory, publisher/signer match. No signing, no process spawns.

import { readFileSync, readdirSync } from "node:fs";
import path from "node:path";
import { readPeImports } from "./pe-imports.mjs";
import { WINDOWS_SYSTEM_DLLS } from "./swift-windows-stage.mjs";

const fail = (code, message) => Object.assign(new Error(message), { code });
const lower = (v) => String(v).toLowerCase();
const isApiSet = (name) => /^(api|ext)-ms-win-[a-z0-9-]+-l\d+-\d+-\d+\.dll$/.test(name);

/**
 * Immutable ordering (docs/rules/release-signing.md): inner PE files (DLLs, then EXEs) are signed
 * before packing, the package is signed after, verification follows, then seal. Returns step list.
 */
export function planMsixSigning({ files, sign = true, appInstaller = false }) {
  const pe = files.filter((f) => /\.(dll|exe)$/i.test(f));
  const rank = (f) => (/\.dll$/i.test(f) ? 0 : 1);
  const ordered = [...pe].sort((a, b) => rank(a) - rank(b) || a.localeCompare(b));
  return [
    { step: "assemble" },
    { step: "verify-inventory" },
    ...(sign ? [{ step: "sign-pe", files: ordered }] : []),
    { step: "pack" },
    ...(sign ? [{ step: "sign-msix" }, { step: "verify-msix" }, { step: "verify-publisher" }] : []),
    ...(appInstaller ? [{ step: "appinstaller" }] : []),
    { step: "seal" },
  ];
}

/**
 * Fail closed unless (a) every DLL in the declared inventory is in the payload and (b) every PE in the
 * payload imports only payload files, Windows system DLLs/API sets, or DLLs provided by a declared
 * package dependency. Equivalent to `dumpbin /dependents` closure, but host independent.
 */
export function verifyPayloadInventory({ root, inventory = {}, architecture }) {
  const names = readdirSync(root);
  const present = new Set(names.map(lower));
  const problems = [];
  for (const dll of inventory.dlls ?? []) if (!present.has(lower(dll))) problems.push(`declared runtime DLL missing from payload: ${dll}`);
  const system = new Set([...WINDOWS_SYSTEM_DLLS, ...(inventory.systemDlls ?? [])].map(lower));
  const provided = new Set((inventory.dependencies ?? []).flatMap((d) => d.provides ?? []).map(lower));
  const observed = new Set();
  for (const name of names.filter((n) => /\.(exe|dll)$/i.test(n))) {
    const info = readPeImports(readFileSync(path.join(root, name)));
    if (architecture && info.architecture !== architecture) problems.push(`${name} is ${info.architecture}, package is ${architecture}`);
    for (const dep of [...info.imports, ...info.delayImports]) {
      const key = lower(dep);
      observed.add(key);
      if (!present.has(key) && !system.has(key) && !isApiSet(key) && !provided.has(key)) problems.push(`${name} imports ${dep}, which is neither in the payload, a system DLL, nor provided by a declared dependency`);
    }
  }
  // Declared-but-never-imported DLLs are reported (not fatal): they may be loaded dynamically.
  const unreferenced = (inventory.dlls ?? []).filter((d) => !observed.has(lower(d)));
  if (problems.length) throw fail("MSIX_INVENTORY_INCOMPLETE", problems.join("; "));
  return { dlls: [...(inventory.dlls ?? [])], unreferenced };
}

const normalizeDn = (dn) => String(dn).split(/,(?=\s*[A-Za-z]+=)/).map((p) => p.trim().replace(/\s*=\s*/, "=")).sort().join(",");

/** The package signer's certificate subject must equal Identity/@Publisher or Windows refuses to install. */
export function assertPublisherMatchesSigner(publisher, signerSubject) {
  if (!signerSubject) throw fail("MSIX_PUBLISHER_MISMATCH", "could not read the signer certificate subject from the signed package");
  if (normalizeDn(publisher) !== normalizeDn(signerSubject)) {
    throw fail("MSIX_PUBLISHER_MISMATCH", `manifest Publisher "${publisher}" does not equal signer subject "${signerSubject}"; set identity.publisher to the signer subject`);
  }
}
