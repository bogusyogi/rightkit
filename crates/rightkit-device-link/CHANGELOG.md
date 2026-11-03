# Changelog

## 0.1.0 (unpublished)

- New crate (rightkit#1244): reliable delivery layer over the relay. Per-stream sequence numbers, in-order delivery, cumulative ACK after handler accept, UTF-8-safe 128 KiB fragments (64 per message, 4 MiB messages), bounds of 64 unacknowledged messages and 16 MiB pending and reassembly with a reserved head slot, replay after reconnect with `transportBaseSeq` in the `Hello`, skip markers for failure and timeout, send-only clients, version fail-closed and plain-payload fallback. Not compiled when written; verification is the gate.
