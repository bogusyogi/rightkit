//! `Service::builder()`: one [`Handler`] and one [`MethodTable`], hosted on the local transport.
//!
//! ```text
//! let service = Service::builder()
//!     .methods(MethodTable::new().method("echo", MethodPolicy::local(["app"]).and_remote(["venture"])))
//!     .handler(handler.clone())
//!     .local(&suite, ServiceConfig::new("svc", "role", 5, "1.0", Allowlist::new()), HostOptions::new())
//!     .start()?;
//! // Signed devices (rightkit-daemon-remote, feature `remote`) reach the same handler and table:
//! // Daemon::open(cfg, clock, None)?.with_calls(DeviceCalls::new(service.methods().clone(), handler))
//! ```
//!
//! The method table decides which transport each method is on, so a `transport: both` method is
//! reachable by [`Client::call`](crate::Client::call) and, through `rightkit-daemon-remote`, by a
//! signed device with the same params and the handler's own error codes. Until 0.7.0 this builder
//! also opened that remote daemon (`ServiceBuilder::remote`); the remote host is its own crate now.

use std::sync::Arc;

use crate::contract::{Handler, MethodTable, TableError};
use crate::registration::{Registration, RegistrationError};
use crate::server::{host_with_options, HostError, HostOptions, Hosted, ServiceConfig};
use rightkit_roots::Suite;

/// Why [`ServiceBuilder::start`] failed.
#[derive(Debug)]
pub enum BuildError {
    /// A required part was not given (`methods`, `handler`).
    Missing(&'static str),
    /// `local` was not configured.
    NoTransport,
    /// The method table was refused.
    Table(TableError),
    /// The local host did not start.
    Host(Box<HostError>),
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

/// A running service: the local host over one handler.
pub struct Service {
    table: MethodTable,
    hosted: Option<Hosted>,
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

    /// Release the local host.
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
}

impl ServiceBuilder {
    /// Declare the whole service as one [`Registration`] (identity, method descriptors and a
    /// handler per method) and host it on the local transport. Replaces `methods`, `handler` and
    /// `local`. Opens no listener beyond the current-user-only local endpoint.
    pub fn register(
        mut self,
        suite: &Suite,
        registration: Registration,
        opts: HostOptions,
    ) -> Result<Self, BuildError> {
        let (identity, _profile, table, handler) = registration
            .into_parts()
            .map_err(BuildError::Registration)?;
        self.table = Some(table);
        self.handler = Some(handler);
        self.local = Some(LocalSpec {
            suite: suite.clone(),
            cfg: identity.to_config(),
            opts,
        });
        Ok(self)
    }

    /// The one source of method policy for every transport.
    pub fn methods(mut self, table: MethodTable) -> Self {
        self.table = Some(table);
        self
    }

    /// The one handler every transport calls.
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

    /// Open the local host; it publishes `service.json` last.
    pub fn start(self) -> Result<Service, BuildError> {
        let table = self.table.ok_or(BuildError::Missing("methods"))?;
        let handler = self.handler.ok_or(BuildError::Missing("handler"))?;
        let l = self.local.ok_or(BuildError::NoTransport)?;
        let cfg = l.cfg.with_methods(&table);
        let hosted = host_with_options(&l.suite, cfg, handler, l.opts)
            .map_err(|e| BuildError::Host(Box::new(e)))?;
        Ok(Service {
            table,
            hosted: Some(hosted),
        })
    }
}
