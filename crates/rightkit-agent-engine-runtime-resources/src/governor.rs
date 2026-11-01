use crate::ServiceScope;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceKind {
    Inference,
    /// Aggregate admission for any external execution group. Process-backed
    /// capabilities reserve this in addition to their specific kind so a
    /// Practical host cannot accidentally run one shell + one MCP + one LSP
    /// + one browser at the same time merely because each per-kind limit fits.
    ExternalExecution,
    ChildProcess,
    BackgroundProcess,
    Mcp,
    Lsp,
    BrowserProcess,
    BrowserContext,
    RetainedOutputBytes,
    ToolResultBytes,
    EventQueueBytes,
}

impl ResourceKind {
    pub fn is_process_backed(self) -> bool {
        matches!(
            self,
            Self::ExternalExecution
                | Self::ChildProcess
                | Self::BackgroundProcess
                | Self::Mcp
                | Self::Lsp
                | Self::BrowserProcess
                | Self::BrowserContext
        )
    }
}

/// Canonical the host app host resource profile. This is policy on the existing
/// ResourceGovernor, not a second process limiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResourceProfile {
    /// One the host app process. Process-backed capabilities are unavailable
    /// before launch; native provider HTTP + in-process file/search/edit stay.
    Strict,
    /// Normal local target: one resident the host app process and one active
    /// external execution group, with deliberately small/lazy helper ceilings.
    #[default]
    Practical,
    /// Service/expanded host: larger but still finite helper pools.
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

    pub fn limits(self) -> ResourceLimits {
        match self {
            Self::Strict => ResourceLimits {
                inference: 4,
                daemon_children: 0,
                session_children: 0,
                daemon_background: 0,
                session_background: 0,
                mcp: 0,
                lsp: 0,
                browser_processes: 0,
                browser_contexts: 0,
                retained_output_bytes: 128 * 1024,
                tool_result_bytes: 64 * 1024 * 1024,
                event_queue_bytes: 1024 * 1024,
            },
            Self::Practical => ResourceLimits {
                inference: 4,
                daemon_children: 8,
                session_children: 4,
                daemon_background: 4,
                session_background: 2,
                mcp: 2,
                lsp: 2,
                browser_processes: 1,
                browser_contexts: 4,
                retained_output_bytes: 128 * 1024,
                tool_result_bytes: 64 * 1024 * 1024,
                event_queue_bytes: 1024 * 1024,
            },
            // Preserve the pre-Refactor-1 ResourceLimits::default values as
            // the expanded/service profile. Existing callers of `new(default)`
            // therefore keep their behavior until composition explicitly
            // selects a profile.
            Self::Expanded => ResourceLimits::default(),
        }
    }

    fn external_execution_limit(self) -> usize {
        match self {
            Self::Strict => 0,
            Self::Practical => 1,
            Self::Expanded => 16,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PressureLevel {
    Normal,
    Constrained,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    pub inference: usize,
    pub daemon_children: usize,
    pub session_children: usize,
    pub daemon_background: usize,
    pub session_background: usize,
    pub mcp: usize,
    pub lsp: usize,
    pub browser_processes: usize,
    pub browser_contexts: usize,
    pub retained_output_bytes: usize,
    pub tool_result_bytes: usize,
    pub event_queue_bytes: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            inference: 4,
            daemon_children: 128,
            session_children: 16,
            daemon_background: 64,
            session_background: 8,
            mcp: 16,
            lsp: 16,
            browser_processes: 4,
            browser_contexts: 32,
            retained_output_bytes: 128 * 1024,
            tool_result_bytes: 64 * 1024 * 1024,
            event_queue_bytes: 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionError {
    pub kind: ResourceKind,
    pub scope: ServiceScope,
    pub requested: usize,
    pub limit: usize,
    pub pressure: PressureLevel,
}

#[derive(Default)]
struct Usage {
    global: HashMap<ResourceKind, usize>,
    scoped: HashMap<(ResourceKind, ServiceScope), usize>,
}

pub struct ResourceGovernor {
    profile: ResourceProfile,
    limits: ResourceLimits,
    external_execution_limit: usize,
    pressure: Mutex<PressureLevel>,
    usage: Mutex<Usage>,
}

/// An admission token releases precisely its reservation on drop.
pub struct ResourcePermit {
    governor: Arc<ResourceGovernor>,
    kind: ResourceKind,
    scope: ServiceScope,
    units: usize,
}

impl ResourceGovernor {
    /// Back-compatible constructor for custom limits. Existing callers keep
    /// their historical per-kind behavior and no new aggregate execution cap.
    /// Product composition should use [`Self::for_profile`] for Refactor-1.
    pub fn new(limits: ResourceLimits) -> Arc<Self> {
        Arc::new(Self {
            profile: ResourceProfile::Expanded,
            limits,
            external_execution_limit: usize::MAX,
            pressure: Mutex::new(PressureLevel::Normal),
            usage: Mutex::new(Usage::default()),
        })
    }

    pub fn for_profile(profile: ResourceProfile) -> Arc<Self> {
        Arc::new(Self {
            profile,
            limits: profile.limits(),
            external_execution_limit: profile.external_execution_limit(),
            pressure: Mutex::new(PressureLevel::Normal),
            usage: Mutex::new(Usage::default()),
        })
    }

    pub fn profile(&self) -> ResourceProfile {
        self.profile
    }

    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }

    pub fn set_pressure(&self, pressure: PressureLevel) {
        *self
            .pressure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = pressure;
    }

    pub fn pressure(&self) -> PressureLevel {
        *self
            .pressure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn spill_threshold_bytes(&self) -> usize {
        match self.pressure() {
            PressureLevel::Normal => 8 * 1024 * 1024,
            PressureLevel::Constrained => 128 * 1024,
            PressureLevel::Critical => 32 * 1024,
        }
    }

    /// Cheap preflight used by tool/capability catalogs. It does not reserve
    /// capacity; it answers whether this profile can ever admit one unit of
    /// the requested kind. Strict therefore hides process-backed capabilities
    /// before any launch adapter is reached.
    pub fn capability_available(&self, kind: ResourceKind) -> bool {
        self.global_limit(kind) > 0
    }

    pub fn acquire(
        self: &Arc<Self>,
        kind: ResourceKind,
        scope: ServiceScope,
        units: usize,
        foreground: bool,
    ) -> Result<ResourcePermit, AdmissionError> {
        let pressure = self.pressure();
        if pressure == PressureLevel::Critical
            && !foreground
            && matches!(
                kind,
                ResourceKind::BrowserProcess | ResourceKind::Mcp | ResourceKind::Lsp
            )
        {
            return Err(AdmissionError {
                kind,
                scope,
                requested: units,
                limit: 0,
                pressure,
            });
        }
        let mut usage = self
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let global_limit = self.global_limit(kind);
        let current = usage.global.get(&kind).copied().unwrap_or_default();
        let scoped_limit = self.scoped_limit(kind, &scope);
        let scoped_current = usage
            .scoped
            .get(&(kind, scope.clone()))
            .copied()
            .unwrap_or_default();
        if current.saturating_add(units) > global_limit
            || scoped_limit.is_some_and(|limit| scoped_current.saturating_add(units) > limit)
        {
            return Err(AdmissionError {
                kind,
                scope,
                requested: units,
                limit: scoped_limit.unwrap_or(global_limit).min(global_limit),
                pressure,
            });
        }
        *usage.global.entry(kind).or_default() += units;
        *usage.scoped.entry((kind, scope.clone())).or_default() += units;
        Ok(ResourcePermit {
            governor: Arc::clone(self),
            kind,
            scope,
            units,
        })
    }

    pub fn usage(&self, kind: ResourceKind, scope: &ServiceScope) -> usize {
        self.usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scoped
            .get(&(kind, scope.clone()))
            .copied()
            .unwrap_or_default()
    }

    fn global_limit(&self, kind: ResourceKind) -> usize {
        match kind {
            ResourceKind::Inference => self.limits.inference,
            ResourceKind::ExternalExecution => self.external_execution_limit,
            ResourceKind::ChildProcess => self.limits.daemon_children,
            ResourceKind::BackgroundProcess => self.limits.daemon_background,
            ResourceKind::Mcp => self.limits.mcp,
            ResourceKind::Lsp => self.limits.lsp,
            ResourceKind::BrowserProcess => self.limits.browser_processes,
            ResourceKind::BrowserContext => self.limits.browser_contexts,
            ResourceKind::RetainedOutputBytes => self.limits.retained_output_bytes,
            ResourceKind::ToolResultBytes => self.limits.tool_result_bytes,
            ResourceKind::EventQueueBytes => self.limits.event_queue_bytes,
        }
    }

    fn scoped_limit(&self, kind: ResourceKind, scope: &ServiceScope) -> Option<usize> {
        match (kind, scope) {
            (ResourceKind::ChildProcess, ServiceScope::Session(_)) => {
                Some(self.limits.session_children)
            }
            (ResourceKind::BackgroundProcess, ServiceScope::Session(_)) => {
                Some(self.limits.session_background)
            }
            _ => None,
        }
    }
}

impl Drop for ResourcePermit {
    fn drop(&mut self) {
        let mut usage = self
            .governor
            .usage
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let global = usage.global.entry(self.kind).or_default();
        *global = global.saturating_sub(self.units);
        let scoped = usage
            .scoped
            .entry((self.kind, self.scope.clone()))
            .or_default();
        *scoped = scoped.saturating_sub(self.units);
    }
}
