# Changelog

## 0.4.0 (unpublished)

Breaking: the trust store is public and keyed by a subject, and the crate gains a transport layer (rightkit#407; Pulse replaces `core/src/localsend` and the mesh re-implementation of the send rules in `engine/src/share_mesh.rs`).

- Added: `transport` module. `Transport` (peers, `send_small`, `send_file`, `path_of`) is what a host implements for a channel that is not LocalSend's HTTPS (a device mesh). `Sender` sends text and files over it (Deny gate, one text as one small item, files as a chunked manifest, an announce and one stream per file named `b<16 hex>/<path>`); `Receiver` applies the receive rules (trust admission, unproven limits from `Policy`, the whole manifest approved once, each stream accepted only for a name in it, once, at no more than its declared size, bounded manifest storage, expiry of unstarted and stalled batches, text replay suppression). `strict_relative` refuses, never rewrites, names with `..`, absolute or UNC paths, backslashes, control or bidi characters, and on Windows reserved device names, trailing dots or spaces and `<>:"|?*`. The small-item wire (`Small`, tag `t`: `text`, `manifest`, `batch`) is the one Pulse's mesh already speaks. Not carried: Pulse's agent-chat quota and disk-reserve rules, LAN twin binding, hub log lines (those stay in the host; a batch's `agent` tag is passed through untouched).
- Added: `TrustSubject`, the generic trust key (lowercase, 1 to 128 characters, no whitespace or control characters): `TrustSubject::tls_fingerprint` for a LocalSend peer, `TrustSubject::new` for any other proven identifier such as a mesh `dev_<32 hex>` id. It is stored in the record's existing `fingerprint` field, so every stored trust file and the Swift SendKit layout read unchanged. `TrustEntry::subject()`.
- Changed (breaking): `trust::TrustStore` is `pub` (it was crate-private), so a host can keep a mesh trust file with the same Allow, Ask and Deny records, `set_label`, `forget`, default state and unreadable-file hold. Its keyed methods take `impl AsRef<str>` (a `&str`, a `String` or a `TrustSubject`). New `TrustStore::admission(subject, proven) -> Admission`: Deny refuses, Allow accepts only from a proven peer, everything else asks. `Admission` is exported.
- Added: `commands` module, a JSON command surface over `Service` (`invoke`, `COMMANDS`) for a Tauri host to register as one command; no webview dependency. Used by `@rightkit/send-ui`.
- Added: `SERVICE_TYPE` (`_pulse-send._tcp`), the Bonjour type registered and browsed by the `bonjour` feature, exported for hosts that declare it elsewhere (Pulse #251).
- Added: `Error` variants `InvalidSubject`, `TextTooLarge`, `NameRefused`, `ItemFailed`, `FileFailed`, `UnknownCommand`, `BadArguments` (the enum is `non_exhaustive`).
- Not built or run: written without compiling; the next gate run is the first compile. End-to-end proof needs Pulse's mesh transport carrying a real send between two devices.

## 0.3.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.3.0 (2026-10-29)

- Dependency: `libc` 0.2.190 -> 0.2.191 (minimum version). (rightkit#1284)
- Dependency: `sysinfo` =0.37.2 -> =0.40.0. `Networks::new_with_refreshed_list` now returns a `Result`; an interface list the OS refuses yields no local addresses and no sweep targets. (rightkit#1284)
- Added: `Transfer.accepted: bool` (serde default false), set when the receiver answers `/prepare-upload` with 200 or 204 and never cleared, not even by `end_transfer`. A host can tell a send that failed before the receiver's owner was asked (`accepted == false`: fall back to another route) from one that failed after they said yes (`accepted == true`: do not ask again). The byte and file counts could not say so: a zero-byte file or a first-byte failure leaves both at 0 (rightkit#1275; Adrian: "those things that are wrong with right kit, can you not fix them? ... make the changes").
- Changed: the first update after the receiver accepts, and the first `Phase::Sending` update, are always emitted as `Event::Progress`, outside the 100 ms limit, so a host sees `state == "active"` and `accepted == true`. `send::Progress` gains a public `accepted` field.

## 0.2.0 (2026-10-17)

- Added: `Entry::open_reader() -> io::Result<Box<dyn Read + Send>>`, a public reader over a prepared entry (file or message text), for a host that streams entries over its own transport (rightkit#537). `Entry.source` stays private.

## 0.1.0

Initial crate, extracted from Pulse's `core/src/localsend`. First published release.

- **Fixed: a device list that could not be read is never overwritten with a partial one.** When the stored list could
  not be read at start (on Windows an open can be refused for a moment while another process replaces the file), the
  store started empty and held back automatic saves, but the first explicit change (allow, deny, label, forget, default)
  wrote a list holding only that change over the file. Every other record and a stored default of Deny were lost.
  Lost records fell back to Ask, so nothing was ever granted, but denials and labels were forgotten. Now nothing is
  written while the list is unread. An explicit change reads the file again first: if it can be read, the change is
  made to the full list (devices only sighted meanwhile are added and take the stored default); if it still cannot,
  the change fails with `Error::TrustStore` and nothing is changed. Remaining limit: until the list has been read,
  every device is treated as Ask, so a device the owner denied gets a request card again. Not built or run locally.

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
