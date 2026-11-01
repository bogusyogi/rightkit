//! This binary owns its process-global governor. Keep the unconfigured and
//! configured phases in one test so parallel tests cannot supply composition.
#![cfg(feature = "coding_tools-host")]

use rightkit_agent_engine_runtime_resources::ServiceScope;
use rightkit_agent_tools::coding_tools::process_resource_authority::{
    install_process_resource_profile, process_resource_governor, reserve_process,
    spawn_with_process_authority, ProcessAdmissionError, ResourceKind, ResourceProfile,
};
use rightkit_sandbox::execution::{
    CommandLaunchSpec, EffectExecutionError, ManagedChild, ProcessLaunchAdapter,
    ProcessLaunchContext,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct CountingAdapter(AtomicUsize);

#[async_trait::async_trait]
impl ProcessLaunchAdapter for CountingAdapter {
    async fn spawn(
        &self,
        _spec: CommandLaunchSpec,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(EffectExecutionError::ResourceLimitsMissing)
    }
}

fn spec() -> CommandLaunchSpec {
    let workspace = std::env::temp_dir().to_string_lossy().into_owned();
    CommandLaunchSpec {
        program: "must-not-be-started".into(),
        argv: Vec::new(),
        cwd: workspace.clone(),
        context: ProcessLaunchContext {
            workspace_root: workspace,
            sandbox_profile: None,
            sandbox_required: false,
            network_required: false,
        },
        env: Vec::new(),
    }
}

#[tokio::test]
async fn missing_composition_never_launches_and_installed_authority_cannot_be_retuned() {
    assert!(process_resource_governor().is_none());
    let adapter = CountingAdapter(AtomicUsize::new(0));
    assert!(matches!(
        reserve_process(
            ResourceKind::ChildProcess,
            ServiceScope::Session("s".into()),
            true
        ),
        Err(ProcessAdmissionError::Unconfigured)
    ));
    assert!(matches!(
        spawn_with_process_authority(&adapter, spec(), ResourceKind::ChildProcess, true).await,
        Err(EffectExecutionError::ResourceLimitsMissing)
    ));
    assert_eq!(adapter.0.load(Ordering::SeqCst), 0);
    assert!(
        process_resource_governor().is_none(),
        "execution must not invent a default authority"
    );

    let governor = install_process_resource_profile(ResourceProfile::Strict).unwrap();
    let same = install_process_resource_profile(ResourceProfile::Strict).unwrap();
    assert!(Arc::ptr_eq(&governor, &same));
    assert!(matches!(
        install_process_resource_profile(ResourceProfile::Expanded),
        Err(ResourceProfile::Strict)
    ));
    assert!(matches!(
        reserve_process(
            ResourceKind::BrowserProcess,
            ServiceScope::Session("s".into()),
            true
        ),
        Err(ProcessAdmissionError::Rejected(_))
    ));
    assert!(matches!(
        spawn_with_process_authority(&adapter, spec(), ResourceKind::ChildProcess, true).await,
        Err(EffectExecutionError::ResourceLimitsMissing)
    ));
    assert_eq!(adapter.0.load(Ordering::SeqCst), 0);
}
