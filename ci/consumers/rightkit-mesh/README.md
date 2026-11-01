# rightkit-mesh 0.1.0 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-mesh =0.1.0` exactly with no default features (the phone configuration: no macOS fast datapath). It installs the crate from the sealed archive through the offline directory registry and runs `src/main.rs`, which exits non-zero on the first failed check. No other dependency.

## What it proves

- The seal: round trip between two paired devices, the sender sequence readable from the clear header (`peek_seq`), padding to size buckets, binding to the record id, refusal of a tampered item, of another key and of malformed input.
- The relay caps: the small-item limit (`check_small`, `SMALL_ITEM_MAX`) and month keys (`month_key`).
- `RelayBudget`: reserve within the monthly limit, refusal over it, refund, and persistence across reopen.

## What it deliberately does not prove

- iroh endpoints, live sessions, the mailbox server (`rightkit-mesh-mailbox`), sync reconciliation and transfers: they need two endpoints and a relay, which a hermetic run does not have.
- The macOS fast datapath (feature `fast-apple-datapath`, off here).
