# rightkit-device-link 0.1.0 registry consumer

Standalone package outside the RightKit workspace. It depends on `rightkit-device-link` by exact registry pin
(`=0.1.0`) with default features. It has no path, Git, workspace or patch dependency and no
committed `Cargo.lock`.

The crate is sans-IO, so the whole protocol runs in memory: a SendQueue and a Receiver exchange
encoded frames. Covers fragmenting of UTF-8 text, hello, ordered delivery, cumulative
acknowledgement, skip markers, timeout expiry, reconnect replay, every refusal (hello required,
stream mismatch, not delivered, ack beyond sent, too large, window full) and wire decoding (plain
payload, other version, unknown type, trailing bytes).

Proof row: public API surface of the new crate. Each check prints an `ok:` line; a failed check prints `FAIL:` and exits 1.
Reviewed 2026-11-03 as a publication qualification recipe for a crate that had none; it makes no claim about live OS
integration, which the installed-app QA carries.
