# rightkit-app-manager registry consumer

Standalone consumer, outside the workspace, with an exact registry pin. The offline registry serves the candidate archive. The consumer exits non-zero on the first failed check.

## What it proves

The crate validates bundle IDs, compares versions, takes the host app's identity once and exports the account-home resolver setter on the host where the consumer runs.
