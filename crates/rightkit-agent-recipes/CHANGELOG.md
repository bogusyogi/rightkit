# Changelog

## 0.1.3

- `resolve_run_plan` refuses a plan with more than `MAX_RUN_PLAN_STEPS` (64) steps, with the error "recipe run plan exceeds 64 steps". Sub-recipe depth was already capped but breadth was not, so a catalog of repeated sub-recipe references could expand without bound. Plans of 64 steps or fewer resolve as before.

## 0.1.2

- Host naming option, default unchanged: `set_deeplink_scheme` / `deeplink_scheme()` (default `rightkit-agent`, `DEFAULT_DEEPLINK_SCHEME`); the deeplink builder writes it and the parser requires it.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-recipes` (CodeRight).
