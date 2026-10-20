//! `Service::builder()`: one [`Handler`] and one [`MethodTable`], hosted on the local transport
//! and, with the `remote` feature, offered to signed devices as `POST /v1/call/{method}`.
//!
//! ```text
//! let service = Service::builder()
//!     .methods(MethodTable::new().method("echo", MethodPolicy::local(["app"]).and_remote(["venture"])))
//!     .handler(Arc::new(MyHandler))
//!     .local(&suite, ServiceConfig::new("svc", "role", 5, "1.0", Allowlist::new()), HostOptions::new())
//!     .remote(DaemonConfig::new(state_dir))
//!     .start()?;
//! // Local callers use `Client`; serve `service.daemon()` over TLS with `remote::serve_blocking`.
//! ```
//!
//! The method table decides which transport each method is on, so a `transport: both` method is
//! reachable by [`Client::call`](crate::Client::call) and by a signed device with the same params
//! and the handler's own error codes. Methods that are remote-only by nature (`upload.chunk`,
//! `oauth.callback`, `webhook.inbound`) and the existing v1 routes keep their own routes.

use std::sync::Arc;

use crate::contract::{Handler, MethodTable, TableError};
use crate::registration::{Registration, RegistrationError, TransportProfile};
use crate::server::{host_with_options, HostError, HostOptions, Hosted, ServiceConfig};
use crate::suite::Suite;

#[cfg(feature = "remote")]
use crate::remote::{Clock, Daemon, DaemonConfig, DeviceCalls, OAuthCallbackHandler, SystemClock};

/// Why [`ServiceBuilder::start`] failed.
#[derive(Debug)]
pub enum BuildError {
    /// A required part was not given (`methods`, `handler`).
    Missing(&'static str),
    /// Neither `local` nor `remote` was configured.
    NoTransport,
    /// The method table was refused.
    Table(TableError),
    /// The local host did not start.
    Host(Box<HostError>),
    /// The remote daemon did not open its state.
    Io(std::io::Error),
    /// A [`Registration`] was refused (see [`RegistrationError`]).
    Registration(RegistrationError),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Missing(what) => write!(f, "service builder: {what} not set"),
            BuildError::NoTransport => f.write_str("service builder: no transport configured"),
            BuildError::Table(e) => write!(f, "service builder: {e}"),
            BuildError::Host(e) => write!(f, "service builder: local host: {e}"),
            BuildError::Io(e) => write!(f, "service builder: remote state: {e}"),
            BuildError::Registration(e) => write!(f, "service builder: {e}"),
        }
    }
}

impl std::error::Error for BuildError {}

struct LocalSpec {
    suite: Suite,
    cfg: ServiceConfig,
    opts: HostOptions,
}

#[cfg(feature = "remote")]
struct RemoteSpec {
    cfg: DaemonConfig,
    clock: Arc<dyn Clock>,
    oauth: Option<Arc<dyn OAuthCallbackHandler>>,
}

/// A running service: the local host, the remote daemon, or both, over one handler.
pub struct Service {
    table: MethodTable,
    hosted: Option<Hosted>,
    #[cfg(feature = "remote")]
    daemon: Option<Arc<Daemon>>,
}

impl Service {
    pub fn builder() -> ServiceBuilder {
        ServiceBuilder::default()
    }

    pub fn methods(&self) -> &MethodTable {
        &self.table
    }

    /// The local transport host, when configured. Dropping the service releases it.
    pub fn local(&self) -> Option<&Hosted> {
        self.hosted.as_ref()
    }

    /// The remote daemon, when configured. Serve it over TLS with
    /// [`remote::serve`](crate::remote::serve) or [`remote::serve_blocking`](crate::remote::serve_blocking).
    #[cfg(feature = "remote")]
    pub fn daemon(&self) -> Option<&Arc<Daemon>> {
        self.daemon.as_ref()
    }

    /// Release the local host. The remote listener stops with its own `StopHandle`.
    pub fn shutdown(&self) {
        if let Some(h) = &self.hosted {
            h.shutdown();
        }
    }
}

#[derive(Default)]
pub struct ServiceBuilder {
    table: Option<MethodTable>,
    handler: Option<Arc<dyn Handler>>,
    local: Option<LocalSpec>,
    profile: Option<TransportProfile>,
    #[cfg(feature = "remote")]
    remote: Option<RemoteSpec>,
}

impl ServiceBuilder {
    /// Declare the whole service as one [`Registration`] (identity, method descriptors and a
    /// handler per method) and host it on the local transport. Replaces `methods`, `handler` and
    /// `local`; the `remote` calls stay available only for a registration whose profile is
    /// [`TransportProfile::LocalAndRemote`] (`start` refuses them otherwise). Opens no listener
    /// beyond the current-user-only local endpoint.
    pub fn register(
        mut self,
        suite: &Suite,
        registration: Registration,
        opts: HostOptions,
    ) -> Result<Self, BuildError> {
        let (identity, profile, table, handler) = registration
            .into_parts()
            .map_err(BuildError::Registration)?;
        self.table = Some(table);
        self.handler = Some(handler);
        self.local = Some(LocalSpec {
            suite: suite.clone(),
            cfg: identity.to_config(),
            opts,
        });
        self.profile = Some(profile);
        Ok(self)
    }

    /// The one source of method policy for every transport.
    pub fn methods(mut self, table: MethodTable) -> Self {
        self.table = Some(table);
        self
    }

    /// The one handler both transports call.
    pub fn handler(mut self, handler: Arc<dyn Handler>) -> Self {
        self.handler = Some(handler);
        self
    }

    /// Host on the local transport. The allowlist in `cfg` is replaced by the one the method
    /// table implies.
    pub fn local(mut self, suite: &Suite, cfg: ServiceConfig, opts: HostOptions) -> Self {
        self.local = Some(LocalSpec {
            suite: suite.clone(),
            cfg,
            opts,
        });
        self
    }

    /// Open the remote daemon with the system clock and no OAuth handler.
    #[cfg(feature = "remote")]
    pub fn remote(self, cfg: DaemonConfig) -> Self {
        self.remote_with(cfg, Arc::new(SystemClock), None)
    }

    /// [`remote`](Self::remote) with an injected clock and an OAuth callback handler.
    #[cfg(feature = "remote")]
    pub fn remote_with(
        mut self,
        cfg: DaemonConfig,
        clock: Arc<dyn Clock>,
        oauth: Option<Arc<dyn OAuthCallbackHandler>>,
    ) -> Self {
        self.remote = Some(RemoteSpec { cfg, clock, oauth });
        self
    }

    /// Open the configured transports. The remote state opens first (it fails without side
    /// effects); the local host publishes `service.json` last.
    pub fn start(self) -> Result<Service, BuildError> {
        let table = self.table.ok_or(BuildError::Missing("methods"))?;
        let handler = self.handler.ok_or(BuildError::Missing("handler"))?;
        #[cfg(feature = "remote")]
        let has_remote = self.remote.is_some();
        #[cfg(not(feature = "remote"))]
        let has_remote = false;
        // A local-only registration never gets a remote daemon, whichever order the calls came in.
        if has_remote && self.profile == Some(TransportProfile::Local) {
            return Err(BuildError::Registration(
                RegistrationError::RemoteNotDeclared,
            ));
        }
        if self.local.is_none() && !has_remote {
            return Err(BuildError::NoTransport);
        }

        #[cfg(feature = "remote")]
        let daemon = match self.remote {
            Some(r) => {
                let d = Daemon::open(r.cfg, r.clock, r.oauth)
                    .map_err(BuildError::Io)?
                    .with_calls(DeviceCalls::new(table.clone(), handler.clone()));
                Some(Arc::new(d))
            }
            None => None,
        };

        let hosted = match self.local {
            Some(l) => {
                let cfg = l.cfg.with_methods(&table);
                Some(
                    host_with_options(&l.suite, cfg, handler, l.opts)
                        .map_err(|e| BuildError::Host(Box::new(e)))?,
                )
            }
            None => None,
        };
        Ok(Service {
            table,
            hosted,
            #[cfg(feature = "remote")]
            daemon,
        })
    }
}
