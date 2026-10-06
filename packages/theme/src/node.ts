import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { parseBrand, type BrandFile } from "./brand.js";
import { checkBrandContrast, checkGeneratedCss, scanHardcodedColors, type ContrastReport, type ScanOptions, type TokenFinding } from "./check.js";
import { generateCss, type CssFormat } from "./css.js";

export function readBrand(path: string): BrandFile {
  return parseBrand(JSON.parse(readFileSync(path, "utf8")));
}

/** GenRight's logo invariant: the master SVG must carry the approved brand colour as a fill. */
export function checkLogo(brandPath: string, brand: BrandFile): string[] {
  const master = brand.logo?.master;
  if (!master) return [];
  const file = resolve(dirname(brandPath), "..", "..", master);
  const candidates = [resolve(dirname(brandPath), master), file];
  const found = candidates.find((p) => existsSync(p));
  if (!found) return [`logo master not found: ${master}`];
  const svg = readFileSync(found, "utf8").toLowerCase();
  return svg.includes(`fill="${brand.colors.brand.toLowerCase()}"`) ? [] : ["logo colour differs from the approved brand colour"];
}

export interface GenerateResult {
  path: string;
  changed: boolean;
  css: string;
}

/** Write the stylesheet only when bytes differ (stable mtimes for watchers). With `check`, never writes. */
export function generateFile(brandPath: string, outPath: string, options: { format?: CssFormat; check?: boolean } = {}): GenerateResult {
  const brand = readBrand(brandPath);
  const css = generateCss(brand, { format: options.format, source: brandPath.split(/[\\/]/).slice(-3).join("/") });
  const existing = existsSync(outPath) ? readFileSync(outPath, "utf8") : "";
  const changed = existing !== css;
  if (changed && !options.check) {
    mkdirSync(dirname(outPath), { recursive: true });
    writeFileSync(outPath, css);
  }
  return { path: outPath, changed, css };
}

export function checkBrandFile(brandPath: string): { brand: BrandFile; report: ContrastReport; logo: string[] } {
  const brand = readBrand(brandPath);
  return { brand, report: checkBrandContrast(brand), logo: checkLogo(brandPath, brand) };
}

export function scanFiles(files: string[], options?: ScanOptions): TokenFinding[] {
  return files.flatMap((f) => scanHardcodedColors(f, readFileSync(f, "utf8"), options));
}

export { checkGeneratedCss };
