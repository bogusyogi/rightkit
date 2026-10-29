# @rightkit/outdated contract

Owner: RightKit (`packages/outdated`, capability `dependency-updates`). This contract holds Adrian's decisions for the package. Each rule names its source.

## Rules

| # | Rule | Source |
|---|---|---|
| O1 | `check` never reports a row it could not judge as current. An `unknown` row makes `check` exit 1 unless its name has a justified hold. "All current" (exit 0) means every row was judged current or is held with a reason. | Adrian, 2026-10-29: "what is the issue with rightkit outdated, tell new rk to documnt it as an issue, a decision and fix and publish asap"; bogusyogi/rightkit#1267; 0.3.0 |
| O2 | A hold is `{ name, reason, evidence, exitCondition }`, each a non-empty string. Only a justified hold excuses an unknown row. A bare name (the 0.2 form) still holds the dependency, but excuses nothing. | as O1 |
| O3 | Coverage: a manifest or toolchain file in the repository that `.rightkit/update.json` does not declare is a failure, not silence. Native libraries built from source at a pinned revision, and toolchains (`rust-toolchain.toml`, `packageManager`/`engines`), are kinds `check` judges. | as O1. **Not built yet** (open under #1267). |
