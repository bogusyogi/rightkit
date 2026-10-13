use super::caller::{Caller, Scope};
use crate::error::{ServiceError, ERROR_FORBIDDEN, ERROR_NOT_ALLOWED};
use crate::record::Allowlist;
use std::collections::{BTreeMap, BTreeSet};

/// Which transports a method is reachable on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Transport {
    /// Local callers only.
    Local,
    /// Remote devices only.
    Remote,
    /// Both.
    Both,
}

/// Policy for one method id: which local apps may call it and which scopes a device needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodPolicy {
    transport: Transport,
    /// Local apps allowed; `"*"` means any app the table knows.
    apps: BTreeSet<String>,
    /// A device needs at least one of these scopes (empty: any enrolled device).
    scopes: BTreeSet<Scope>,
}

impl MethodPolicy {
    /// Local-only, for the given apps (`"*"` for any).
    pub fn local<I, S>(apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            transport: Transport::Local,
            apps: apps.into_iter().map(Into::into).collect(),
            scopes: BTreeSet::new(),
        }
    }

    /// Remote-only, for devices holding at least one of `scopes`.
    pub fn remote<I, S>(scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Scope>,
    {
        Self {
            transport: Transport::Remote,
            apps: BTreeSet::new(),
            scopes: scopes.into_iter().map(Into::into).collect(),
        }
    }

    /// Add local apps; the method becomes reachable on both transports if it was remote-only.
    pub fn and_local<I, S>(mut self, apps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.apps.extend(apps.into_iter().map(Into::into));
        self.transport = match self.transport {
            Transport::Remote => Transport::Both,
            t => t,
        };
        self
    }

    /// Add device scopes; the method becomes reachable on both transports if it was local-only.
    pub fn and_remote<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Scope>,
    {
        self.scopes.extend(scopes.into_iter().map(Into::into));
        self.transport = match self.transport {
            Transport::Local => Transport::Both,
            t => t,
        };
        self
    }

    pub fn transport(&self) -> Transport {
        self.transport
    }

    fn permits_scopes(&self, held: &BTreeSet<Scope>) -> bool {
        self.transport != Transport::Local
            && (self.scopes.is_empty() || self.scopes.iter().any(|s| held.contains(s)))
    }
}

/// Local-transport authorization against an [`Allowlist`] (the form the host config carries and
/// `service.json` publishes); same answers as [`MethodTable::authorize`] for a local app.
pub(crate) fn authorize_allowlist(
    list: &Allowlist,
    caller: &Caller,
    method: &str,
) -> Result<(), ServiceError> {
    let ok = match caller {
        Caller::LocalApp { app, .. } => list.permits(app, method),
        Caller::Device { .. } => method == "ping",
    };
    if ok {
        Ok(())
    } else {
        Err(ServiceError::new(
            ERROR_NOT_ALLOWED,
            format!("{} may not call {method}", caller.label()),
        ))
    }
}

/// Every method of a service with its policy: the one source the local allowlist and a remote
/// route table are both derived from. `ping` is never listed and always allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MethodTable {
    methods: BTreeMap<String, MethodPolicy>,
}

impl MethodTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace a method.
    pub fn method(mut self, id: impl Into<String>, policy: MethodPolicy) -> Self {
        self.methods.insert(id.into(), policy);
        self
    }

    pub fn get(&self, id: &str) -> Option<&MethodPolicy> {
        self.methods.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.methods.keys().map(String::as_str)
    }

    /// The local allowlist this table implies: each local-reachable method, per app. A `"*"`
    /// app entry stays `"*"` and, as in [`Allowlist`], applies only to apps with no entry of
    /// their own.
    pub fn allowlist(&self) -> Allowlist {
        let mut a = Allowlist::new();
        for (id, p) in &self.methods {
            if p.transport == Transport::Remote {
                continue;
            }
            for app in &p.apps {
                a = a.allow(app, [id.clone()]);
            }
        }
        a
    }

    /// Whether `caller` may call `method`. `ping` is always allowed. A local app is refused with
    /// `request_not_allowed`; a device lacking scope with `forbidden`.
    pub fn authorize(&self, caller: &Caller, method: &str) -> Result<(), ServiceError> {
        if method == "ping" {
            return Ok(());
        }
        let policy = self.methods.get(method);
        match caller {
            Caller::LocalApp { .. } => authorize_allowlist(&self.allowlist(), caller, method),
            Caller::Device { id, scopes } => {
                if policy.is_some_and(|p| p.permits_scopes(scopes)) {
                    Ok(())
                } else {
                    Err(ServiceError::new(
                        ERROR_FORBIDDEN,
                        format!("{id} may not call {method}"),
                    ))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer::PeerIdentity;

    fn table() -> MethodTable {
        MethodTable::new()
            .method("echo", MethodPolicy::local(["*"]))
            .method("both", MethodPolicy::local(["a"]).and_remote(["s1"]))
            .method("up", MethodPolicy::remote(["s2"]))
    }

    #[test]
    fn allowlist_is_derived_and_skips_remote_only() {
        let a = table().allowlist();
        assert!(a.permits("any", "echo"));
        assert!(a.permits("a", "both"));
        assert!(!a.permits("b", "both"));
        assert!(!a.permits("a", "up"));
        assert!(a.permits("b", "ping"));
    }

    #[test]
    fn authorize_by_caller() {
        let t = table();
        let app = |n: &str| Caller::local_app(n, PeerIdentity::default());
        assert!(t.authorize(&app("a"), "both").is_ok());
        assert_eq!(
            t.authorize(&app("b"), "both").unwrap_err().code,
            ERROR_NOT_ALLOWED
        );
        assert_eq!(
            t.authorize(&app("a"), "up").unwrap_err().code,
            ERROR_NOT_ALLOWED
        );
        let dev = Caller::device("dev_1", ["s1"]);
        assert!(t.authorize(&dev, "both").is_ok());
        assert_eq!(t.authorize(&dev, "up").unwrap_err().code, ERROR_FORBIDDEN);
        assert_eq!(t.authorize(&dev, "echo").unwrap_err().code, ERROR_FORBIDDEN);
        assert_eq!(
            t.authorize(&dev, "missing").unwrap_err().code,
            ERROR_FORBIDDEN
        );
        assert!(t.authorize(&dev, "ping").is_ok());
    }
}
