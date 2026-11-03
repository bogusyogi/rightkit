use crate::peer::PeerIdentity;
use std::collections::BTreeSet;

/// A capability a device holds (from enrolment). A plain label compared for equality.
pub type Scope = String;

/// A scope that names a role instead of a capability: `role:<name>`, for example `role:operator`.
///
/// A device's role is whatever its enrolment granted. Scopes are stored with the device record
/// when it enrols (by an enrolment code or a pairing window the owner issued) and read back after
/// the request's signature verified, so a role taken from [`Caller::role`] is the role of the
/// signing device: no environment variable, header or request field can set it. Gate a method on
/// a role by naming the role scope in its [`MethodPolicy`](crate::contract::MethodPolicy):
/// `MethodPolicy::remote(["role:operator"])`.
pub const ROLE_SCOPE_PREFIX: &str = "role:";

/// The scope that grants `role`.
pub fn role_scope(role: &str) -> Scope {
    format!("{ROLE_SCOPE_PREFIX}{role}")
}

/// The one role among `scopes`: `Some` only when exactly one scope is a role scope with a
/// non-empty name. Two roles, or none, give `None`, so an ambiguous device never gets a role by
/// the luck of an iteration order.
pub fn role_of<'a>(scopes: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut found = None;
    for scope in scopes {
        if let Some(name) = scope.strip_prefix(ROLE_SCOPE_PREFIX) {
            if name.is_empty() || found.is_some() {
                return None;
            }
            found = Some(name);
        }
    }
    found
}

/// Who is making a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// A local app on this machine. `app` is the label declared in `hello` (a claim, an allowlist
    /// key); `peer` is what the OS verified about the other end.
    LocalApp { app: String, peer: PeerIdentity },
    /// A remote device identified by its id, with the scopes it was enrolled with.
    Device { id: String, scopes: BTreeSet<Scope> },
}

impl Caller {
    pub fn local_app(app: impl Into<String>, peer: PeerIdentity) -> Self {
        Caller::LocalApp {
            app: app.into(),
            peer,
        }
    }

    pub fn device<I, S>(id: impl Into<String>, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Scope>,
    {
        Caller::Device {
            id: id.into(),
            scopes: scopes.into_iter().map(Into::into).collect(),
        }
    }

    /// The OS-verified peer, for a local caller only.
    pub fn peer(&self) -> Option<&PeerIdentity> {
        match self {
            Caller::LocalApp { peer, .. } => Some(peer),
            Caller::Device { .. } => None,
        }
    }

    /// The role of a device caller (see [`ROLE_SCOPE_PREFIX`]); `None` for a local app, for a
    /// device with no role scope and for one with more than one.
    pub fn role(&self) -> Option<&str> {
        match self {
            Caller::LocalApp { .. } => None,
            Caller::Device { scopes, .. } => role_of(scopes.iter().map(String::as_str)),
        }
    }

    /// The app label for a local caller, the device id for a device.
    pub fn label(&self) -> &str {
        match self {
            Caller::LocalApp { app, .. } => app,
            Caller::Device { id, .. } => id,
        }
    }
}
