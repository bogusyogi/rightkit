# rightkit-approvals 0.2.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-approvals =0.2.0` exactly with its default features (`file`). It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which prints `ok:` lines and exits 1 on the first failed check.

## What it proves

- `bind_of` length-prefixes its parts (two splits of the same bytes differ) and is 64 lowercase hex; `new_id` gives distinct 32-hex ids.
- `Binding::check`: another destination is `Different`, other arguments or another revision `Changed`, a past expiry `Expired`.
- The rules on a `MemoryStore`: a repeat of a waiting request gets the same id; the pending cap refuses with `Busy`; a decision with a display digest that is not the request's is `Shown` and leaves it pending; a decision with the right digest approves; `take` refuses another door, filer or destination (`Different`) and other arguments (`Changed`), uses once, and a second use is `Used`; a pending request without a proof is `Pending`; a session grant is found for its own client only.
- A `Proof` that refuses blocks a decision; one that accepts lets `take` decide and use a pending request in one move.
- Claim and settle on a `MemoryStore`: one claim wins and a second is `Used`; a claimed approval cannot be taken or revoked; `settle` records the result and a second settlement is `Unclaimed`; `recover` settles every claim nobody settled as the reconciler answers (`failed`, `unknown_outcome`), and a later settlement is `Unclaimed`.
- `revoke` withdraws an approved request (`Denied`, reason `revoked`; its use is `Denied`); `revoke_where` withdraws every pending or approved request it selects.
- `Audited` over a `MemoryStore`: a failed `Audit` append stops the move and leaves the approval as it was; filings and moves each get their row first.
- `FileStore` under a temp folder: the request file sits at `<kind>/<destination>.<id>.json` with `nonce` and `input_hash` keys, a use creates `.used`, and a second use loses; a claim creates `.claimed`, is listed by `claimed()`, and settles once to an unknown outcome read back from the file.

## What it deliberately does not prove

App proofs (launch-secret tokens, approver signatures, PINs) and app stores (MailRight's SQL table): their crates' recipes cover them.
