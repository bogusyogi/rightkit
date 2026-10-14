//! Nearby sharing over the open LocalSend protocol v2 (github.com/localsend/protocol),
//! with the Pulse extensions (`pulse` key in prepare-upload, `px` in Bonjour TXT).
//!
//! `Service` is a long-running object a host owns: it announces this device,
//! keeps a list of nearby devices, receives files over HTTPS after the owner
//! accepts, and sends files to a chosen device. The host supplies a stored
//! `Identity`, the trust store path, the folders and the wording of anything it
//! shows; it hears about changes through the `Event` callback and reads
//! `Service::snapshot`. Errors are typed (`Error`); no user-facing English is
//! produced here.

#[cfg(all(feature = "bonjour", target_os = "macos"))]
mod bonjour;
#[cfg(all(feature = "multicast", not(target_os = "ios")))]
pub mod discovery;
mod error;
pub mod net;
mod policy;
pub mod proto;
mod receive;
pub mod send;
pub mod trust;
mod workers;

pub use error::Error;
pub use net::Identity;
pub use policy::Policy;
pub use send::{Entry, Peer, SendItem};
use trust::normalize;
pub use trust::{TrustEntry, TrustState};
use workers::{Stopper, Worker};

use proto::DeviceInfo;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What the host configures. Every string here is the host's own choice: the
/// crate never names a product.
#[derive(Clone, Debug)]
pub struct Config {
    /// The host's product name, reported in `Snapshot::product`.
    pub product_name: String,
    /// How other devices list this device.
    pub alias: String,
    /// Listen port (TLS). 0 picks a free port; `Service::bound_port` reports it.
    pub port: u16,
    /// Device class announced to others: "desktop", "mobile", "web", "headless" or "server".
    pub device_type: String,
    /// Model announced to others (empty for none).
    pub device_model: String,
    /// Where received files go.
    pub save_dir: PathBuf,
    /// The JSON trust store file. Created on the first change.
    pub trust_path: PathBuf,
    /// Sent as the HTTP User-Agent on every outgoing request.
    pub user_agent: String,
    /// Written into the macOS quarantine attribute as the agent name.
    pub quarantine_agent: String,
    /// Suffix of the temporary file a receive is written to until it is complete (".pulse-part" for Pulse).
    pub temp_suffix: String,
    /// Prefix of the diagnostic lines written to stderr (e.g. "pulse sharing:").
    pub log_prefix: String,
    /// Limits and folder names. `Policy::default()` is Pulse's current behaviour.
    pub policy: Policy,
}

/// A device on the network.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub fingerprint: String,
    pub alias: String,
    pub device_model: Option<String>,
    pub device_type: Option<String>,
    pub ip: String,
    pub port: u16,
    pub protocol: String,
    /// The device announced the Pulse extensions (Bonjour TXT `px=1`). A host
    /// decides what to offer; LocalSend-only devices report false.
    pub px: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct IncomingFile {
    pub name: String,
    pub size: u64,
}

/// A request waiting for the user's answer.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Incoming {
    pub id: String,
    pub from: String,
    pub device_model: Option<String>,
    pub fingerprint: String,
    pub ip: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub is_message: bool,
    /// Always None: a text message is not shown until it is accepted.
    pub preview: Option<String>,
    /// The first few files, for display (none for a message).
    pub files: Vec<IncomingFile>,
    /// The sender proved it holds the key behind its fingerprint by presenting its
    /// certificate. Anything less is unproven: saved apart, no clipboard, capped.
    pub verified: bool,
}

/// One send or receive, from the first request to the last byte.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: String,
    /// "send" or "receive".
    pub direction: String,
    pub peer: String,
    pub peer_fingerprint: String,
    /// waiting, active, done, failed, cancelled or declined.
    pub state: String,
    pub total_bytes: u64,
    pub done_bytes: u64,
    pub files_total: usize,
    pub files_done: usize,
    pub current: Option<String>,
    pub saved_to: Option<String>,
    pub saved_files: Vec<String>,
    pub error: Option<Error>,
    /// A received text message, shown with Copy rather than saved as a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Sent from the clipboard: the receiver puts it on its clipboard.
    #[serde(default)]
    pub clipboard: bool,
    pub started: u64,
    pub finished: Option<u64>,
}

impl Transfer {
    pub fn is_open(&self) -> bool {
        self.state == "waiting" || self.state == "active"
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// The host product name from `Config::product_name`.
    pub product: String,
    pub alias: String,
    pub fingerprint: String,
    pub port: u16,
    pub save_dir: String,
    pub devices: Vec<Device>,
    pub incoming: Vec<Incoming>,
    pub transfers: Vec<Transfer>,
    /// Things that don't work, typed (discovery socket, …).
    pub warnings: Vec<Error>,
    /// "unknown", "granted" or "blocked" (macOS Local Network access).
    pub local_network: String,
    /// A sweep of the local networks is running.
    pub scanning: bool,
}

#[derive(Clone, Debug)]
pub enum Event {
    /// The device list changed.
    Devices,
    /// A device wants to send; the user must answer (`Service::respond`).
    Incoming(Incoming),
    /// A request was answered, withdrawn or timed out.
    IncomingResolved(String),
    /// Bytes moved (rate-limited).
    Progress(Transfer),
    /// A transfer reached an end state.
    Finished(Transfer),
    /// Anything else (a transfer dismissed).
    Changed,
}

/// One entry of `known-devices.json`: a bare fingerprint (accepted before).
/// Older files held objects with extra fields; only the fingerprint is kept.
#[derive(Deserialize)]
#[serde(untagged)]
enum KnownEntry {
    Plain(String),
    Object {
        fingerprint: String,
        #[serde(default)]
        label: Option<String>,
    },
}

/// The old list as (fingerprint, label) pairs; the label is empty when none.
fn load_known(path: &std::path::Path) -> Vec<(String, String)> {
    let entries = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Vec<KnownEntry>>(&bytes).ok())
        .unwrap_or_default();
    entries
        .into_iter()
        .map(|entry| match entry {
            KnownEntry::Plain(fingerprint) => (fingerprint, String::new()),
            KnownEntry::Object { fingerprint, label } => (fingerprint, label.unwrap_or_default()),
        })
        .collect()
}

/// Write `contents` next to `path` and rename it into place; the file is private
/// (0600) on Unix. The temp file is `.<name>.tmp-<hex>` in the same directory,
/// created exclusively (a name collision retries), fsynced, renamed over the
/// target, and the directory is fsynced so the rename survives power loss. A
/// failure removes the temp file and leaves the target as it was.
///
/// Deliberately not `rightkit-fs`: that crate requires rustc 1.98 and this one
/// is pinned to 1.90 (see docs/plans/2026-10-10-rightkit-send-design.md).
fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => std::path::Path::new("."),
    };
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut attempt = 0;
    let (temp, mut file) = loop {
        let temp = parent.join(format!(".{name}.tmp-{}", proto::random_hex(4)));
        match options.open(&temp) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 8 => attempt += 1,
            Err(e) => return Err(e),
        }
    };
    let written = file.write_all(contents).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(e);
    }
    // The new content is in place; a directory sync failure is not a failed write.
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod write_atomic_tests {
    use super::write_atomic;

    fn dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("rk-send-atomic-{tag}-{}", proto::random_hex(4)));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    use super::proto;

    #[test]
    fn replaces_existing_content_and_leaves_no_temp() {
        let d = dir("replace");
        let target = d.join("nearby_devices.json");
        write_atomic(&target, b"one").unwrap();
        write_atomic(&target, b"two").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["nearby_devices.json".to_string()]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn creates_missing_parents() {
        let d = dir("parents");
        let target = d.join("a").join("b").join("store.json");
        write_atomic(&target, b"x").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"x");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn the_result_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = dir("mode");
        let target = d.join("store.json");
        write_atomic(&target, b"x").unwrap();
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Replacing a broader file narrows it: the new inode is 0600.
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_atomic(&target, b"y").unwrap();
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sibling_differing_only_by_extension_is_untouched() {
        let d = dir("sibling");
        std::fs::write(d.join("store.bak"), b"keep").unwrap();
        write_atomic(&d.join("store.json"), b"x").unwrap();
        assert_eq!(std::fs::read(d.join("store.bak")).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_rename_removes_the_temp_and_keeps_the_target() {
        let d = dir("fail");
        // A directory at the target makes the rename fail on every platform.
        let target = d.join("store.json");
        std::fs::create_dir(&target).unwrap();
        assert!(write_atomic(&target, b"x").is_err());
        assert!(target.is_dir());
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["store.json".to_string()],
            "no stray temp: {names:?}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_at_the_target_is_replaced_not_followed() {
        let d = dir("link");
        let outside = d.join("outside.txt");
        std::fs::write(&outside, b"precious").unwrap();
        let target = d.join("store.json");
        std::os::unix::fs::symlink(&outside, &target).unwrap();
        write_atomic(&target, b"x").unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"precious");
        assert_eq!(std::fs::read(&target).unwrap(), b"x");
        assert!(
            !std::fs::symlink_metadata(&target)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let _ = std::fs::remove_dir_all(&d);
    }
}

pub(crate) struct Seen {
    pub device: Device,
    pub last_seen: Instant,
    pub misses: u32,
}

pub(crate) struct Pending {
    pub incoming: Incoming,
    pub decision: Mutex<Option<bool>>,
    pub changed: Condvar,
}

pub(crate) struct CancelHandle {
    pub flag: AtomicBool,
    pub stream: Mutex<Option<TcpStream>>,
}

pub(crate) struct SessionFile {
    pub meta: proto::FileMeta,
    pub token: String,
    pub done: bool,
}

pub(crate) struct Session {
    pub transfer_id: String,
    pub peer_ip: IpAddr,
    pub peer_fingerprint: String,
    pub files: HashMap<String, SessionFile>,
    pub cancel: Arc<AtomicBool>,
    pub activity: Instant,
    pub remaining: usize,
    pub saved: Vec<PathBuf>,
    pub save_dir: PathBuf,
    /// The sender proved its key by certificate (not only by callback).
    pub proven: bool,
}

pub(crate) struct Inner {
    pub cfg: Mutex<Config>,
    pub me: DeviceInfo,
    pub tls: Arc<rustls::ServerConfig>,
    pub stop: AtomicBool,
    /// Wakes the threads that wait with a timeout (liveness, accept retry) when the service stops.
    pub park: Stopper,
    pub devices: Mutex<Vec<Seen>>,
    pub transfers: Mutex<Vec<Transfer>>,
    pub pending: Mutex<HashMap<String, Arc<Pending>>>,
    pub sessions: Mutex<HashMap<String, Session>>,
    pub cancels: Mutex<HashMap<String, Arc<CancelHandle>>>,
    pub trust: Mutex<trust::TrustStore>,
    pub on_event: Arc<dyn Fn(Event) + Send + Sync>,
    pub last_progress: Mutex<Instant>,
    pub connections: AtomicUsize,
    pub warnings: Mutex<Vec<Error>>,
    /// 0 unknown, 1 reachable, 2 macOS is refusing local network access.
    pub local_network: std::sync::atomic::AtomicU8,
    /// A sweep of the local networks is running.
    pub scanning: AtomicBool,
    /// Someone asked for a sweep; the scanner thread takes it.
    #[cfg(all(feature = "multicast", not(target_os = "ios")))]
    pub scan_requested: AtomicBool,
    /// This Mac's own addresses and when they were read.
    pub own_ips: Mutex<(Instant, Vec<Ipv4Addr>)>,
    /// Bonjour instance name to the fingerprint it resolved to, so a Remove can be
    /// mapped back to a device.
    #[cfg(all(feature = "bonjour", target_os = "macos"))]
    pub bonjour_names: Mutex<HashMap<String, String>>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Inner {
    pub fn emit(&self, event: Event) {
        (self.on_event)(event);
    }

    fn config(&self) -> Config {
        lock(&self.cfg).clone()
    }

    /// Whether `ip` is one of this Mac's own addresses.
    fn is_own_ip(&self, ip: IpAddr) -> bool {
        let IpAddr::V4(v4) = ip else {
            return ip.is_loopback();
        };
        if v4.is_loopback() {
            return true;
        }
        let mut own = lock(&self.own_ips);
        if own.0.elapsed() > Duration::from_secs(30) {
            *own = (Instant::now(), local_ipv4s());
        }
        own.1.contains(&v4)
    }

    /// Record a device heard from `ip`, unless it is this Mac itself. Emits
    /// `Devices` when the list changed. Returns whether the device was taken.
    pub fn upsert(&self, info: &DeviceInfo, ip: IpAddr) -> bool {
        let fingerprint = normalize(&info.fingerprint);
        if fingerprint.is_empty() || fingerprint == self.me.fingerprint || self.is_own_ip(ip) {
            return false;
        }
        // An announcement is nobody's proof: it never sets a record's proof.
        lock(&self.trust).observe(
            &fingerprint,
            &info.alias,
            info.device_model.as_deref(),
            info.device_type.as_deref(),
            None,
        );
        let device = Device {
            fingerprint,
            alias: info.alias.clone(),
            device_model: info.device_model.clone(),
            device_type: info.device_type.clone(),
            ip: ip.to_string(),
            port: info.port,
            protocol: info.protocol.clone(),
            px: false,
        };
        let changed = {
            let mut devices = lock(&self.devices);
            match devices
                .iter_mut()
                .find(|s| s.device.fingerprint == device.fingerprint)
            {
                Some(seen) => {
                    // An announcement can move a device (ip, port) but never takes
                    // it from https back to http.
                    let mut device = device;
                    device.px = seen.device.px || device.px;
                    if seen.device.protocol == "https" {
                        device.protocol = "https".to_string();
                    }
                    let changed = seen.device.alias != device.alias
                        || seen.device.ip != device.ip
                        || seen.device.port != device.port
                        || seen.device.protocol != device.protocol;
                    seen.device = device;
                    seen.last_seen = Instant::now();
                    seen.misses = 0;
                    changed
                }
                None => {
                    devices.push(Seen {
                        device,
                        last_seen: Instant::now(),
                        misses: 0,
                    });
                    true
                }
            }
        };
        if changed {
            self.emit(Event::Devices);
        }
        true
    }

    /// Sweep the local networks for devices multicast didn't find.
    #[cfg(all(feature = "multicast", not(target_os = "ios")))]
    fn scan(&self) {
        self.scanning.store(true, Ordering::Relaxed);
        self.emit(Event::Changed);
        discovery::sweep(&self.me, &self.stop, &|heard| {
            self.upsert(&heard.info, heard.ip);
        });
        self.scanning.store(false, Ordering::Relaxed);
        self.emit(Event::Changed);
    }

    pub fn device_list(&self) -> Vec<Device> {
        let mut list: Vec<Device> = lock(&self.devices)
            .iter()
            .map(|s| s.device.clone())
            .collect();
        list.sort_by_key(|d| d.alias.to_lowercase());
        list
    }

    /// The port the device with this fingerprint announced, if it is on the list.
    pub fn announced_port(&self, fingerprint: &str) -> Option<u16> {
        let fingerprint = normalize(fingerprint);
        lock(&self.devices)
            .iter()
            .find(|s| s.device.fingerprint == fingerprint)
            .map(|s| s.device.port)
    }

    pub fn trust_state(&self, fingerprint: &str) -> TrustState {
        lock(&self.trust).state_of(fingerprint)
    }

    pub fn add_transfer(&self, transfer: Transfer) {
        let mut transfers = lock(&self.transfers);
        transfers.push(transfer);
        while transfers.len() > 12 {
            match transfers.iter().position(|t| !t.is_open()) {
                Some(index) => {
                    transfers.remove(index);
                }
                None => break,
            }
        }
    }

    /// Change one transfer. A progress event goes out at most ten times a
    /// second unless `force` is set.
    pub fn update_transfer(&self, id: &str, force: bool, change: impl FnOnce(&mut Transfer)) {
        let updated = {
            let mut transfers = lock(&self.transfers);
            transfers.iter_mut().find(|t| t.id == id).map(|t| {
                change(t);
                t.clone()
            })
        };
        let Some(transfer) = updated else { return };
        let due = {
            let mut last = lock(&self.last_progress);
            if force || last.elapsed() >= Duration::from_millis(100) {
                *last = Instant::now();
                true
            } else {
                false
            }
        };
        if due {
            self.emit(Event::Progress(transfer));
        }
    }

    /// Put a transfer in an end state, once.
    pub fn end_transfer(&self, id: &str, state: &str, error: Option<Error>) {
        let ended = {
            let mut transfers = lock(&self.transfers);
            transfers
                .iter_mut()
                .find(|t| t.id == id && t.is_open())
                .map(|t| {
                    t.state = state.to_string();
                    t.error = error;
                    t.finished = Some(now_ms());
                    if state == "done" {
                        t.done_bytes = t.total_bytes;
                        t.files_done = t.files_total;
                    }
                    t.current = None;
                    t.clone()
                })
        };
        if let Some(transfer) = ended {
            self.emit(Event::Finished(transfer));
        }
    }
}

/// Runtime figures of a running service, computed on call by [`Service::status`].
#[derive(Clone, Debug)]
pub struct SendStatus {
    /// Devices on the nearby list.
    pub devices: usize,
    /// Transfers not yet finished.
    pub transfers_open: usize,
    /// Transfers held in the list, finished ones included until dismissed.
    pub transfers_listed: usize,
    /// Incoming requests waiting for an answer.
    pub pending_incoming: usize,
    /// Connections being served now (capped at 64).
    pub connections: usize,
    /// Typed warnings currently raised.
    pub warnings: usize,
    /// A sweep of the local networks is running.
    pub scanning: bool,
    /// Resident memory, CPU time and I/O counters of the whole process (the acceptor, discovery and
    /// liveness threads run in it), or `None` when the host cannot report them. See
    /// `rightkit_child_process::ProcessUsage`.
    pub process: Option<rightkit_child_process::ProcessUsage>,
}

/// A running sharing service. Dropping it stops it.
pub struct Service {
    inner: Arc<Inner>,
    /// The acceptor and liveness threads, ended and joined by [`Service::stop`].
    workers: Mutex<Vec<Worker>>,
    #[cfg(all(feature = "bonjour", target_os = "macos"))]
    bonjour: Mutex<Option<(bonjour::Advertiser, bonjour::Browser)>>,
}

impl Service {
    /// Start sharing with `config`, presenting `identity`: the host's stored
    /// certificate and key. Fails before anything is announced when the identity,
    /// the trust store or the listen port cannot be used.
    pub fn start(
        config: Config,
        identity: Identity,
        on_event: Arc<dyn Fn(Event) + Send + Sync>,
    ) -> Result<Service, Error> {
        let tls = net::server_config(&identity).map_err(|e| Error::Identity {
            detail: e.to_string(),
        })?;
        // Windows lets a second program bind a port another one holds in some
        // cases (a dual-stack listener); a connection that succeeds first says
        // somebody is already listening, so say so instead of failing silently.
        #[cfg(windows)]
        {
            if config.port != 0 {
                let local = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, config.port));
                if TcpStream::connect_timeout(&local, Duration::from_millis(300)).is_ok() {
                    return Err(Error::PortInUse { port: config.port });
                }
            }
        }
        let listener = TcpListener::bind(("0.0.0.0", config.port)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse
                || (cfg!(windows) && e.kind() == std::io::ErrorKind::PermissionDenied)
            {
                Error::PortInUse { port: config.port }
            } else {
                Error::Listen {
                    port: config.port,
                    detail: e.to_string(),
                }
            }
        })?;
        let port = listener
            .local_addr()
            .map(|address| address.port())
            .map_err(|e| Error::Listen {
                port: config.port,
                detail: e.to_string(),
            })?;
        let mut config = config;
        config.port = port;

        net::set_client_identity(&identity);
        net::set_user_agent(&config.user_agent);
        let trust = trust::TrustStore::load(&config.trust_path);
        let me = DeviceInfo {
            alias: config.alias.clone(),
            version: proto::VERSION.to_string(),
            device_model: Some(config.device_model.clone()),
            device_type: Some(config.device_type.clone()),
            fingerprint: identity.fingerprint.clone(),
            port,
            protocol: "https".to_string(),
            download: false,
            announce: None,
        };
        let inner = Arc::new(Inner {
            cfg: Mutex::new(config.clone()),
            me,
            tls,
            stop: AtomicBool::new(false),
            park: Stopper::new(),
            devices: Mutex::new(Vec::new()),
            transfers: Mutex::new(Vec::new()),
            pending: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            cancels: Mutex::new(HashMap::new()),
            trust: Mutex::new(trust),
            on_event,
            last_progress: Mutex::new(Instant::now()),
            connections: AtomicUsize::new(0),
            warnings: Mutex::new(Vec::new()),
            local_network: std::sync::atomic::AtomicU8::new(0),
            scanning: AtomicBool::new(false),
            #[cfg(all(feature = "multicast", not(target_os = "ios")))]
            scan_requested: AtomicBool::new(false),
            own_ips: Mutex::new((Instant::now(), local_ipv4s())),
            #[cfg(all(feature = "bonjour", target_os = "macos"))]
            bonjour_names: Mutex::new(HashMap::new()),
        });

        // The listener stays blocking: the acceptor sleeps in `accept` until a peer arrives or
        // `stop` connects to it.
        let acceptor =
            spawn_acceptor(inner.clone(), listener, port).map_err(|e| Error::Listen {
                port,
                detail: e.to_string(),
            })?;
        #[cfg(all(feature = "multicast", not(target_os = "ios")))]
        {
            match discovery::listen(port) {
                Ok(socket) => spawn_discovery(inner.clone(), socket),
                Err(e) => lock(&inner.warnings).push(Error::DiscoveryUnavailable {
                    detail: e.to_string(),
                }),
            }
            spawn_announcer(inner.clone());
            spawn_scanner(inner.clone());
        }
        #[cfg(all(feature = "bonjour", target_os = "macos"))]
        let bonjour = start_bonjour(&inner, &config, &identity.fingerprint);
        let service = Service {
            inner: inner.clone(),
            workers: Mutex::new(vec![acceptor]),
            #[cfg(all(feature = "bonjour", target_os = "macos"))]
            bonjour: Mutex::new(bonjour),
        };
        match spawn_liveness(inner) {
            Ok(liveness) => lock(&service.workers).push(liveness),
            // Dropping `service` on the way out stops what has started.
            Err(e) => {
                return Err(Error::Listen {
                    port,
                    detail: e.to_string(),
                });
            }
        }
        Ok(service)
    }

    /// The port the service listens on (the chosen one when `Config::port` was 0).
    pub fn bound_port(&self) -> u16 {
        lock(&self.inner.cfg).port
    }

    /// Add a device a host-run resolver found (on iOS, Swift's NWBrowser feeds
    /// this). `txt` holds the TXT keys (`fp`, `alias`, `type`, `model`, `px`).
    /// The fingerprint must be 64 hex characters; identity is still proven by
    /// the handshake certificate, never by this announcement.
    pub fn peer_add_resolved(
        &self,
        fingerprint: &str,
        alias: &str,
        host: IpAddr,
        port: u16,
        txt: &[(String, String)],
    ) -> Result<(), Error> {
        let fingerprint = normalize(fingerprint);
        if !net::valid_pin(&fingerprint) {
            return Err(Error::InvalidFingerprint);
        }
        let info = resolved_info(&fingerprint, alias, port, txt);
        let px = txt_value(txt, "px").as_deref() == Some("1");
        self.inner.add_resolved(&info, host, px);
        Ok(())
    }

    /// Remove a device a host-run resolver reported gone. Returns whether it was
    /// on the list.
    pub fn peer_remove(&self, fingerprint: &str) -> bool {
        self.inner.remove_device(&normalize(fingerprint))
    }

    /// Counts and process resource usage, read now. No polling thread runs for this.
    pub fn status(&self) -> SendStatus {
        let inner = &self.inner;
        let transfers = lock(&inner.transfers);
        SendStatus {
            devices: lock(&inner.devices).len(),
            transfers_open: transfers.iter().filter(|t| t.is_open()).count(),
            transfers_listed: transfers.len(),
            pending_incoming: lock(&inner.pending).len(),
            connections: inner.connections.load(Ordering::Relaxed),
            warnings: lock(&inner.warnings).len(),
            scanning: inner.scanning.load(Ordering::Relaxed),
            process: rightkit_child_process::process_usage().ok(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let inner = &self.inner;
        let cfg = inner.config();
        let mut incoming: Vec<Incoming> = lock(&inner.pending)
            .values()
            .map(|p| p.incoming.clone())
            .collect();
        incoming.sort_by(|a, b| a.id.cmp(&b.id));
        Snapshot {
            product: cfg.product_name.clone(),
            alias: cfg.alias,
            fingerprint: inner.me.fingerprint.clone(),
            port: cfg.port,
            save_dir: cfg.save_dir.to_string_lossy().into_owned(),
            devices: inner.device_list(),
            incoming,
            transfers: lock(&inner.transfers).clone(),
            warnings: lock(&inner.warnings).clone(),
            local_network: match inner.local_network.load(Ordering::Relaxed) {
                1 => "granted",
                2 => "blocked",
                _ => "unknown",
            }
            .to_string(),
            scanning: inner.scanning.load(Ordering::Relaxed),
        }
    }

    /// Look for devices again now: announce on multicast and sweep the local
    /// networks (the sweep is skipped while one is already running).
    pub fn refresh(&self) {
        #[cfg(all(feature = "multicast", not(target_os = "ios")))]
        {
            let inner = self.inner.clone();
            thread::spawn(move || {
                let _ = discovery::announce(&inner.me, inner.config().port, true);
            });
            self.inner.scan_requested.store(true, Ordering::Relaxed);
        }
    }

    pub fn devices(&self) -> Vec<Device> {
        self.inner.device_list()
    }

    /// The fingerprint of this Mac's certificate.
    pub fn fingerprint(&self) -> String {
        self.inner.me.fingerprint.clone()
    }

    /// Every device with a record, newest sighting first.
    pub fn devices_trust(&self) -> Vec<TrustEntry> {
        lock(&self.inner.trust).list()
    }

    /// What a never-seen device is subject to: Ask or Deny.
    pub fn default_state(&self) -> TrustState {
        lock(&self.inner.trust).default_state()
    }

    /// Set a device to Allow, Ask or Deny. Deny also withdraws what the device
    /// has pending or in flight.
    pub fn set_trust(&self, fingerprint: &str, state: TrustState) -> Result<(), Error> {
        lock(&self.inner.trust).set_state(fingerprint, state)?;
        if state == TrustState::Deny {
            self.revoke(fingerprint);
        }
        self.inner.emit(Event::Changed);
        Ok(())
    }

    /// The owner's own name for a device (empty clears it).
    pub fn set_label(&self, fingerprint: &str, label: &str) -> Result<(), Error> {
        lock(&self.inner.trust).set_label(fingerprint, label)?;
        self.inner.emit(Event::Changed);
        Ok(())
    }

    /// Drop the record of a device that is not on the network now.
    pub fn forget(&self, fingerprint: &str) -> Result<(), Error> {
        if lock(&self.inner.devices)
            .iter()
            .any(|s| s.device.fingerprint == normalize(fingerprint))
        {
            return Err(Error::DeviceNearby);
        }
        lock(&self.inner.trust).forget(fingerprint)?;
        self.inner.emit(Event::Changed);
        Ok(())
    }

    /// How devices seen for the first time start: Ask or Deny.
    pub fn set_default_state(&self, state: TrustState) -> Result<(), Error> {
        lock(&self.inner.trust).set_default(state)?;
        self.inner.emit(Event::Changed);
        Ok(())
    }

    /// Decline this device's waiting requests and cancel its receives in flight.
    fn revoke(&self, fingerprint: &str) {
        let fingerprint = normalize(fingerprint);
        let fingerprint = fingerprint.as_str();
        let waiting: Vec<Arc<Pending>> = lock(&self.inner.pending)
            .values()
            .filter(|p| p.incoming.fingerprint == fingerprint)
            .cloned()
            .collect();
        for p in waiting {
            *lock(&p.decision) = Some(false);
            p.changed.notify_all();
        }
        let ended: Vec<String> = {
            let mut sessions = lock(&self.inner.sessions);
            let ids: Vec<String> = sessions
                .iter()
                .filter(|(_, s)| s.peer_fingerprint == fingerprint)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter()
                .filter_map(|id| sessions.remove(id))
                .map(|session| {
                    session.cancel.store(true, Ordering::Relaxed);
                    session.transfer_id
                })
                .collect()
        };
        for transfer_id in ended {
            self.inner.end_transfer(&transfer_id, "cancelled", None);
        }
    }

    pub fn set_save_dir(&self, directory: PathBuf) {
        lock(&self.inner.cfg).save_dir = directory;
    }

    /// The nearby device with this fingerprint (or, failing that, alias).
    fn find_device(&self, target: &str) -> Option<Device> {
        let devices = lock(&self.inner.devices);
        let wanted = normalize(target);
        devices
            .iter()
            .find(|s| s.device.fingerprint == wanted)
            .or_else(|| {
                devices
                    .iter()
                    .find(|s| s.device.alias.eq_ignore_ascii_case(target))
            })
            .map(|s| s.device.clone())
    }

    /// This computer's name as other devices list it.
    pub fn alias(&self) -> String {
        self.inner.config().alias
    }

    /// Send to the device with this fingerprint (or, failing that, alias).
    /// Returns the transfer id; progress arrives as events.
    pub fn send(
        &self,
        target: &str,
        items: Vec<SendItem>,
        clipboard: bool,
    ) -> Result<String, Error> {
        let device = self.find_device(target).ok_or(Error::DeviceNotNearby)?;
        if self.inner.trust_state(&device.fingerprint) == TrustState::Deny {
            return Err(Error::DeviceBlocked);
        }
        if device.protocol != "https" {
            return Err(Error::PeerPlaintext {
                peer: device.alias.clone(),
            });
        }
        if !net::valid_pin(&device.fingerprint) {
            return Err(Error::InvalidIdentity {
                peer: device.alias.clone(),
            });
        }
        let ip: IpAddr = device.ip.parse().map_err(|_| Error::NoUsableAddress)?;
        let peer = Peer {
            ip,
            port: device.port,
            https: device.protocol == "https",
            fingerprint: device.fingerprint.clone(),
            alias: device.alias.clone(),
        };
        let id = proto::random_hex(8);
        self.inner.add_transfer(Transfer {
            id: id.clone(),
            direction: "send".into(),
            peer: device.alias.clone(),
            peer_fingerprint: device.fingerprint.clone(),
            state: "waiting".into(),
            total_bytes: 0,
            done_bytes: 0,
            files_total: 0,
            files_done: 0,
            current: None,
            saved_to: None,
            saved_files: Vec::new(),
            error: None,
            message: None,
            clipboard,
            started: now_ms(),
            finished: None,
        });
        let handle = Arc::new(CancelHandle {
            flag: AtomicBool::new(false),
            stream: Mutex::new(None),
        });
        lock(&self.inner.cancels).insert(id.clone(), handle.clone());
        self.inner.emit(Event::Changed);
        let inner = self.inner.clone();
        let transfer_id = id.clone();
        thread::spawn(move || run_send(inner, transfer_id, peer, items, clipboard, handle));
        Ok(id)
    }

    /// Answer a waiting request.
    pub fn respond(&self, id: &str, accept: bool) -> bool {
        let pending = lock(&self.inner.pending).get(id).cloned();
        let Some(pending) = pending else { return false };
        *lock(&pending.decision) = Some(accept);
        pending.changed.notify_all();
        true
    }

    /// Cancel a transfer, either direction.
    pub fn cancel(&self, transfer_id: &str) -> bool {
        if let Some(handle) = lock(&self.inner.cancels).get(transfer_id).cloned() {
            handle.flag.store(true, Ordering::Relaxed);
            if let Some(stream) = lock(&handle.stream).as_ref() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
            return true;
        }
        receive::cancel_receive(&self.inner, transfer_id)
    }

    /// Forget a finished transfer.
    pub fn dismiss(&self, transfer_id: &str) {
        lock(&self.inner.transfers).retain(|t| t.id != transfer_id || t.is_open());
        self.inner.emit(Event::Changed);
    }

    /// Stop sharing. Pending requests are declined, transfers in flight are cancelled, and the
    /// acceptor and liveness threads are ended and joined: when this returns the listening socket
    /// is closed and its port can be bound again, and no thread of the service's own is left
    /// running. It returns promptly (a few milliseconds), never waiting on a peer. Connection
    /// handlers and liveness probes already running are not joined; they finish on their own
    /// within their socket timeouts. Calling it again, or dropping the `Service`, does nothing more.
    pub fn stop(&self) {
        self.shutdown();
    }

    /// [`Service::stop`], reporting whether every worker thread was joined.
    fn shutdown(&self) -> bool {
        self.inner.stop.store(true, Ordering::Relaxed);
        self.inner.park.stop();
        #[cfg(all(feature = "bonjour", target_os = "macos"))]
        {
            let handles = lock(&self.bonjour).take();
            drop(handles);
        }
        let pending: Vec<Arc<Pending>> = lock(&self.inner.pending).values().cloned().collect();
        for p in pending {
            *lock(&p.decision) = Some(false);
            p.changed.notify_all();
        }
        let handles: Vec<Arc<CancelHandle>> = lock(&self.inner.cancels).values().cloned().collect();
        for handle in handles {
            handle.flag.store(true, Ordering::Relaxed);
            if let Some(stream) = lock(&handle.stream).as_ref() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        for session in lock(&self.inner.sessions).values() {
            session.cancel.store(true, Ordering::Relaxed);
        }
        // No lock is held here: the threads being joined never take one that this thread holds.
        let workers = std::mem::take(&mut *lock(&self.workers));
        let mut all_ended = true;
        for worker in workers {
            all_ended &= worker.stop();
        }
        all_ended
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- background threads ----------------------------------------------------

struct ConnectionGuard(Arc<Inner>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Start the acceptor on a thread of its own. `port` is where `stop` connects to wake it.
fn spawn_acceptor(inner: Arc<Inner>, listener: TcpListener, port: u16) -> std::io::Result<Worker> {
    Worker::spawn(
        "rk-send-accept",
        move || wake_listener(port),
        move || accept_loop(&inner, listener),
    )
}

/// End a blocked `accept` by connecting to the listener from this process (see `workers.rs` for
/// why this and not `shutdown` or `close`). The connection is dropped at once; the acceptor sees
/// the stop flag and drops it too. A failed connect is harmless: `Worker::stop` tries again.
fn wake_listener(port: u16) {
    let address = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let _ = TcpStream::connect_timeout(&address, Duration::from_millis(250));
}

/// Connections this service serves at once; the next one is closed unread.
const MAX_CONNECTIONS: usize = 64;

/// The acceptor: blocks in `accept` and hands each connection to a handler thread of its own.
/// Owns the listener, so the port is released when this returns.
fn accept_loop(inner: &Arc<Inner>, listener: TcpListener) {
    while !inner.stop.load(Ordering::Relaxed) {
        let accepted = listener.accept();
        #[cfg(test)]
        inner.park.note_wake();
        match accepted {
            Ok((socket, address)) => {
                // `Service::stop` ends the blocking accept with a connection of its own.
                if inner.stop.load(Ordering::Relaxed) {
                    break;
                }
                let _ = socket.set_nonblocking(false);
                if inner.connections.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
                    continue;
                }
                inner.connections.fetch_add(1, Ordering::Relaxed);
                // Counted down however the handler ends, panics included.
                let guard = ConnectionGuard(inner.clone());
                thread::spawn(move || {
                    let guard = guard;
                    receive::handle_connection(&guard.0, socket, address);
                });
            }
            // A failed accept (descriptor exhaustion, a peer that reset before it was taken):
            // back off so a persistent fault cannot spin, and let a stop end the wait.
            Err(_) => {
                if inner.park.wait(Duration::from_millis(250)) {
                    break;
                }
            }
        }
    }
}

#[cfg(all(feature = "multicast", not(target_os = "ios")))]
fn spawn_discovery(inner: Arc<Inner>, socket: std::net::UdpSocket) {
    thread::spawn(move || {
        let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));
        let mut buffer = vec![0u8; 64 * 1024];
        let mut rejoined = Instant::now();
        while !inner.stop.load(Ordering::Relaxed) {
            if rejoined.elapsed() >= Duration::from_secs(30) {
                rejoined = Instant::now();
                discovery::rejoin(&socket);
            }
            let Ok((n, from)) = socket.recv_from(&mut buffer) else {
                continue;
            };
            let Some(heard) = discovery::parse(&buffer[..n], from) else {
                continue;
            };
            if !inner.upsert(&heard.info, heard.ip) {
                continue;
            }
            if heard.info.announce.unwrap_or(false) {
                let inner = inner.clone();
                thread::spawn(move || answer_announcement(&inner, &heard));
            }
        }
    });
}

#[cfg(all(feature = "multicast", not(target_os = "ios")))]
/// Reply to an announcement the way the protocol asks: register with the
/// sender over HTTP, and fall back to a multicast reply.
fn answer_announcement(inner: &Arc<Inner>, heard: &discovery::Heard) {
    let mut me = inner.me.clone();
    me.announce = None;
    let registered = serde_json::to_vec(&me).ok().is_some_and(|body| {
        net::request_json(
            heard.ip,
            heard.info.port,
            heard.info.protocol == "https",
            &heard.info.fingerprint,
            "POST",
            &format!("{}/register", proto::API),
            Some(&body),
            Duration::from_secs(3),
        )
        .is_ok_and(|reply| reply.status == 200)
    });
    if !registered {
        let _ = discovery::announce(&inner.me, inner.config().port, false);
    }
}

#[cfg(all(feature = "multicast", not(target_os = "ios")))]
fn spawn_announcer(inner: Arc<Inner>) {
    thread::spawn(move || {
        let mut waits = vec![100u64, 400, 1500].into_iter();
        while !inner.stop.load(Ordering::Relaxed) {
            let sent = discovery::announce(&inner.me, inner.config().port, true);
            inner.local_network.store(
                match &sent {
                    Ok(()) => 1,
                    // EHOSTUNREACH: what a denied Local Network permission looks like.
                    #[cfg(target_os = "macos")]
                    Err(e) if e.raw_os_error() == Some(65) => 2,
                    // WSAEACCES: Windows Firewall (or policy) refusing the multicast send.
                    #[cfg(windows)]
                    Err(e) if e.raw_os_error() == Some(10013) => 2,
                    Err(_) => 0,
                },
                Ordering::Relaxed,
            );
            inner.emit(Event::Changed);
            let wait = waits.next().unwrap_or(30_000);
            let until = Instant::now() + Duration::from_millis(wait);
            while Instant::now() < until && !inner.stop.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
}

/// Sweep the local networks shortly after start (multicast gets the first
/// word), when asked, and every minute while no device is known.
#[cfg(all(feature = "multicast", not(target_os = "ios")))]
fn spawn_scanner(inner: Arc<Inner>) {
    thread::spawn(move || {
        let mut due = Instant::now() + Duration::from_secs(2);
        let mut first = true;
        while !inner.stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
            let asked = inner.scan_requested.load(Ordering::Relaxed);
            let waited = Instant::now() >= due;
            if !asked && !waited {
                continue;
            }
            if asked || first || lock(&inner.devices).is_empty() {
                inner.scan();
                inner.scan_requested.store(false, Ordering::Relaxed);
                first = false;
            }
            due = Instant::now() + Duration::from_secs(60);
        }
    });
}

/// How often devices not heard from are probed.
const LIVENESS_PERIOD: Duration = Duration::from_secs(20);

/// Drop devices that stop answering `GET /info`.
fn spawn_liveness(inner: Arc<Inner>) -> std::io::Result<Worker> {
    // The wait is on `park`, which `Service::stop` has already stopped: nothing more to wake.
    Worker::spawn("rk-send-liveness", || {}, move || liveness_loop(&inner))
}

/// One timed wait per period, ended at once by a stop.
fn liveness_loop(inner: &Arc<Inner>) {
    while !inner.park.wait(LIVENESS_PERIOD) {
        let stale: Vec<Device> = lock(&inner.devices)
            .iter()
            .filter(|s| s.last_seen.elapsed() > Duration::from_secs(20))
            .map(|s| s.device.clone())
            .collect();
        for device in stale {
            let inner = inner.clone();
            thread::spawn(move || {
                let alive = device.ip.parse::<IpAddr>().ok().is_some_and(|ip| {
                    net::request_json(
                        ip,
                        device.port,
                        device.protocol == "https",
                        &device.fingerprint,
                        "GET",
                        &format!("{}/info", proto::API),
                        None,
                        Duration::from_secs(2),
                    )
                    .is_ok_and(|reply| reply.status == 200)
                });
                let removed = {
                    let mut devices = lock(&inner.devices);
                    match devices
                        .iter()
                        .position(|s| s.device.fingerprint == device.fingerprint)
                    {
                        Some(index) if alive => {
                            devices[index].last_seen = Instant::now();
                            devices[index].misses = 0;
                            false
                        }
                        Some(index) => {
                            devices[index].misses += 1;
                            if devices[index].misses >= 2 {
                                devices.remove(index);
                                true
                            } else {
                                false
                            }
                        }
                        None => false,
                    }
                };
                if removed {
                    inner.emit(Event::Devices);
                }
            });
        }
    }
}

fn run_send(
    inner: Arc<Inner>,
    id: String,
    peer: Peer,
    items: Vec<SendItem>,
    clipboard: bool,
    handle: Arc<CancelHandle>,
) {
    let finish = |state: &str, error: Option<Error>| {
        inner.end_transfer(&id, state, error);
        lock(&inner.cancels).remove(&id);
    };
    let entries = match send::expand(&items) {
        Ok(entries) => entries,
        Err(message) => return finish("failed", Some(message)),
    };
    inner.update_transfer(&id, true, |t| {
        t.files_total = entries.len();
        t.total_bytes = entries.iter().map(|e| e.size).sum();
    });
    let register = |stream: Option<TcpStream>| {
        *lock(&handle.stream) = stream;
    };
    // A message counts as sent once it is on the other screen.
    let delivered = std::cell::Cell::new(false);
    let result = send::deliver(
        &inner.me,
        &peer,
        &entries,
        clipboard,
        &handle.flag,
        &register,
        &mut |p| {
            if p.phase == send::Phase::Delivered {
                delivered.set(true);
                inner.end_transfer(&id, "done", None);
                return;
            }
            let waiting = p.phase == send::Phase::Waiting;
            inner.update_transfer(&id, p.total > 0 && p.done >= p.total, |t| {
                t.state = if waiting { "waiting" } else { "active" }.to_string();
                t.total_bytes = p.total;
                t.done_bytes = p.done;
                t.files_total = p.files_total;
                t.files_done = p.files_done;
                t.current = p.current.clone();
            });
        },
    );
    if delivered.get() {
        lock(&inner.cancels).remove(&id);
        return;
    }
    match result {
        Ok(send::Outcome::Done) => finish("done", None),
        Ok(send::Outcome::Declined) => finish("declined", None),
        Ok(send::Outcome::Cancelled) => finish("cancelled", None),
        Err(_) if handle.flag.load(Ordering::Relaxed) => finish("cancelled", None),
        Err(message) => finish("failed", Some(message)),
    }
}

// ---- resolved peers, host-fed or Bonjour ----------------------------------

/// The TXT value for `key`, if present.
fn txt_value(txt: &[(String, String)], key: &str) -> Option<String> {
    txt.iter()
        .find(|(k, _)| k.as_str() == key)
        .map(|(_, v)| v.clone())
}

/// A resolved device as an announcement would describe it. An empty alias
/// falls back to the start of the fingerprint.
fn resolved_info(
    fingerprint: &str,
    alias: &str,
    port: u16,
    txt: &[(String, String)],
) -> DeviceInfo {
    let alias = if alias.is_empty() {
        fingerprint.chars().take(8).collect()
    } else {
        alias.to_string()
    };
    DeviceInfo {
        alias,
        version: proto::VERSION.to_string(),
        device_model: txt_value(txt, "model").filter(|m| !m.is_empty()),
        device_type: txt_value(txt, "type").filter(|t| !t.is_empty()),
        fingerprint: fingerprint.to_string(),
        port,
        protocol: "https".to_string(),
        download: false,
        announce: None,
    }
}

/// This device's IPv4 addresses, for telling its own announcements apart. Empty
/// where multicast discovery is not compiled in.
#[cfg(all(feature = "multicast", not(target_os = "ios")))]
fn local_ipv4s() -> Vec<Ipv4Addr> {
    discovery::local_ipv4s()
}

#[cfg(not(all(feature = "multicast", not(target_os = "ios"))))]
fn local_ipv4s() -> Vec<Ipv4Addr> {
    Vec::new()
}

impl Inner {
    /// Record a device a resolver found, with its `px` flag. Returns whether it
    /// was taken (not this device itself).
    fn add_resolved(&self, info: &DeviceInfo, ip: IpAddr, px: bool) -> bool {
        if !self.upsert(info, ip) {
            return false;
        }
        let fingerprint = normalize(&info.fingerprint);
        let changed = {
            let mut devices = lock(&self.devices);
            match devices
                .iter_mut()
                .find(|s| s.device.fingerprint == fingerprint)
            {
                Some(seen) if seen.device.px != px => {
                    seen.device.px = px;
                    true
                }
                _ => false,
            }
        };
        if changed {
            self.emit(Event::Devices);
        }
        true
    }

    /// Drop a device from the list. Returns whether it was listed.
    fn remove_device(&self, fingerprint: &str) -> bool {
        let removed = {
            let mut devices = lock(&self.devices);
            let before = devices.len();
            devices.retain(|s| s.device.fingerprint != fingerprint);
            devices.len() != before
        };
        if removed {
            self.emit(Event::Devices);
        }
        removed
    }
}

#[cfg(all(feature = "bonjour", target_os = "macos"))]
impl Inner {
    fn bonjour_found(&self, found: bonjour::Found) {
        let Some(fingerprint) = txt_value(&found.txt, "fp")
            .map(|f| normalize(&f))
            .filter(|f| net::valid_pin(f))
        else {
            return;
        };
        let alias = txt_value(&found.txt, "alias").unwrap_or_default();
        let info = resolved_info(&fingerprint, &alias, found.port, &found.txt);
        let px = txt_value(&found.txt, "px").as_deref() == Some("1");
        if self.add_resolved(&info, found.host, px) {
            lock(&self.bonjour_names).insert(found.instance, fingerprint);
        }
    }

    fn bonjour_lost(&self, instance: &str) {
        let fingerprint = lock(&self.bonjour_names).remove(instance);
        if let Some(fingerprint) = fingerprint {
            self.remove_device(&fingerprint);
        }
    }
}

#[cfg(all(feature = "bonjour", target_os = "macos"))]
fn start_bonjour(
    inner: &Arc<Inner>,
    config: &Config,
    fingerprint: &str,
) -> Option<(bonjour::Advertiser, bonjour::Browser)> {
    let model = Some(config.device_model.as_str()).filter(|m| !m.is_empty());
    let txt = bonjour::txt_encode(fingerprint, &config.alias, &config.device_type, model);
    let advertiser = match bonjour::Advertiser::start(&config.alias, config.port, &txt) {
        Ok(advertiser) => advertiser,
        Err(e) => {
            lock(&inner.warnings).push(Error::DiscoveryUnavailable {
                detail: e.to_string(),
            });
            return None;
        }
    };
    let found = inner.clone();
    let lost = inner.clone();
    match bonjour::Browser::start(
        config.alias.clone(),
        Box::new(move |peer| found.bonjour_found(peer)),
        Box::new(move |instance| lost.bonjour_lost(&instance)),
    ) {
        Ok(browser) => Some((advertiser, browser)),
        Err(e) => {
            lock(&inner.warnings).push(Error::DiscoveryUnavailable {
                detail: e.to_string(),
            });
            None
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::io::{ErrorKind, Read};
    use std::net::SocketAddr;

    /// `Service::start` sets the client identity and User-Agent for the whole process, so these
    /// tests run one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> MutexGuard<'static, ()> {
        lock(&SERIAL)
    }

    fn config(port: u16) -> Config {
        let root = std::env::temp_dir().join(format!("rk-send-life-{}", proto::random_hex(4)));
        Config {
            product_name: "Test".into(),
            alias: "Lifecycle".into(),
            port,
            device_type: "desktop".into(),
            device_model: "Test".into(),
            save_dir: root.join("save"),
            trust_path: root.join("nearby_devices.json"),
            user_agent: "LifecycleTest/1".into(),
            quarantine_agent: "LifecycleTest".into(),
            temp_suffix: ".test-part".into(),
            log_prefix: "test sharing:".into(),
            policy: Policy::default(),
        }
    }

    fn start_on(port: u16) -> Result<Service, Error> {
        let identity = Identity::generate("Lifecycle").expect("identity");
        Service::start(config(port), identity, Arc::new(|_: Event| {}))
    }

    #[test]
    fn an_idle_service_wakes_its_threads_about_once_a_second_at_most() {
        let _serial = serial();
        let service = start_on(0).expect("service starts");
        let (began, woke_before) = (Instant::now(), service.inner.park.wakes());
        thread::sleep(Duration::from_secs(2));
        let (secs, woke) = (
            began.elapsed().as_secs_f64(),
            service.inner.park.wakes() - woke_before,
        );
        // The window can stretch on a loaded machine and the allowance stretches with it. The 50 ms
        // accept poll and 100 ms liveness tick this replaces woke about 60 times in two seconds.
        let allowed = 3u64.max(secs.ceil() as u64 + 1);
        assert!(
            woke <= allowed,
            "the service woke {woke} times in {secs:.1} s"
        );
        assert!(service.shutdown());
    }

    #[test]
    fn stop_returns_promptly_having_joined_every_service_thread() {
        let _serial = serial();
        let service = start_on(0).expect("service starts");
        // Let both threads reach their waits; stopping before they do ends them at once too.
        thread::sleep(Duration::from_millis(100));
        assert_eq!(lock(&service.workers).len(), 2, "acceptor and liveness");
        let began = Instant::now();
        assert!(service.shutdown(), "a service thread did not end");
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "stop took {:?}",
            began.elapsed()
        );
        assert!(lock(&service.workers).is_empty());
        // Stopping again, and the drop that follows, change nothing.
        assert!(service.shutdown());
        service.stop();
    }

    #[test]
    fn a_second_start_on_the_same_port_succeeds_after_stop() {
        let _serial = serial();
        let first = start_on(0).expect("service starts");
        let port = first.bound_port();
        assert!(first.shutdown());
        for round in 0..3 {
            let again = start_on(port)
                .unwrap_or_else(|e| panic!("restart {round} on port {port} failed: {e:?}"));
            assert_eq!(again.bound_port(), port);
            assert!(again.shutdown());
        }
    }

    #[test]
    fn the_64_connection_cap_still_closes_the_next_connection_unread() {
        let _serial = serial();
        let service = start_on(0).expect("service starts");
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, service.bound_port()));
        let mut byte = [0u8; 1];

        // At the cap the connection is closed without a byte exchanged: end of stream or a reset,
        // never a timeout.
        service
            .inner
            .connections
            .store(MAX_CONNECTIONS, Ordering::Relaxed);
        let mut full = TcpStream::connect(address).expect("connect");
        full.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        match full.read(&mut byte) {
            Ok(0) => {}
            Err(e) if !matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            other => panic!("a connection over the cap must be closed, got {other:?}"),
        }

        // Below the cap it is kept: the handler waits for a TLS hello (30 s timeout), so a short
        // read here times out.
        service.inner.connections.store(0, Ordering::Relaxed);
        let mut open = TcpStream::connect(address).expect("connect");
        open.set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        match open.read(&mut byte) {
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            other => panic!("a connection under the cap must be kept open, got {other:?}"),
        }
        drop(open);
        assert!(service.shutdown());
    }
}
