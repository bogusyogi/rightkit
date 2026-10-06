//! Reserve-before-spawn resource authority (ported from CodeRight KRN-008/009
//! `process_resource_authority` + `runtime_resources::governor`, decoupled).
//! Callers reserve capacity first; a [`Reservation`] is held for the child's
//! lifetime. `Strict` denies every process-backed kind with a typed error.

use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResourceProfile {
    /// No process-backed capability is available.
    Strict,
    /// One active external execution group; small per-kind ceilings.
    #[default]
    Practical,
    /// Larger but finite pools.
    Expanded,
}

impl ResourceProfile {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "strict" => Ok(Self::Strict),
            "practical" | "default" | "local" => Ok(Self::Practical),
            "expanded" | "service" => Ok(Self::Expanded),
            other => Err(format!(
                "unknown resource profile {other:?}; expected strict, practical, or expanded"
            )),
        }
    }
    fn aggregate(self) -> usize {
        match self {
            Self::Strict => 0,
            Self::Practical => 1,
            Self::Expanded => 16,
        }
    }
    fn limit(self, kind: ResourceKind) -> usize {
        match (self, kind) {
            (Self::Strict, _) => 0,
            (Self::Practical, ResourceKind::ChildProcess) => 4,
            (Self::Practical, ResourceKind::BackgroundProcess) => 2,
            (Self::Expanded, ResourceKind::ChildProcess) => 32,
            (Self::Expanded, ResourceKind::BackgroundProcess) => 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// Short-lived confined child.
    ChildProcess,
    /// Long-lived process (PTY sessions, services).
    BackgroundProcess,
}

impl ResourceKind {
    fn idx(self) -> usize {
        match self {
            Self::ChildProcess => 0,
            Self::BackgroundProcess => 1,
        }
    }
}

/// Typed denial; no process exists when this is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDenied {
    pub kind: ResourceKind,
    pub profile: ResourceProfile,
    pub limit: usize,
    pub in_use: usize,
}

impl ResourceDenied {
    pub fn code(&self) -> &'static str {
        "resource_admission_denied"
    }
}

impl std::fmt::Display for ResourceDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: {:?} denied under {:?} profile (limit {}, in use {})",
            self.code(),
            self.kind,
            self.profile,
            self.limit,
            self.in_use
        )
    }
}
impl std::error::Error for ResourceDenied {}

#[derive(Debug, Default)]
struct Counts {
    aggregate: usize,
    per_kind: [usize; 2],
}

/// Shared, cloneable authority. Clone it into every launcher of one host.
#[derive(Debug, Clone)]
pub struct ResourceAuthority {
    profile: ResourceProfile,
    counts: Arc<Mutex<Counts>>,
}

impl ResourceAuthority {
    pub fn new(profile: ResourceProfile) -> Self {
        Self {
            profile,
            counts: Arc::default(),
        }
    }
    pub fn profile(&self) -> ResourceProfile {
        self.profile
    }
    /// Reserve one unit of `kind` (plus one aggregate execution slot).
    pub fn reserve(&self, kind: ResourceKind) -> Result<Reservation, ResourceDenied> {
        let mut c = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let limit = self.profile.limit(kind);
        let denied = |c: &Counts, limit| ResourceDenied {
            kind,
            profile: self.profile,
            limit,
            in_use: c.per_kind[kind.idx()],
        };
        if c.aggregate >= self.profile.aggregate() {
            return Err(ResourceDenied {
                kind,
                profile: self.profile,
                limit: self.profile.aggregate(),
                in_use: c.aggregate,
            });
        }
        if c.per_kind[kind.idx()] >= limit {
            return Err(denied(&c, limit));
        }
        c.aggregate += 1;
        c.per_kind[kind.idx()] += 1;
        Ok(Reservation {
            counts: self.counts.clone(),
            kind,
        })
    }
}

/// RAII permit; dropping releases the slots.
#[derive(Debug)]
pub struct Reservation {
    counts: Arc<Mutex<Counts>>,
    kind: ResourceKind,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut c = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        c.aggregate = c.aggregate.saturating_sub(1);
        c.per_kind[self.kind.idx()] = c.per_kind[self.kind.idx()].saturating_sub(1);
    }
}
