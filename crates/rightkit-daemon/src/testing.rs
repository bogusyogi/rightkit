//! Test fixtures for consumers of the [`client`](crate::client): a TLS test
//! certificate generated at run time (no private key is ever committed) and an
//! in-process daemon on `127.0.0.1:0`. Behind the `testing` feature; not for
//! production use.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::client::{Client, ClientConfig, Error};
use crate::{load_tls, serve_blocking, Daemon, DaemonConfig, StopHandle, SystemClock};

/// A fresh self-signed certificate for `localhost` and `127.0.0.1`, generated with rcgen on
/// every call. The certificate PEM doubles as the root a client pins.
pub struct TestCertificate {
    pub cert_pem: String,
    pub key_pem: String,
}

impl TestCertificate {
    pub fn generate() -> io::Result<Self> {
        let ck = rcgen::generate_simple_self_signed(vec![
            "localhost".to_string(),
            "127.0.0.1".to_string(),
        ])
        .map_err(io::Error::other)?;
        Ok(Self {
            cert_pem: ck.cert.pem(),
            key_pem: ck.key_pair.serialize_pem(),
        })
    }
}

/// A running daemon on a loopback port, stopped and joined when dropped.
pub struct TestDaemon {
    /// The bound `127.0.0.1` address.
    pub addr: SocketAddr,
    /// PEM of the daemon's certificate: pass it to [`ClientConfig::root_pem`].
    pub root_pem: String,
    /// A single-use enrolment code for all three scopes (`venture`, `channel`, `account`).
    pub enrolment_code: String,
    /// The daemon itself, for issuing more codes or inspecting events and committed objects.
    pub daemon: Arc<Daemon>,
    stop: StopHandle,
    thread: Option<JoinHandle<io::Result<()>>>,
    _state_dir: tempfile::TempDir,
}

/// Upload bounds small enough for tests: 4 byte minimum chunk, 1 MiB maximum, 64 MiB objects.
pub fn test_config(state_dir: &std::path::Path) -> DaemonConfig {
    let mut cfg = DaemonConfig::new(state_dir);
    cfg.limits.chunk_min = 4;
    cfg.limits.chunk_max = 1 << 20;
    cfg.limits.max_object_bytes = 64 << 20;
    cfg
}

impl TestDaemon {
    /// Start a daemon with [`test_config`] limits.
    pub fn start() -> io::Result<Self> {
        Self::start_with(|_| {})
    }

    /// Start a daemon, letting `tweak` change the [`test_config`] before it opens.
    pub fn start_with(tweak: impl FnOnce(&mut DaemonConfig)) -> io::Result<Self> {
        let dir = tempfile::tempdir()?;
        let mut cfg = test_config(dir.path());
        tweak(&mut cfg);
        let daemon = Arc::new(Daemon::open(cfg, Arc::new(SystemClock), None)?);
        let cert = TestCertificate::generate()?;
        let tls = load_tls(cert.cert_pem.as_bytes(), cert.key_pem.as_bytes())?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let stop = StopHandle::new();
        let thread = {
            let (daemon, stop) = (daemon.clone(), stop.clone());
            std::thread::Builder::new()
                .name("rightkit-test-daemon".into())
                .spawn(move || serve_blocking(daemon, listener, tls, ready_tx, stop))?
        };
        let addr = match ready_rx.recv_timeout(Duration::from_secs(20)) {
            Ok(addr) => addr,
            Err(_) => {
                stop.stop();
                // A server that failed to start reports why; one that is merely slow is stopped.
                return match thread.join() {
                    Ok(Err(e)) => Err(e),
                    _ => Err(io::Error::other("test daemon did not report its address")),
                };
            }
        };
        let enrolment_code =
            daemon.issue_enrolment_code(&["venture", "channel", "account"], 600)?;
        Ok(Self {
            addr,
            root_pem: cert.cert_pem,
            enrolment_code,
            daemon,
            stop,
            thread: Some(thread),
            _state_dir: dir,
        })
    }

    /// Client config for this daemon, pinned to its certificate.
    pub fn client_config(&self) -> Result<ClientConfig, Error> {
        ClientConfig::new(self.addr).root_pem(self.root_pem.as_bytes())
    }

    pub fn client(&self) -> Result<Client, Error> {
        Client::new(self.client_config()?)
    }

    /// Issue another single-use enrolment code with the given scopes.
    pub fn issue_code(&self, scopes: &[&str]) -> io::Result<String> {
        self.daemon.issue_enrolment_code(scopes, 600)
    }

    /// A handle that stops the server; the daemon is also stopped when `self` is dropped.
    pub fn stop_handle(&self) -> StopHandle {
        self.stop.clone()
    }

    /// Stop now and return the server's result.
    pub fn stop(mut self) -> io::Result<()> {
        self.stop.stop();
        match self.thread.take() {
            Some(t) => t
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("test daemon thread panicked"))),
            None => Ok(()),
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.stop.stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
