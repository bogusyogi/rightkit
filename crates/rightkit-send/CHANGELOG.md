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
