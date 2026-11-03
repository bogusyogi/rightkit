# rightkit-device-link

Reliable, ordered message delivery between linked devices over a relay (rightkit#1244; consumers: CodeRight iOS to desktop, Pulse device links, any app pairing a phone with a desktop). Sans-IO, std only. Catalog capability: `device-link-reliable` (declaration-only).

The caller moves `Frame`s over its transport (the relay of rightkit#133) with `encode` and `decode`, and drives two state machines.

## Sending: `SendQueue`

- `enqueue(message, now_ms)` numbers the message (stream id plus sequence) and splits text over 128 KiB into UTF-8-safe fragments. Messages are capped at 4 MiB (at most 64 fragments), at most 64 are unacknowledged, and at most 16 MiB is pending (`EnqueueError`).
- `transmit()` returns the frames not yet sent on this connection: a `Hello` (stream id and `base_seq`, the transport base sequence) first on a new connection, then fragments or skip markers.
- `on_frame(&Ack)` drops everything below the cumulative `next_expected`.
- `reconnect()` replays every unacknowledged message under its original sequence number after a new `Hello`; `retransmit()` replays without a new `Hello`.
- `skip(seq, reason)` and `expire(now_ms, timeout_ms)` replace a permanently failed or timed-out message with a skip marker under the same sequence number, so the stream advances.

A device that only sends commands uses a `SendQueue` and no `Receiver`.

## Receiving: `Receiver`

- `on_frame(frame)` takes `Hello`, `Data` and `Skip`. A `Hello` for a new stream resumes at the sender's `base_seq` (a receiver that restarted and lost its ACK state continues instead of waiting for sequence 0). Duplicates of accepted messages are dropped and answered with the current `Ack`. Out-of-order and partial messages wait in a bounded buffer (window of 64, reassembly cap 16 MiB). The slot for the message at the head of the queue is always reserved: other messages may only use the cap minus one maximum-size message, so a full buffer cannot deadlock.
- `poll_delivery()` hands out the head message once complete, strictly in order, one at a time.
- `accept(seq)` returns the cumulative `Ack` only now, after the handler accepted; `fail(seq)` advances the same way for a message that can never be handled.

## Version and old peers

Every frame carries `RKDL` and a version byte. `decode` returns `DecodeError::UnsupportedVersion` for another version (fail closed) and `DecodeError::NotAFrame` for bytes without the magic, which the caller treats as the plain payload of an old peer.

## Acceptance still to run

Over the #133 relay with one app (device run). The crate has no unit tests (end-to-end only, Adrian 2026-11-03).

## Not here

The transport itself, encryption, persistence of the send queue across a process restart (callers keep `base_seq` and use `SendQueue::resume`).
