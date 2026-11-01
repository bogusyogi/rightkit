//! Focused PTY consumer tests: no OS process is launched. Native closure is
//! separately opt-in in managed_pty_closure.rs.
#![cfg(feature = "coding_tools-host")]

use rightkit_agent_engine_runtime_resources::{ResourceGovernor, ResourceKind, ResourceProfile};
use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
use rightkit_agent_tools::coding_tools::{
    background_shell::{BgStatus, BgStore, ProcessTool},
    process_resource_authority::GovernedProcessLaunchAdapter,
    shell::ShellTool,
    Tool, ToolContext,
};
use rightkit_sandbox::execution::{
    CommandLaunchSpec, EffectExecutionError, ExecutionChannel, ManagedChild, ProcessLaunchAdapter,
    ProcessLaunchContext, PtySize,
};
use serde_json::{json, Value};
use std::{
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Default)]
struct Trace {
    launches: Mutex<Vec<(CommandLaunchSpec, ExecutionChannel)>>,
    input: Mutex<Vec<u8>>,
    sizes: Mutex<Vec<PtySize>>,
    eof: AtomicBool,
    cancelled: AtomicBool,
    dropped: AtomicBool,
}
struct RecordingAdapter(Arc<Trace>);
struct RecordingChild {
    trace: Arc<Trace>,
    channel: ExecutionChannel,
}
struct RecordingInput(Arc<Trace>);

impl AsyncWrite for RecordingInput {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.input.lock().unwrap().extend_from_slice(bytes);
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.0.eof.store(true, Ordering::Release);
        Poll::Ready(Ok(()))
    }
}
#[async_trait::async_trait]
impl ProcessLaunchAdapter for RecordingAdapter {
    async fn spawn(
        &self,
        _: CommandLaunchSpec,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
        panic!("PTY consumer must use explicit channel, not implicit pipe spawn")
    }
    async fn spawn_channel(
        &self,
        spec: CommandLaunchSpec,
        channel: ExecutionChannel,
    ) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
        self.0.launches.lock().unwrap().push((spec, channel));
        Ok(Box::new(RecordingChild {
            trace: self.0.clone(),
            channel,
        }))
    }
}
#[async_trait::async_trait]
impl ManagedChild for RecordingChild {
    fn pid(&self) -> Option<u32> {
        Some(42)
    }
    fn execution_channel(&self) -> ExecutionChannel {
        self.channel
    }
    fn take_stdin(&mut self) -> Option<Pin<Box<dyn AsyncWrite + Send>>> {
        Some(Box::pin(RecordingInput(self.trace.clone())))
    }
    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        Some(Box::pin(std::io::Cursor::new(vec![255, 0, 62])))
    }
    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        None
    }
    fn resize_pty(&mut self, size: PtySize) -> Result<(), EffectExecutionError> {
        self.trace.sizes.lock().unwrap().push(size);
        self.channel = ExecutionChannel::Pty(size);
        Ok(())
    }
    async fn wait(&mut self) -> Result<i32, EffectExecutionError> {
        loop {
            if self.trace.cancelled.load(Ordering::Acquire) {
                return Ok(1);
            }
            if self.trace.eof.load(Ordering::Acquire) {
                return Ok(0);
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    }
    async fn cancel(&mut self) -> Result<(), EffectExecutionError> {
        self.trace.cancelled.store(true, Ordering::Release);
        Ok(())
    }
}
impl Drop for RecordingChild {
    fn drop(&mut self) {
        self.trace.dropped.store(true, Ordering::Release);
    }
}

fn context(
    root: &std::path::Path,
    mode: PermissionMode,
    profile: ResourceProfile,
    trace: Arc<Trace>,
) -> ToolContext {
    ToolContext::new("pty-test", root, mode, WorkspaceTrust::Trusted)
        .with_background_shells(BgStore::default())
        .with_process_launch(
            Arc::new(GovernedProcessLaunchAdapter::new(
                RecordingAdapter(trace),
                ResourceGovernor::for_profile(profile),
                ResourceKind::ChildProcess,
                true,
            )),
            ProcessLaunchContext {
                workspace_root: root.display().to_string(),
                sandbox_profile: None,
                sandbox_required: false,
                network_required: false,
            },
        )
}
async fn settled(ctx: &ToolContext, id: &str) -> BgStatus {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let status = ctx.background_shells.as_ref().unwrap().lock().unwrap()[id].status();
            if status != BgStatus::Running {
                return status;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("process must settle")
}

#[tokio::test]
async fn invalid_and_unapproved_pty_never_reach_process_admission() {
    let root = tempfile::tempdir().unwrap();
    let trace = Arc::new(Trace::default());
    let ctx = context(
        root.path(),
        PermissionMode::Ask,
        ResourceProfile::Practical,
        trace.clone(),
    );
    for args in [
        json!({"command":"echo hello", "execution_channel":"wrong"}),
        json!({"command":"echo hello", "execution_channel":"pty", "pty_size":{"rows":0,"cols":80}}),
        json!({"command":"echo hello", "execution_channel":"pty"}),
    ] {
        assert!(ShellTool.execute(args, &ctx).await.is_error);
    }
    assert!(trace.launches.lock().unwrap().is_empty());
}

#[tokio::test]
async fn strict_resource_denial_creates_no_pty_or_inventory_handle() {
    let root = tempfile::tempdir().unwrap();
    let trace = Arc::new(Trace::default());
    let ctx = context(
        root.path(),
        PermissionMode::Bypass,
        ResourceProfile::Strict,
        trace.clone(),
    );
    let result = ShellTool
        .execute(
            json!({"command":"echo hello","execution_channel":"pty"}),
            &ctx,
        )
        .await;
    assert!(result.is_error && result.content.contains("resource_limits_missing"));
    assert!(trace.launches.lock().unwrap().is_empty());
    assert!(ctx
        .background_shells
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn shell_pty_uses_existing_inventory_controls_and_settlement() {
    use base64::Engine as _;
    let root = tempfile::tempdir().unwrap();
    let trace = Arc::new(Trace::default());
    let ctx = context(
        root.path(),
        PermissionMode::Bypass,
        ResourceProfile::Practical,
        trace.clone(),
    );
    let result = ShellTool
        .execute(
            json!({"command":"echo hello","execution_channel":"pty"}),
            &ctx,
        )
        .await;
    assert!(!result.is_error, "{}", result.content);
    let id = result.process_id.unwrap();
    assert_eq!(trace.launches.lock().unwrap().len(), 1);
    assert_eq!(
        trace.launches.lock().unwrap()[0].1,
        ExecutionChannel::Pty(PtySize::default())
    );
    assert_eq!(
        trace.launches.lock().unwrap()[0]
            .0
            .context
            .sandbox_profile
            .as_deref(),
        Some("shell")
    );
    let input = ProcessTool
        .execute(
            json!({"operation":"input","process_id":id,"input_base64":"AP9B","lease_epoch":1,"input_sequence":0}),
            &ctx,
        )
        .await;
    assert!(!input.is_error, "{}", input.content);
    assert_eq!(*trace.input.lock().unwrap(), vec![0, 255, 65]);
    let resize = ProcessTool
        .execute(
            json!({"operation":"resize","process_id":id,"size":{"rows":40,"cols":120}}),
            &ctx,
        )
        .await;
    assert!(!resize.is_error, "{}", resize.content);
    assert_eq!(trace.sizes.lock().unwrap()[0].rows, 40);
    let eof = ProcessTool
        .execute(
            json!({"operation":"eof","process_id":id,"lease_epoch":1,"input_sequence":1}),
            &ctx,
        )
        .await;
    assert!(!eof.is_error);
    assert_eq!(settled(&ctx, &id).await, BgStatus::Exited(0));
    let result = ProcessTool
        .execute(json!({"operation":"output","process_id":id}), &ctx)
        .await;
    let output: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(output["output_base64"].as_str().unwrap())
            .unwrap(),
        vec![255, 0, 62]
    );
    assert!(trace.dropped.load(Ordering::Acquire));
    assert!(
        !trace.cancelled.load(Ordering::Acquire),
        "EOF is not cancellation"
    );
}

#[tokio::test]
async fn process_stop_cancels_the_same_managed_child() {
    let root = tempfile::tempdir().unwrap();
    let trace = Arc::new(Trace::default());
    let ctx = context(
        root.path(),
        PermissionMode::Bypass,
        ResourceProfile::Practical,
        trace.clone(),
    );
    let result = ShellTool
        .execute(
            json!({"command":"echo hello","execution_channel":"pty"}),
            &ctx,
        )
        .await;
    let id = result.process_id.expect("managed handle");
    assert!(
        !ProcessTool
            .execute(json!({"operation":"stop","process_id":id}), &ctx)
            .await
            .is_error
    );
    assert_eq!(settled(&ctx, &id).await, BgStatus::Killed);
    assert!(trace.cancelled.load(Ordering::Acquire));
    assert!(trace.dropped.load(Ordering::Acquire));
}

#[test]
fn process_input_eof_and_resize_remain_policy_facing_controls() {
    for operation in ["input", "eof", "resize"] {
        assert_eq!(
            ProcessTool.capability_class_for(&json!({"operation":operation})),
            Some(rightkit_agent_policy::CapabilityClass::ProcessControl)
        );
    }
    for operation in ["input", "eof"] {
        assert_eq!(
            ProcessTool.durable_write_kind(&json!({"operation":operation})),
            Some(rightkit_agent_policy::DurableWriteKind::ShellCommand)
        );
    }
    assert_eq!(
        ShellTool.input_schema()["properties"]["execution_channel"]["default"],
        "pipe"
    );
}
