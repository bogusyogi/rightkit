# rightkit-prompt-recipes

Prompt contracts per image model and their validator, the recipe composer (a structured brief to a
per-model `{prompt, params, meta}`), the image workflow catalogue, the character and LoRA lock data,
and the video shot-contract templates, over JSON data embedded at compile time.

## Brands are the host's

The crate ships no brands. A host installs its brand codes and their per-library defaults once at
start with `brands::set_brands(Brands { codes, defaults })`; `NA` (no brand) is always accepted.
Without brands a request names its model and items explicitly.

## Host options

`host::set_options(HostOptions { .. })` installs, once per process and before the first composition,
the few places a host's behaviour differs from the defaults: the app's display name in two character
messages, the first segment of the character-training approval basis, the content-root variable and
the preflight `source` of the video templates, the order of the video brand message, whether the
recipe catalogue reports `depends_on`, and a JSON overlay merged over the embedded recipe and video
libraries (objects merge by key, arrays replace whole, `null` deletes; the result is validated).
`swap_templates::set_downloader` (feature `commands`) installs the host's URL downloader. Every
option defaults to the crate's own behaviour.

## Features

| Feature | Adds |
|---|---|
| (default) | The pure library: no engine, database or network. |
| `commands` | The `studio.<area>.*` command handlers (`commands`, `dispatch`, each module's `handle`) over a `rightkit_media_gen::Context` and the approval gate of `rightkit-media-gen-approvals`, plus `recipe_catalogue` and `swap_templates`, which join the engine's model registry. |

Moved into RightKit from GenRight (genright-studio recipes, contracts, catalog, characters, video
and swap templates) on 2026-11-01. The KDP workflow and the brand list stayed with the app
(2026-11-11).
