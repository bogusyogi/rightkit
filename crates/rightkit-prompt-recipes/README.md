# rightkit-prompt-recipes

Prompt contracts per image model and their validator, the recipe composer (a structured brief to a
per-model `{prompt, params, meta}`), the image workflow catalogue, the character and LoRA lock data,
and the video shot-contract templates, over JSON data embedded at compile time.

## Brands are the host's

The crate ships no brands. A host installs its brand codes and their per-library defaults once at
start with `brands::set_brands(Brands { codes, defaults })`; `NA` (no brand) is always accepted.
Without brands a request names its model and items explicitly.

## Features

| Feature | Adds |
|---|---|
| (default) | The pure library: no engine, database or network. |
| `commands` | The `studio.<area>.*` command handlers (`commands`, `dispatch`, each module's `handle`) over a `rightkit_media_gen::Context` and the approval gate of `rightkit-media-gen-approvals`, plus `recipe_catalogue` and `swap_templates`, which join the engine's model registry. |

Moved into RightKit from GenRight (genright-studio recipes, contracts, catalog, characters, video
and swap templates) on 2026-11-01. The KDP workflow and the brand list stayed with the app
(2026-11-11).
