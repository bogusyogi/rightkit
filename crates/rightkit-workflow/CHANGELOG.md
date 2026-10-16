# Changelog

## 0.1.0

Renamed from rightkit-scenario (never published).

- First published release. Scenario interpreter and durable state machine for the RightKit scenario runtime (RK8): `perform` separated from `observe_until`, per-step idempotency keys, an append-only fsynced journal with torn-write truncation, leases with fencing tokens, cooperative cancellation with a deadline, retake replay with postcondition comparison, and the `WorkerTransport` trait boundary for remote workers.
