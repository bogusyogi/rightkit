# rightkit-send — extraction brief and C surface (agreed 2026-10-10)

Source: pulse/docs/scraperight-handoff.md; Pulse Mac chat (protocol authority) and scraper chat (iOS consumer) messages of 2026-10-10. RightKit owns the crate, its publication and the iOS packaging; Pulse owns the wire extensions, trust rules and hub UI; ScrapeRight owns the phone app. Status: brief agreed between the three chats; extraction starts after the 2026-10-10 crate release; Pulse re-pins and deletes its copy only after the parity gate is green and Adrian says yes.

## Source and split

- Extract `pulse/core/src/localsend/` (4,225 lines: discovery, mod, net, proto, receive, send, trust; no `crate::` imports outside itself) into `crates/rightkit-send`.
- Product-neutral: typed errors instead of English strings (mod.rs:722, send.rs:215/272/277, 413/403 texts); product name, User-Agent (net.rs:607), quarantine agent (receive.rs:602), temp suffix `.pulse-part` (receive.rs:637) and log prefix become caller configuration; a `Policy` struct with Pulse's current defaults (unverified folder name "Received (unverified)", unproven caps 200 files / 2 GiB, 64 GiB single-file refusal, 64 KiB message cap, ANSWER_WAIT); caller-supplied trust store path and identity storage (Keychain via RightKitAppleCredentials on iOS). Wire key `pulse.clipboard` unchanged (deployed). `send_cmd.rs`, `hub/src-tauri/src/share.rs` and the notches stay in Pulse.
- Defect fixed during extraction, not copied: one stored identity (certificate + key) per install, supplied by the caller; never a throwaway identity per run.
- Dependencies: rustls 0.23 with the custom client/server verifiers, sha2 directly, serde; threads and blocking sockets, no tokio. `rust-version = "1.90"` (Pulse pins rustc 1.90); the rest of the workspace stays at 1.98, so the crate takes **no** dependency on any RightKit crate that requires 1.98 (rightkit-fs, logs, secrets all declare 1.98 today); digests use sha2 in-crate.
- Features: `multicast` (UDP discovery and HTTP register for LocalSend peers) off on Apple targets; `bonjour` advertise/browse as below, implemented on macOS (dns_sd) and Windows 10+ (DnsServiceRegister/DnsServiceBrowse, no Apple Bonjour install) because Pulse on both computers must advertise and browse so the phone finds them and they find the phone; on iOS browse/advertise stays in Swift.

## Discovery (Bonjour)

Service `_pulse-send._tcp`; port from SRV (the engine's HTTPS port, default 53317). TXT, short ASCII keys: `fp` 64-hex lowercase SHA-256 of the device certificate (DER); `alias` UTF-8 ≤80 chars, no control chars; `v` "2.2"; `type` desktop|mobile|web|headless|server; `model` optional; `px` "1" (understands the `pulse` extensions and presents a TLS client certificate). TXT is an announcement only; identity is proven by the handshake certificate, never by `fp`. On iOS the crate must not open its own mDNS/multicast sockets: browse/advertise is done by Swift (NWBrowser/NWListener) and fed through `peer_add_resolved`/`peer_remove`; the crate exposes `bound_port` for the Swift advertiser. The phone advertises `type=mobile` only while foregrounded. (Platform claim from scraper, unverified on device until built.)

## Security parity (named tests, run against both engines before Pulse re-pins)

Certificate vs callback proof tiers; a record that has proved a certificate never accepts a callback; Deny wins while a card or callback is pending; no http sends; 64-hex fingerprint required; overflow-checked sizes; received files 0644 and never executable, never opened automatically, with the platform hardening mark (quarantine xattr on macOS, Zone.Identifier on Windows, none on iOS where files stay in the app container — the parity test must not expect a mark there); a message's text is never exposed before accept (Incoming carries no preview; text attaches to the transfer after accept); auto-accept applies only to a fingerprint proven by the handshake certificate; the `clipboard` flag is reported true only for a trusted, certificate-proven sender; selected-peer fingerprint checked against the handshake certificate (mismatch is a distinct error code).

Parity is measured against the **Rust API**, which keeps Pulse's full trust store: three states (Allow, Ask, Deny) with a default state; per-device records (fingerprint, label, alias, alias_claimed, model, kind, proof, firstSeen, lastSeen, refused count); set / label / forget / set-default operations; the callback proof tier; and the events the hub maps into share-state.json (Incoming, IncomingResolved, transfer updates). The C surface below is a subset of that API.

## C surface (opaque handle; callbacks take `void*` context and fire on a library thread; no panics across FFI; every returned buffer has a matching free; int error codes + `rk_send_last_error_message`)

1. Identity: `identity_create(alias) -> cert DER + private key bytes` (app stores both via RightKitAppleCredentials, device-only scope); `identity_load(cert, key) -> handle`; `identity_fingerprint(handle) -> 64-hex`.
2. Lifecycle: `start(handle, config{alias, device type, model, receive dir, listen port | 0})`, `stop`; `suspend`/`resume` (iOS drops sockets in background); `bound_port`.
3. Discovery: `discover_start`/`discover_stop` with `on_peer_found`/`on_peer_lost {fp, alias, type, model, host, port, px}`; `peer_add_resolved(fp, alias, host, port, txt)` and `peer_remove(fp)` for Swift-fed peers. Only `px=1` peers are surfaced as "Send to <alias>".
4. Send: `send_text(peer_fp, utf8, clipboard)` and `send_files(peer_fp, paths[], clipboard) -> transfer id`; `on_progress(id, sent, total)`, `on_finished(id, status)`, `cancel(id)`.
5. Receive: `on_offer(offer_id, sender fp, alias, cert_proven, clipboard, items[name, size, mime, is_message])` — no inline text before accept; `accept(offer_id)`/`decline(offer_id)`; `trusted_set(fps[])` auto-accepts only offers whose sender proved that fingerprint with its certificate (an unproven arrival of a trusted fingerprint raises `on_offer` with `cert_proven=false` and is never auto-accepted); no callback tier on the phone; `on_progress`; `on_received(offer_id, item index, file path | text)`; `on_error`.
6. Packaging: xcframework with ios-arm64 and ios-arm64-simulator slices, min iOS 17.0; Swift product `RightKitSend` in RightKitSwift with async wrappers. Built through `right-release`; this is RightKit's first Rust C ABI and sets the pattern for SwiftRight/Pulse mac adoption of process/hotkeys.

## Open for Adrian

- Receive tab history (handoff open point 3): scraper proposes keeping received items for 30 days or until deleted, with "Process" beside "Open" on received links.
- Pulse re-pin and deletion of its engine copy after parity (Pulse is asking him directly).
- Catalog capability name (`device-send`) in the next trust-sync batch.
