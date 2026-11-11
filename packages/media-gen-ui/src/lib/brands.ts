import type { BrandInfo } from './gallery-filters.js';

// The host's brands (codes, names and descriptors) for the brand pickers and gallery labels. The
// package ships none; hosts call setHostBrands() once at startup, before rendering, with the same
// catalogue they register with the engine (rightkit-media-gen `gallery::set_brand_catalog` and
// rightkit-prompt-recipes `brands::set_brands`).
let current: BrandInfo[] = [];

export function setHostBrands(brands: BrandInfo[]): void {
  current = brands.map((b) => ({ ...b }));
}

export function hostBrands(): BrandInfo[] {
  return current;
}

export function hostBrandCodes(): string[] {
  return current.map((b) => b.code);
}
