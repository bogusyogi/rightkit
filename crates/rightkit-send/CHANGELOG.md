# Changelog

## 0.1.0

Initial crate, extracted from Pulse's `core/src/localsend` (unpublished: `publish = false`
until the lead integrates).

- Typed errors (`Error`) replace the English strings Pulse returned. `Error::code` values
  serialize as snake_case tags; `Display` is a short developer diagnostic.
- Product neutrality through `Config`: product name, alias, device type and model,
  User-Agent, quarantine agent, temporary-file suffix and log prefix are caller-supplied.
- `Policy` carries the limits and folder name (defaults equal Pulse's current values).
- Caller-supplied trust store path (`Config::trust_path`). The legacy `known-devices.json`
  beside it is still read once, as before.
- Identity defect fixed: no throwaway identity per run. `Identity::generate(alias)` makes a
  new certificate and key, `Identity::load(cert, key)` restores a stored pair, and the host
  stores both. The crate never writes key material.
- Fingerprint mismatch on a pinned connection is its own error (`PeerIdentityMismatch`).
- Feature `multicast` (UDP discovery, HTTP register and sweep) is off by default and is
  never compiled into iOS builds.
- Feature `bonjour` advertises and browses `_pulse-send._tcp` with dns_sd on macOS.
  `Service::peer_add_resolved`, `Service::peer_remove` and `Service::bound_port` serve
  externally fed peers. `Device::px` reports the Pulse extension flag.
- Received files are 0644, never executable, and carry the quarantine mark on macOS.
- **Idle wake-ups (runtime budget).** The acceptor no longer polls a non-blocking listener every 50 ms
  and the liveness thread no longer ticks every 100 ms (15.3 wake-ups per second measured while idle).
  The acceptor blocks in `accept` on a thread of its own; the liveness thread sleeps its whole
  20-second period in one timed wait that a stop ends at once. Nothing else in the service wakes
  while it is idle (a quiet two seconds is at most three wake-ups). The 64-connection cap
  (the next connection is closed unread) and the 30 s read and write timeouts of a connection are unchanged.
- **`Service::stop` joins its threads.** It sets the stop flag, wakes the blocked `accept` with a
  connection to `127.0.0.1:<bound port>` from inside the process (the same wake on macOS and Windows; see
  `src/workers.rs` for why not `shutdown` or `close`), ends the liveness wait, and joins both threads, so
  it returns in a few milliseconds with the listener closed: a second `Service::start` on the same port
  right after `stop` succeeds (it could race the acceptor's next 50 ms poll before). If the wake were ever
  to fail, `stop` gives up on that thread after 2 s and leaves it detached rather than hanging.
  Connection handlers and liveness probes already running are not joined; they end on their own within
  their socket timeouts. Public API and signatures unchanged; `Service` gains private fields only.
- Not changed, recorded for the lead: with the `multicast` feature the announcer and scanner threads
  still tick every 100 ms and the discovery socket times out every 500 ms (`spawn_announcer`,
  `spawn_scanner`, `spawn_discovery`). The runtime budget bench builds without that feature.
- Tests: `workers.rs` (a stop ends an hour-long wait; a connection to itself ends a blocked accept and
  frees the port; a thread that will not end is left after the bound), `lib.rs` (at most three wake-ups
  in a quiet two seconds; `stop` under a second with every thread joined; three restarts on one port; the
  64-connection cap), `tests/localsend.rs` (stop is prompt and the same port serves again). Not yet
  re-measured: the recorded budget evidence predates this change.
