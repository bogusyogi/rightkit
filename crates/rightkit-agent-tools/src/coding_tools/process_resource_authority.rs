//! Process-wide resource admission for process-backed tool capabilities.
//!
//! Refactor-1 keeps `ResourceGovernor` as the single resource authority. This
//! module is only the product binding seam: the composition root installs one
//! host profile, and process producers reserve capacity through that exact
//! governor. It does not spawn processes or duplicate sandbox policy.

use rightkit_agent_engine_runtime_resources::{
    AdmissionError, ResourceGovernor, ResourcePermit, ServiceScope,
};
pub use rightkit_agent_engine_runtime_resources::{ResourceKind, ResourceProfile};
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use tokio::io::{AsyncRead, AsyncWrite};

#[path = "process_resource_authority_error.rs"]
mod error;
pub use error::ProcessAdmissionError;

pub const PROCESS_RESOURCE_PROFILE_ENV: &str = "RIGHTKIT_RESOURCE_PROFILE";

static PROCESS_GOVERNOR: OnceLock<Arc<ResourceGovernor>> = OnceLock::new();

/// Resolve the local host profile from an explicit operator/test value. Normal
/// local execution remains Practical; Strict and Expanded require an explicit
/// value. Kept pure so qualification can exercise the selector without racing
/// process-global environment mutation.
pub fn local_resource_profile_from_value(raw: Option<&str>) -> Result<ResourceProfile, String> {
    match raw.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(ResourceProfile::Practical),
        Some(value) => ResourceProfile::parse(value),
    }
}

pub fn local_resource_profile_from_env() -> Result<ResourceProfile, String> {
    let raw = std::env::var(PROCESS_RESOURCE_PROFILE_ENV).ok();
    local_resource_profile_from_value(raw.as_deref())
}

/// Install the one process-wide resource governor selected by host composition.
/// Re-installing the same profile is idempotent; attempting to change profile
/// after first use is rejected so unrelated surfaces cannot silently retune
/// process authority mid-runtime.
pub fn install_process_resource_profile(
    profile: ResourceProfile,
) -> Result<Arc<ResourceGovernor>, ResourceProfile> {
    if let Some(existing) = PROCESS_GOVERNOR.get() {
        return if existing.profile() == profile {
            Ok(existing.clone())
        } else {
            Err(existing.profile())
        };
    }

    #[cfg(feature = "coding_tools-test-support")]
    let governor = if profile == ResourceProfile::Expanded {
        // Parallel engine tests share one process-global composition but test
        // independent session lifecycles. Keep every finite per-kind ceiling;
        // omit only aggregate cross-test contention. Production/default builds
        // do not compile this branch and retain Expanded's 16-unit aggregate.
        ResourceGovernor::new(profile.limits())
    } else {
        ResourceGovernor::for_profile(profile)
    };
    #[cfg(not(feature = "coding_tools-test-support"))]
    let governor = ResourceGovernor::for_profile(profile);
    match PROCESS_GOVERNOR.set(governor.clone()) {
        Ok(()) => Ok(governor),
        Err(_) => {
            let existing = PROCESS_GOVERNOR
                .get()
                .expect("process resource governor must exist after set race");
            if existing.profile() == profile {
                Ok(existing.clone())
            } else {
                Err(existing.profile())
            }
        }
    }
}

/// Current composition-owned governor, when host composition has installed it.
/// Optionality is for observation only; execution must use the required accessor.
pub fn process_resource_governor() -> Option<Arc<ResourceGovernor>> {
    PROCESS_GOVERNOR.get().cloned()
}

/// Fail before execution when a host omitted its resource-policy binding.
pub fn require_process_resource_governor() -> Result<Arc<ResourceGovernor>, ProcessAdmissionError> {
    process_resource_governor().ok_or(ProcessAdmissionError::Unconfigured)
}

/// Reservations held for the full lifetime of one external execution group.
/// Dropping this releases both the aggregate host slot and the specific
/// capability slot.
pub struct ProcessAdmission {
    _external: Arc<ResourcePermit>,
    _specific: ResourcePermit,
}

/// KRN-009: the Practical profile admits one active external execution
/// *group*. Processes launched for the same scope (a session's foreground
/// shell beside its background dev server) join that scope's live group
/// instead of competing for a second aggregate slot; per-kind ceilings still
/// bound how many processes the group may hold.
fn join_external_group(
    governor: &Arc<ResourceGovernor>,
    scope: &ServiceScope,
    foreground: bool,
) -> Result<Arc<ResourcePermit>, AdmissionError> {
    type Groups = std::collections::HashMap<(usize, ServiceScope), std::sync::Weak<ResourcePermit>>;
    static GROUPS: OnceLock<std::sync::Mutex<Groups>> = OnceLock::new();
    let key = (Arc::as_ptr(governor) as usize, scope.clone());
    let mut groups = GROUPS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(live) = groups.get(&key).and_then(std::sync::Weak::upgrade) {
        return Ok(live);
    }
    groups.retain(|_, group| group.strong_count() > 0);
    let permit = Arc::new(governor.acquire(
        ResourceKind::ExternalExecution,
        scope.clone(),
        1,
        foreground,
    )?);
    groups.insert(key, Arc::downgrade(&permit));
    Ok(permit)
}

/// Reservation for one specific resource kind. Unlike [`ProcessAdmission`],
/// this does not also reserve the aggregate `ExternalExecution` slot.
pub struct SpecificProcessAdmission {
    _specific: ResourcePermit,
}

fn reserve_specific_with_governor(
    governor: &Arc<ResourceGovernor>,
    kind: ResourceKind,
    scope: ServiceScope,
    foreground: bool,
) -> Result<SpecificProcessAdmission, AdmissionError> {
    if kind == ResourceKind::ExternalExecution {
        return Err(AdmissionError {
            kind,
            scope,
            requested: 1,
            limit: 0,
            pressure: governor.pressure(),
        });
    }

    governor
        .acquire(kind, scope, 1, foreground)
        .map(|specific| SpecificProcessAdmission {
            _specific: specific,
        })
}

/// Reserve one process-backed capability against an explicit governor. Kept
/// separate from the process-global accessor so tests can prove policy without
/// mutating global process state.
pub fn reserve_with_governor(
    governor: &Arc<ResourceGovernor>,
    kind: ResourceKind,
    scope: ServiceScope,
    foreground: bool,
) -> Result<ProcessAdmission, AdmissionError> {
    debug_assert!(kind.is_process_backed());
    debug_assert_ne!(kind, ResourceKind::ExternalExecution);

    let external = join_external_group(governor, &scope, foreground)?;
    let specific = governor.acquire(kind, scope, 1, foreground)?;
    Ok(ProcessAdmission {
        _external: external,
        _specific: specific,
    })
}

/// Reserve against the composition-owned governor. Missing composition is a
/// typed error, not permission to launch an ungoverned process. Every successful
/// result owns real permits; there is no migration-only `Ok(None)` escape.
pub fn reserve_process(
    kind: ResourceKind,
    scope: ServiceScope,
    foreground: bool,
) -> Result<ProcessAdmission, ProcessAdmissionError> {
    let governor = require_process_resource_governor()?;
    #[cfg(test)]
    if test_denied_scopes()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .contains(&scope)
    {
        return Err(ProcessAdmissionError::Rejected(AdmissionError {
            kind,
            scope,
            requested: 1,
            limit: 0,
            pressure: governor.pressure(),
        }));
    }
    reserve_with_governor(&governor, kind, scope, foreground)
        .map_err(ProcessAdmissionError::Rejected)
}

/// Test-only per-scope denial. Forcing a denial by exhausting the shared
/// process governor's global per-kind counter would deny every concurrently
/// running test; this denies only the exact scope a test owns.
#[cfg(test)]
pub(crate) fn test_denied_scopes(
) -> &'static std::sync::Mutex<std::collections::HashSet<ServiceScope>> {
    static DENIED: OnceLock<std::sync::Mutex<std::collections::HashSet<ServiceScope>>> =
        OnceLock::new();
    DENIED.get_or_init(Default::default)
}

/// Reserve only the requested specific resource against the installed
/// governor. `ExternalExecution` is an aggregate authority and is rejected
/// here so callers cannot accidentally charge it twice.
pub fn reserve_specific_process(
    kind: ResourceKind,
    scope: ServiceScope,
    foreground: bool,
) -> Result<SpecificProcessAdmission, ProcessAdmissionError> {
    let governor = require_process_resource_governor()?;
    reserve_specific_with_governor(&governor, kind, scope, foreground)
        .map_err(ProcessAdmissionError::Rejected)
}

/// Process-launch decorator that acquires ResourceGovernor capacity before
/// delegating to the existing platform/sandbox adapter and holds those permits
/// until the returned child is dropped. It does not alter argv, sandbox policy,
/// process-tree ownership, or child I/O.
pub struct GovernedProcessLaunchAdapter<A> {
    inner: A,
    governor: Arc<ResourceGovernor>,
    kind: ResourceKind,
    foreground: bool,
}

impl<A> GovernedProcessLaunchAdapter<A> {
    pub fn new(
        inner: A,
        governor: Arc<ResourceGovernor>,
        kind: ResourceKind,
        foreground: bool,
    ) -> Self {
        debug_assert!(kind.is_process_backed());
        debug_assert_ne!(kind, ResourceKind::ExternalExecution);
        Self {
            inner,
            governor,
            kind,
            foreground,
        }
    }
}

#[async_trait::async_trait]
impl<A> rightkit_sandbox::execution::ProcessLaunchAdapter for GovernedProcessLaunchAdapter<A>
where
    A: rightkit_sandbox::execution::ProcessLaunchAdapter,
{
    async fn spawn(
        &self,
        spec: rightkit_sandbox::execution::CommandLaunchSpec,
    ) -> Result<
        Box<dyn rightkit_sandbox::execution::ManagedChild>,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        let admission = reserve_with_governor(
            &self.governor,
            self.kind,
            ServiceScope::Workspace(spec.context.workspace_root.clone()),
            self.foreground,
        )
        .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing)?;

        let child = self.inner.spawn(spec).await?;
        Ok(Box::new(GovernedManagedChild {
            child,
            _admission: admission,
        }))
    }

    async fn spawn_channel(
        &self,
        spec: rightkit_sandbox::execution::CommandLaunchSpec,
        channel: rightkit_sandbox::execution::ExecutionChannel,
    ) -> Result<
        Box<dyn rightkit_sandbox::execution::ManagedChild>,
        rightkit_sandbox::execution::EffectExecutionError,
    > {
        let admission = reserve_with_governor(
            &self.governor,
            self.kind,
            ServiceScope::Workspace(spec.context.workspace_root.clone()),
            self.foreground,
        )
        .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing)?;

        let child = self.inner.spawn_channel(spec, channel).await?;
        Ok(Box::new(GovernedManagedChild {
            child,
            _admission: admission,
        }))
    }
}

/// Existing process producers use their current sandbox adapter through this
/// required admission seam. Denial, including missing composition, occurs
/// before the inner adapter is invoked; permits belong to the returned child.
pub async fn spawn_with_process_authority<A>(
    inner: &A,
    spec: rightkit_sandbox::execution::CommandLaunchSpec,
    kind: ResourceKind,
    foreground: bool,
) -> Result<
    Box<dyn rightkit_sandbox::execution::ManagedChild>,
    rightkit_sandbox::execution::EffectExecutionError,
>
where
    A: rightkit_sandbox::execution::ProcessLaunchAdapter + ?Sized,
{
    spawn_with_process_authority_channel(
        inner,
        spec,
        rightkit_sandbox::execution::ExecutionChannel::Pipe,
        kind,
        foreground,
    )
    .await
}

/// Channel-aware form of the one process authority. Resource admission is
/// deliberately acquired before either pipe or PTY creation, so a denied PTY
/// request cannot allocate a ConPTY/openpty handle or spawn a child first.
pub async fn spawn_with_process_authority_channel<A>(
    inner: &A,
    spec: rightkit_sandbox::execution::CommandLaunchSpec,
    channel: rightkit_sandbox::execution::ExecutionChannel,
    kind: ResourceKind,
    foreground: bool,
) -> Result<
    Box<dyn rightkit_sandbox::execution::ManagedChild>,
    rightkit_sandbox::execution::EffectExecutionError,
>
where
    A: rightkit_sandbox::execution::ProcessLaunchAdapter + ?Sized,
{
    let admission = reserve_process(
        kind,
        ServiceScope::Workspace(spec.context.workspace_root.clone()),
        foreground,
    )
    .map_err(|_| rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing)?;

    let child = inner.spawn_channel(spec, channel).await?;
    Ok(Box::new(GovernedManagedChild {
        child,
        _admission: admission,
    }))
}

struct GovernedManagedChild {
    child: Box<dyn rightkit_sandbox::execution::ManagedChild>,
    _admission: ProcessAdmission,
}

#[async_trait::async_trait]
impl rightkit_sandbox::execution::ManagedChild for GovernedManagedChild {
    fn pid(&self) -> Option<u32> {
        self.child.pid()
    }

    fn sandbox_proof(&self) -> Option<rightkit_sandbox::execution::SandboxProcessProof> {
        self.child.sandbox_proof()
    }

    fn execution_channel(&self) -> rightkit_sandbox::execution::ExecutionChannel {
        self.child.execution_channel()
    }

    fn start_kill(&mut self) -> bool {
        self.child.start_kill()
    }

    fn take_stdin(&mut self) -> Option<Pin<Box<dyn AsyncWrite + Send>>> {
        self.child.take_stdin()
    }

    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        self.child.take_stdout()
    }

    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        self.child.take_stderr()
    }

    fn resize_pty(
        &mut self,
        size: rightkit_sandbox::execution::PtySize,
    ) -> Result<(), rightkit_sandbox::execution::EffectExecutionError> {
        self.child.resize_pty(size)
    }

    async fn wait(&mut self) -> Result<i32, rightkit_sandbox::execution::EffectExecutionError> {
        self.child.wait().await
    }

    async fn cancel(&mut self) -> Result<(), rightkit_sandbox::execution::EffectExecutionError> {
        self.child.cancel().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_sandbox::execution::{
        CommandLaunchSpec, ExecutionChannel, ManagedChild, ProcessLaunchAdapter,
        ProcessLaunchContext, PtySize,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn local_profile_defaults_practical_and_accepts_explicit_strict() {
        assert_eq!(
            local_resource_profile_from_value(None).unwrap(),
            ResourceProfile::Practical
        );
        assert_eq!(
            local_resource_profile_from_value(Some("strict")).unwrap(),
            ResourceProfile::Strict
        );
        assert_eq!(
            local_resource_profile_from_value(Some("expanded")).unwrap(),
            ResourceProfile::Expanded
        );
        assert!(local_resource_profile_from_value(Some("unbounded")).is_err());
    }

    #[test]
    fn strict_rejects_external_process_before_spawn() {
        let governor = ResourceGovernor::for_profile(ResourceProfile::Strict);
        let result = reserve_with_governor(
            &governor,
            ResourceKind::ChildProcess,
            ServiceScope::Session("strict".into()),
            true,
        );
        assert!(result.is_err());
    }

    #[test]
    fn practical_allows_only_one_external_execution_group() {
        let governor = ResourceGovernor::for_profile(ResourceProfile::Practical);
        let first = reserve_with_governor(
            &governor,
            ResourceKind::ChildProcess,
            ServiceScope::Session("one".into()),
            true,
        )
        .expect("first practical process is admitted");
        let second = reserve_with_governor(
            &governor,
            ResourceKind::Lsp,
            ServiceScope::Session("two".into()),
            true,
        );
        assert!(
            second.is_err(),
            "aggregate external-execution slot must bind across kinds"
        );
        drop(first);
        assert!(reserve_with_governor(
            &governor,
            ResourceKind::Lsp,
            ServiceScope::Session("two".into()),
            true,
        )
        .is_ok());
    }

    #[test]
    fn same_scope_processes_share_one_external_execution_group() {
        let governor = ResourceGovernor::for_profile(ResourceProfile::Practical);
        let scope = ServiceScope::Session("group".into());
        let background =
            reserve_with_governor(&governor, ResourceKind::ChildProcess, scope.clone(), false)
                .expect("background process opens the group");
        let foreground =
            reserve_with_governor(&governor, ResourceKind::ChildProcess, scope.clone(), true)
                .expect("foreground process joins the live group");
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 1);
        assert!(reserve_with_governor(
            &governor,
            ResourceKind::ChildProcess,
            ServiceScope::Session("other".into()),
            true,
        )
        .is_err());
        drop(background);
        drop(foreground);
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 0);
    }

    #[test]
    fn specific_reservation_does_not_charge_external_execution() {
        let governor = ResourceGovernor::for_profile(ResourceProfile::Practical);
        let scope = ServiceScope::Session("specific".into());
        let admission =
            reserve_specific_with_governor(&governor, ResourceKind::Lsp, scope.clone(), true)
                .expect("specific resource is admitted");
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 0);
        assert_eq!(governor.usage(ResourceKind::Lsp, &scope), 1);
        drop(admission);
    }

    #[test]
    fn specific_ceiling_denial_happens_before_reservation() {
        let mut limits = rightkit_agent_engine_runtime_resources::ResourceLimits::default();
        limits.lsp = 0;
        let governor = ResourceGovernor::new(limits);
        let scope = ServiceScope::Session("ceiling".into());
        let result =
            reserve_specific_with_governor(&governor, ResourceKind::Lsp, scope.clone(), true);
        assert!(result.is_err());
        assert_eq!(governor.usage(ResourceKind::Lsp, &scope), 0);
        assert_eq!(governor.usage(ResourceKind::ExternalExecution, &scope), 0);
    }

    #[test]
    fn specific_external_request_is_typed_rejection() {
        let governor = ResourceGovernor::for_profile(ResourceProfile::Practical);
        let scope = ServiceScope::Session("invalid".into());
        let result =
            reserve_specific_with_governor(&governor, ResourceKind::ExternalExecution, scope, true);
        assert!(matches!(
            result,
            Err(AdmissionError {
                kind: ResourceKind::ExternalExecution,
                limit: 0,
                ..
            })
        ));
    }

    #[test]
    fn missing_installed_governor_is_typed_error() {
        // The governor is one process-global OnceLock; sibling tests in this
        // binary legitimately install profiles, so the unconfigured state is
        // only provable in a fresh process. Re-invoke this test binary with a
        // probe flag so the assertion runs where nothing has installed.
        const PROBE_ENV: &str = "RIGHTKIT_GOVERNOR_UNCONFIGURED_PROBE";
        if std::env::var_os(PROBE_ENV).is_some() {
            assert!(matches!(
                require_process_resource_governor(),
                Err(ProcessAdmissionError::Unconfigured)
            ));
            return;
        }
        let status = rightkit_child_process::command_std(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_resource_authority::tests::missing_installed_governor_is_typed_error",
            ])
            .env(PROBE_ENV, "1")
            .status()
            .expect("spawn governor probe process");
        assert!(status.success(), "unconfigured governor probe failed");
    }

    #[test]
    fn specific_drop_releases_capacity_for_next_acquisition() {
        let mut limits = rightkit_agent_engine_runtime_resources::ResourceLimits::default();
        limits.lsp = 1;
        let governor = ResourceGovernor::new(limits);
        let scope = ServiceScope::Session("release".into());
        let first =
            reserve_specific_with_governor(&governor, ResourceKind::Lsp, scope.clone(), true)
                .expect("first specific resource is admitted");
        assert!(
            reserve_specific_with_governor(&governor, ResourceKind::Lsp, scope.clone(), true,)
                .is_err()
        );
        drop(first);
        assert!(reserve_specific_with_governor(&governor, ResourceKind::Lsp, scope, true).is_ok());
    }

    #[derive(Clone)]
    struct CountingAdapter(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl ProcessLaunchAdapter for CountingAdapter {
        async fn spawn(
            &self,
            _spec: CommandLaunchSpec,
        ) -> Result<Box<dyn ManagedChild>, rightkit_sandbox::execution::EffectExecutionError>
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeChild))
        }

        async fn spawn_channel(
            &self,
            _spec: CommandLaunchSpec,
            _channel: ExecutionChannel,
        ) -> Result<Box<dyn ManagedChild>, rightkit_sandbox::execution::EffectExecutionError>
        {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeChild))
        }
    }

    struct FakeChild;

    #[async_trait::async_trait]
    impl ManagedChild for FakeChild {
        fn pid(&self) -> Option<u32> {
            None
        }
        fn take_stdin(&mut self) -> Option<Pin<Box<dyn AsyncWrite + Send>>> {
            None
        }
        fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
            None
        }
        fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
            None
        }
        async fn wait(&mut self) -> Result<i32, rightkit_sandbox::execution::EffectExecutionError> {
            Ok(0)
        }
        async fn cancel(
            &mut self,
        ) -> Result<(), rightkit_sandbox::execution::EffectExecutionError> {
            Ok(())
        }
    }

    fn fake_spec() -> CommandLaunchSpec {
        CommandLaunchSpec {
            program: "fake".into(),
            argv: Vec::new(),
            cwd: "/work".into(),
            context: ProcessLaunchContext {
                workspace_root: "/work".into(),
                sandbox_profile: None,
                sandbox_required: false,
                network_required: false,
            },
            env: Vec::new(),
        }
    }

    #[tokio::test]
    async fn governed_adapter_denies_before_inner_spawn() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = GovernedProcessLaunchAdapter::new(
            CountingAdapter(calls.clone()),
            ResourceGovernor::for_profile(ResourceProfile::Strict),
            ResourceKind::ChildProcess,
            true,
        );
        let result = adapter.spawn(fake_spec()).await;
        assert!(matches!(
            result,
            Err(rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing)
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "denial must happen before spawn"
        );
    }

    #[tokio::test]
    async fn governed_adapter_denies_pty_before_inner_spawn() {
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = GovernedProcessLaunchAdapter::new(
            CountingAdapter(calls.clone()),
            ResourceGovernor::for_profile(ResourceProfile::Strict),
            ResourceKind::ChildProcess,
            true,
        );
        let result = adapter
            .spawn_channel(fake_spec(), ExecutionChannel::Pty(PtySize::default()))
            .await;
        assert!(matches!(
            result,
            Err(rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing)
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "resource denial must happen before openpty/ConPTY or child spawn"
        );
    }

    #[tokio::test]
    async fn governed_child_holds_practical_slot_until_drop() {
        let governor = ResourceGovernor::for_profile(ResourceProfile::Practical);
        let calls = Arc::new(AtomicUsize::new(0));
        let adapter = GovernedProcessLaunchAdapter::new(
            CountingAdapter(calls),
            governor.clone(),
            ResourceKind::ChildProcess,
            true,
        );

        let other_workspace = || {
            let mut spec = fake_spec();
            spec.context.workspace_root = "/other".into();
            spec
        };
        let first = adapter
            .spawn(fake_spec())
            .await
            .expect("first spawn admitted");
        // KRN-009: a second process in the same workspace joins the live
        // group; another workspace waits for the one active group to end.
        let joined = adapter
            .spawn(fake_spec())
            .await
            .expect("same-workspace spawn joins the active group");
        assert!(matches!(
            adapter.spawn(other_workspace()).await,
            Err(rightkit_sandbox::execution::EffectExecutionError::ResourceLimitsMissing)
        ));
        drop(first);
        drop(joined);
        assert!(adapter.spawn(other_workspace()).await.is_ok());
    }
}
