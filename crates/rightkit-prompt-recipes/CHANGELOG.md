# Changelog

## 0.2.0

- Changed (breaking): the default build is the pure library over the embedded data: contracts and validator, the recipe composer (`recipes::compose`, `compose_edit`, `libraries`), the image workflow catalogue, the character and LoRA lock data and the video shot-contract templates. It no longer depends on `rightkit-media-gen` or `rightkit-media-gen-approvals`. The `studio.<area>.*` command handlers (`commands`, `dispatch`, each module's `handle`) and the two modules that join the engine's registry (`recipe_catalogue`, `swap_templates`) are behind feature `commands`, a thin adapter over a `rightkit_media_gen::Context`.
- Changed (breaking): fallible calls return `RecipeError` / `RecipeResult` (same `{code, message, details?}` shape); with `commands` it converts to and from `rightkit_media_gen::GenError`, and `dispatch` still returns the engine's result type.
- Removed (breaking): `kdp` and `data/kdp` (one product's Kindle colouring-page workflow; it stays in the GenRight app) and `brands` with `data/brands/brands.json` (the owner's brand list and the `studio.brands.*` commands; the gallery's brand catalogue in `rightkit-media-gen` is the host-registered one).
- Added `brands` (new content): the host's brands. `brands::set_brands(Brands { codes, defaults })` installs the brand codes a request may carry and their per-library defaults (`recipes/<library>` and `video/<library>` keys); `NA` (no brand) is always accepted. The shipped data no longer carries the owner's `_brand_defaults`, brand list or venture list; `catalog::brands()`, the video venture check and the video brand check read the host's set.
- Removed owner brand names from the video template data (ad patterns, aesthetics, text overlays). The video preflight meta `source` is `video_preflight`.
- `studio.recipes.catalogue` reports each node's `depends_on`, so a UI reads the graph edges from the engine instead of embedding the seeded files.
- Taken on the recommended option of the 2026-11-11 shape review, pending the owner's confirmation (docs/audits/2026-11-11-shape-matrix/reports/genright-redraw.md).

## Unreleased

- Draft approvals (`issue`, `gate`, `consume`, `approval_fields`) come from `rightkit-media-gen-approvals` 0.1.0, where the GenRight approvals moved when `rightkit-approvals` 0.2.0 became the one approvals core (decision 3 of the 2026-11-11 shape review, taken on the recommended option, pending the owner's confirmation). Same calls, same records; a challenge issued before the move is not read (the app asks for a new one).
- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from GenRight (genright-studio recipes, contracts, catalog, characters, brands, kdp, video and swap templates).
