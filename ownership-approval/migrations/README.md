# Existing-duplicate migration drafts

102 unsigned records cover eight repositories. Each binds canonical repository identity, catalog digest, exact paths & observed occurrence hashes. These records confer no exception authority.

Run `node tools/rightkit/ownership-approval/migrations/validate-migration-drafts.mjs` against prepared local evidence before review. `prepared-migration-manifest.json` records 142 input hashes, source commits, dirty paths, incomplete coverage & unmatched legacy entries; it is generated local evidence rather than portable protected trust.

On a fresh checkout, regenerate unsigned evidence with `node tools/rightkit/ownership-approval/migrations/prepare-migration-drafts.mjs --refresh`. Preparation reads existing handoff backlogs under `tasks/handoffs/2026-10-07/ownership/`; retain those source inputs. Never put signed records in `drafts/`.

Fill expiry, request ID & verified HTTPS tracking reference before native review. Adrian signs each completed record through either host helper after both public keys are enrolled. Save signed output separately, then use `ownership-approval-command.mjs --import` & `--verify` from `packages/release`; import alone proves no approval. Integrate only verified signed records into each app's exception array, preserving existing entries.
