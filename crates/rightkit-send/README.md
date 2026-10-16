# rightkit-send

Nearby file and text sharing over the open LocalSend v2 protocol, with the Pulse extensions:
TLS identity pinning, per-device trust (Allow, Ask, Deny), macOS Bonjour discovery, and
received files that are never executable. Blocking sockets and threads; no async runtime.

Status: `0.1.0`, published on crates.io. Rust API only: no C ABI is exported and no xcframework is in this repository.

## What it owns

- `Service`: starts sharing, keeps the device list, receives after the owner answers, sends
  to a chosen device, and reports changes through the `Event` callback and `Service::snapshot`.
- `Identity`: a certificate and private key. `Identity::generate(alias)` creates one;
  `Identity::load(cert_der, key_der)` restores a stored pair. The host stores both (for
  example in the Keychain) and passes the same pair on every start, so the fingerprint other
  devices remember survives restarts.
- Trust: `Service::set_trust` (Allow, Ask, Deny), `set_label`, `forget`, `set_default_state`,
  `devices_trust`. A record that proved its certificate never accepts the weaker callback.
  If the stored device list cannot be read at start, nothing is written over it: a change reads
  it again first and fails with `Error::TrustStore` while it stays unreadable. Known limit: until
  the list has been read, every device is treated as Ask, so a device the owner denied gets a
  request card again. It is never allowed by this.
- Errors: `Error`, typed. Hosts map variants (or the serialized `code`) to their own words.
- `Policy`: the unproven-sender limits, the 64 GiB single-file refusal, the message cap and
  the answer wait. `Policy::default()` is Pulse's current behaviour.

## Configuration

`Config` is supplied by the host. The crate never names a product. Required strings:
`product_name`, `alias`, `device_type` (desktop, mobile, web, headless or server),
`device_model`, `user_agent`, `quarantine_agent`, `temp_suffix` (`.pulse-part` for Pulse),
`log_prefix` (`pulse sharing:` for Pulse). Paths: `save_dir` and `trust_path`.
`port: 0` picks a free port, reported by `Service::bound_port`.

## Cargo features

| Feature | Default | Enables |
|---|---|---|
| `multicast` | no | UDP discovery and HTTP register for LocalSend peers. Off by default. When enabled it compiles on every target except iOS, macOS included (`src/lib.rs:14`); enable it only where it is the discovery path, for example through a target-specific dependency. Never compiled into iOS builds. |
| `bonjour` | no | macOS dns_sd advertise and browse of `_pulse-send._tcp`. A no-op on other targets; Windows DnsService is not in this release. |

On iOS the crate opens no mDNS or multicast sockets. Browse and advertise belong to Swift
(NWBrowser and NWListener), which feeds results through `Service::peer_add_resolved` and
`Service::peer_remove`.

## Security notes

- TXT records (Bonjour) and announcements are claims. Identity comes only from the handshake
  certificate. A certificate that is not the one the fingerprint names is
  `Error::PeerIdentityMismatch`.
- Sends go only over verified TLS to a pinned fingerprint. A peer on plain HTTP is refused
  before any connection.
- A request's text message is never shown before the owner accepts.
- Auto-accept happens only for an Allow record with proof: a certificate, or a connect-back
  callback (a convenience tier, see `trust.rs`).

## Limits of this release

- One `Service` per process. The client identity and User-Agent are process-wide settings,
  as they were in Pulse.
- `suspend` and `resume` (iOS background), the C ABI and the Windows DnsService backend are
  not in this release.

## Tests

`rightkit cargo test -p rightkit-send --all-features` runs the in-crate tests and the loopback
integration tests in `tests/localsend.rs` (no network is needed beyond the local machine).

## Runtime budget

Generated from `runtime-budget.json`; edit that file, then run `node packages/release/runtime-budgets.mjs sync <crate-dir>`.

- Resident memory ceiling: 15.0 MiB.
- CPU ceiling (idle, steady state): 1%.
- Wake-ups ceiling: undeclared.
- Disk operations ceiling: 1 operations per second.
- Required scale: 50 devices or more.
- Required hosts: macOS and Windows.
- Status exposure: `Service::status()` in `src/lib.rs`; exposes resident memory, idle CPU, wake-ups, disk operations.
- Measured: not yet measured.
