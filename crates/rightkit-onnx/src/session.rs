//! Session construction and execution-provider selection (merged from
//! HeardRight `heardright-onnx-asr/session.rs`; `HR_ONNX_*_EXPERIMENT`
//! environment switches dropped as product policy).

use std::{
    fmt,
    path::{Path, PathBuf},
};

use crate::environment;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionProvider {
    #[default]
    Cpu,
    DirectMl,
    CoreMl,
}

impl ExecutionProvider {
    /// The accelerator this OS can host: CoreML on Apple targets, DirectML on
    /// Windows, CPU elsewhere. Runtime selection for callers that want "the
    /// platform EP" without naming it.
    pub fn platform_accelerator() -> Self {
        if cfg!(target_vendor = "apple") {
            Self::CoreMl
        } else if cfg!(target_os = "windows") {
            Self::DirectMl
        } else {
            Self::Cpu
        }
    }

    pub fn supported_on_this_os(self) -> bool {
        match self {
            Self::Cpu => true,
            Self::DirectMl => cfg!(target_os = "windows"),
            Self::CoreMl => cfg!(target_vendor = "apple"),
        }
    }
}

#[derive(Debug)]
pub enum SessionError {
    MissingArtifact(PathBuf),
    /// Requested provider cannot exist on this OS. Never silently downgraded.
    ProviderUnsupported(ExecutionProvider),
    Runtime(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingArtifact(p) => write!(f, "model file missing: {}", p.display()),
            Self::ProviderUnsupported(p) => {
                write!(f, "execution provider {p:?} unsupported on this OS")
            }
            Self::Runtime(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SessionError {}

#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    pub provider: ExecutionProvider,
    /// DirectML adapter index; `None` lets the runtime pick.
    pub device_id: Option<i32>,
    /// Explicit intra-op threads. Opts the session out of the shared pool.
    pub threads: Option<usize>,
    /// Set for graphs with a fixed time dimension run on DirectML: applies the
    /// HeardRight-measured fusion workaround (ORT-DirectML 1.24.4 silently
    /// returns near-zero output for static-shape conformer encoders). Remove
    /// once a fixed onnxruntime-directml release passes the corpus.
    pub directml_static_shape_workaround: bool,
    /// When the requested accelerator is unsupported on this OS, fails to
    /// register, or fails to load the model, build a CPU session instead.
    /// Off by default: failures are errors. The fallback is never silent;
    /// [`BuiltSession::fallback`] records why it happened.
    pub cpu_fallback: bool,
    /// DirectML only: disable parallel execution (`with_parallel_execution(false)`).
    /// `true` applies it; `false` keeps the ORT default. Ignored for other
    /// providers and for the CPU stages built by [`Self::cpu_for_stage`].
    /// rightkit-ocr sets this for DirectML.
    pub directml_sequential: bool,
    /// DirectML only: disable the memory-pattern optimisation
    /// (`with_memory_pattern(false)`). `true` applies it; `false` keeps the ORT
    /// default. Ignored for other providers. rightkit-ocr sets this for DirectML.
    pub directml_memory_pattern: bool,
    /// Android only: serve the CPU provider through the XNNPACK execution provider (ARM NEON
    /// kernels for Conv/Gemm/MatMul), with the ORT CPU provider taking every node XNNPACK does
    /// not claim. Requires `provider == Cpu`; on any other OS, or with another provider, the
    /// build fails (never silently ignored). Off by default. Whether the shipped
    /// `libonnxruntime.so` contains the XNNPACK provider is not known until it is loaded; check
    /// `probe_providers` first. NNAPI is a later step and is not offered here.
    ///
    /// XNNPACK runs its own thread pool, so the session's intra-op pool is reduced to one
    /// thread with spinning off (see `xnnpack_threads`).
    pub xnnpack: bool,
    /// Threads for XNNPACK's own pool. `None` uses `threads`, else the `GlobalPool::from_budget`
    /// thread budget. Ignored unless `xnnpack` is set.
    pub xnnpack_threads: Option<usize>,
    /// Set `mlas.disable_kleidiai = 1` on the session (arm64 targets), turning the KleidiAI
    /// kernels off on any runtime. Off by default: ONNX Runtime 1.27.0 and newer keep KleidiAI
    /// (its fast Conv path), and an older runtime on an SME2 Mac is refused at bind time instead
    /// (see [`crate::kleidiai`], bogusyogi/rightkit#889). Set it through [`Self::with_kleidiai`].
    /// A runtime that was bound on an SME2 host through
    /// `RuntimeBindingOptions::allow_known_bad_runtime` forces the entry on every session
    /// whatever this says.
    pub disable_kleidiai: bool,
}

/// Session-builder calls that DirectML needs, derived from [`SessionOptions`].
/// `None` leaves the ORT default in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct DirectMlSessionFlags {
    parallel_execution: Option<bool>,
    memory_pattern: Option<bool>,
}

/// A committed session plus the provider that actually serves it.
#[derive(Debug)]
pub struct BuiltSession {
    pub session: ort::session::Session,
    pub provider: ExecutionProvider,
    /// `Some(reason)` when [`SessionOptions::cpu_fallback`] replaced the
    /// requested accelerator with CPU.
    pub fallback: Option<String>,
}

impl SessionOptions {
    pub fn new(provider: ExecutionProvider) -> Self {
        Self {
            provider,
            ..Self::default()
        }
    }

    pub fn with_device_id(mut self, device_id: Option<i32>) -> Self {
        self.device_id = device_id;
        self
    }

    pub fn with_threads(mut self, threads: Option<usize>) -> Self {
        self.threads = threads;
        self
    }

    pub fn with_cpu_fallback(mut self, cpu_fallback: bool) -> Self {
        self.cpu_fallback = cpu_fallback;
        self
    }

    pub fn with_directml_sequential(mut self, directml_sequential: bool) -> Self {
        self.directml_sequential = directml_sequential;
        self
    }

    pub fn with_directml_memory_pattern(mut self, directml_memory_pattern: bool) -> Self {
        self.directml_memory_pattern = directml_memory_pattern;
        self
    }

    /// Request the XNNPACK execution provider on the CPU provider (Android only; see
    /// [`Self::xnnpack`]).
    pub fn with_xnnpack(mut self, xnnpack: bool) -> Self {
        self.xnnpack = xnnpack;
        self
    }

    /// Threads for XNNPACK's own pool (see [`Self::xnnpack_threads`]).
    pub fn with_xnnpack_threads(mut self, threads: Option<usize>) -> Self {
        self.xnnpack_threads = threads;
        self
    }

    /// `with_kleidiai(false)` sets `mlas.disable_kleidiai = 1` on arm64 targets, whatever the
    /// runtime version; `with_kleidiai(true)` (the default) leaves the runtime's own behaviour.
    /// It cannot re-enable KleidiAI for a known-bad runtime bound through
    /// `RuntimeBindingOptions::allow_known_bad_runtime` on an SME2 Mac.
    pub fn with_kleidiai(mut self, enabled: bool) -> Self {
        self.disable_kleidiai = !enabled;
        self
    }

    /// Whether this session sets [`crate::kleidiai::CONFIG_KEY_DISABLE`]. Pure: the arm64 and
    /// forced flags are passed in so the rule is testable on any host.
    fn kleidiai_disabled(&self, aarch64: bool, forced: bool) -> bool {
        crate::kleidiai::session_disables(self.disable_kleidiai, aarch64, forced)
    }

    /// Refuse an XNNPACK request this build or provider cannot serve. Pure: `android` is passed
    /// in so the rule is testable on any host.
    fn check_xnnpack(&self, android: bool) -> Result<(), String> {
        if !self.xnnpack {
            return Ok(());
        }
        if !android {
            return Err("the XNNPACK option is only available on Android".into());
        }
        if self.provider != ExecutionProvider::Cpu {
            return Err(format!(
                "XNNPACK extends the CPU provider; provider is {:?}",
                self.provider
            ));
        }
        Ok(())
    }

    /// Intra-op thread count the session builder gets: one when XNNPACK owns the compute pool,
    /// else the explicit request.
    fn session_intra_threads(&self) -> Option<usize> {
        if self.xnnpack {
            Some(1)
        } else {
            self.threads
        }
    }

    /// Threads for XNNPACK's own pool; never zero.
    fn xnnpack_pool_threads(&self) -> usize {
        self.xnnpack_threads
            .or(self.threads)
            .unwrap_or_else(|| environment::GlobalPool::from_budget().intra_threads)
            .max(1)
    }

    /// DirectML flags for this request: empty unless the DirectML provider is
    /// selected, so CPU stages and other accelerators keep ORT defaults.
    fn directml_session_flags(&self) -> DirectMlSessionFlags {
        if self.provider != ExecutionProvider::DirectMl {
            return DirectMlSessionFlags::default();
        }
        DirectMlSessionFlags {
            parallel_execution: self.directml_sequential.then_some(false),
            memory_pattern: self.directml_memory_pattern.then_some(false),
        }
    }

    /// Same options forced to plain CPU (no XNNPACK), for dispatch-bound stages (mel,
    /// decoder/joint) and for the accelerator fallback.
    pub fn cpu_for_stage(&self) -> Self {
        Self {
            provider: ExecutionProvider::Cpu,
            xnnpack: false,
            xnnpack_threads: None,
            ..self.clone()
        }
    }

    /// Label for the requested provider in fallback reports.
    fn requested_label(&self) -> String {
        if self.xnnpack {
            format!("{:?}+Xnnpack", self.provider)
        } else {
            format!("{:?}", self.provider)
        }
    }

    pub fn build_from_file(&self, path: &Path) -> Result<ort::session::Session, SessionError> {
        self.build_from_file_reported(path).map(|b| b.session)
    }

    pub fn build_from_memory(&self, bytes: &[u8]) -> Result<ort::session::Session, SessionError> {
        self.build_from_memory_reported(bytes).map(|b| b.session)
    }

    /// Like [`Self::build_from_file`], also reporting the effective provider.
    pub fn build_from_file_reported(&self, path: &Path) -> Result<BuiltSession, SessionError> {
        if !path.is_file() {
            return Err(SessionError::MissingArtifact(path.to_path_buf()));
        }
        self.build_with_fallback(|b| b.commit_from_file(path), &path.display().to_string())
    }

    /// Like [`Self::build_from_memory`], also reporting the effective provider.
    pub fn build_from_memory_reported(&self, bytes: &[u8]) -> Result<BuiltSession, SessionError> {
        self.build_with_fallback(|b| b.commit_from_memory(bytes), "<memory>")
    }

    fn build_with_fallback(
        &self,
        commit: impl Fn(
            &mut ort::session::builder::SessionBuilder,
        ) -> ort::Result<ort::session::Session>,
        what: &str,
    ) -> Result<BuiltSession, SessionError> {
        match self.build(&commit, what) {
            Ok(session) => Ok(BuiltSession {
                session,
                provider: self.provider,
                fallback: None,
            }),
            Err(SessionError::MissingArtifact(p)) => Err(SessionError::MissingArtifact(p)),
            Err(error)
                if self.cpu_fallback
                    && (self.provider != ExecutionProvider::Cpu || self.xnnpack) =>
            {
                let session = self.cpu_for_stage().build(&commit, what)?;
                Ok(BuiltSession {
                    session,
                    provider: ExecutionProvider::Cpu,
                    fallback: Some(format!("{} -> Cpu: {error}", self.requested_label())),
                })
            }
            Err(error) => Err(error),
        }
    }

    fn build(
        &self,
        commit: &impl Fn(
            &mut ort::session::builder::SessionBuilder,
        ) -> ort::Result<ort::session::Session>,
        what: &str,
    ) -> Result<ort::session::Session, SessionError> {
        if !self.provider.supported_on_this_os() {
            return Err(SessionError::ProviderUnsupported(self.provider));
        }
        self.check_xnnpack(cfg!(target_os = "android"))
            .map_err(SessionError::Runtime)?;
        // No session may exist before the environment carries this crate's options (telemetry
        // off): ort rc.15 builds sessions from an environment handle, and this crate's is built
        // from the committed options (defaults when nothing was initialised).
        let env = environment::ort_environment()
            .map_err(|e| SessionError::Runtime(format!("environment: {e}")))?;
        let mut builder = ort::session::Session::builder(env).map_err(rt("session builder"))?;
        let shared_pool = environment::shared_pool_active();

        let directml = self.provider == ExecutionProvider::DirectMl;
        if directml {
            builder = builder
                .with_independent_thread_pool()
                .map_err(|e| map_ort_error("independent DirectML pool", e))?;
        }
        // Explicit CPU threads need a private pool; ORT ignores them otherwise.
        if self.provider == ExecutionProvider::Cpu
            && self.session_intra_threads().is_some()
            && shared_pool
        {
            builder = builder
                .with_independent_thread_pool()
                .map_err(|e| map_ort_error("independent CPU stage", e))?;
        }
        if let Some(threads) = self.session_intra_threads() {
            builder = builder
                .with_intra_threads(threads)
                .map_err(|e| map_ort_error("intra threads", e))?;
        }
        if self.xnnpack {
            // XNNPACK's own pool does the work; an idle spinning intra-op pool only contends.
            builder = builder
                .with_intra_op_spinning(false)
                .map_err(|e| map_ort_error("intra-op spinning", e))?;
        }
        let flags = self.directml_session_flags();
        if let Some(parallel) = flags.parallel_execution {
            builder = builder
                .with_parallel_execution(parallel)
                .map_err(|e| map_ort_error("parallel execution", e))?;
        }
        if let Some(memory_pattern) = flags.memory_pattern {
            builder = builder
                .with_memory_pattern(memory_pattern)
                .map_err(|e| map_ort_error("memory pattern", e))?;
        }
        if self.directml_static_shape_workaround && directml {
            builder = builder
                .with_config_entry("ep.dml.disable_graph_fusion", "1")
                .map_err(|e| map_ort_error("disable_graph_fusion", e))?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level1)
                .map_err(|e| map_ort_error("optimization level", e))?;
        }
        if self.kleidiai_disabled(cfg!(target_arch = "aarch64"), crate::kleidiai::forced_off()) {
            builder = builder
                .with_config_entry(crate::kleidiai::CONFIG_KEY_DISABLE, "1")
                .map_err(|e| map_ort_error("disable_kleidiai", e))?;
        }
        builder = self.register_provider(builder)?;
        commit(&mut builder).map_err(|e| SessionError::Runtime(format!("load {what}: {e}")))
    }

    /// Register the requested accelerator. ort rc.15 compiles each EP only
    /// behind its Cargo feature, which rightkit-onnx enables per OS (CoreML on
    /// Apple, DirectML on Windows); `supported_on_this_os` already rejected
    /// the other combinations, so the non-hosting arms are unreachable.
    fn register_provider(
        &self,
        builder: ort::session::builder::SessionBuilder,
    ) -> Result<ort::session::builder::SessionBuilder, SessionError> {
        match self.provider {
            ExecutionProvider::Cpu if self.xnnpack => self.register_xnnpack(builder),
            ExecutionProvider::Cpu => Ok(builder),
            #[cfg(target_os = "windows")]
            ExecutionProvider::DirectMl => {
                let mut ep = ort::ep::DirectML::default();
                if let Some(id) = self.device_id {
                    ep = ep.with_device_id(id);
                }
                builder
                    .with_execution_providers([ep.build().error_on_failure()])
                    .map_err(|e| map_ort_error("directml", e))
            }
            #[cfg(target_vendor = "apple")]
            ExecutionProvider::CoreMl => builder
                .with_execution_providers([ort::ep::CoreML::default().build().error_on_failure()])
                .map_err(|e| map_ort_error("coreml", e)),
            #[allow(unreachable_patterns)]
            other => Err(SessionError::ProviderUnsupported(other)),
        }
    }
}

impl SessionOptions {
    /// UNVERIFIED (never compiled or run for Android): registers XNNPACK ahead of the ORT CPU
    /// provider with `error_on_failure`, so a runtime without XNNPACK is an error that
    /// `cpu_fallback` can catch, never a silent plain-CPU session.
    #[cfg(target_os = "android")]
    fn register_xnnpack(
        &self,
        builder: ort::session::builder::SessionBuilder,
    ) -> Result<ort::session::builder::SessionBuilder, SessionError> {
        let threads = std::num::NonZeroUsize::new(self.xnnpack_pool_threads())
            .unwrap_or(std::num::NonZeroUsize::MIN);
        builder
            .with_execution_providers([ort::ep::XNNPACK::default()
                .with_intra_op_num_threads(threads)
                .build()
                .error_on_failure()])
            .map_err(|e| map_ort_error("xnnpack", e))
    }

    #[cfg(not(target_os = "android"))]
    fn register_xnnpack(
        &self,
        _builder: ort::session::builder::SessionBuilder,
    ) -> Result<ort::session::builder::SessionBuilder, SessionError> {
        // `check_xnnpack` already refused this request off Android.
        let _ = self.xnnpack_pool_threads();
        Err(SessionError::Runtime(
            "the XNNPACK option is only available on Android".into(),
        ))
    }
}

fn rt(label: &str) -> impl FnOnce(ort::Error) -> SessionError {
    let label = label.to_owned();
    move |error| SessionError::Runtime(format!("{label}: {error}"))
}

fn map_ort_error<R>(label: &str, error: ort::Error<R>) -> SessionError {
    SessionError::Runtime(format!("{label}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directml(sequential: bool, memory_pattern: bool) -> SessionOptions {
        SessionOptions::new(ExecutionProvider::DirectMl)
            .with_directml_sequential(sequential)
            .with_directml_memory_pattern(memory_pattern)
    }

    #[test]
    fn xnnpack_is_off_by_default_and_refused_off_android_or_off_cpu() {
        let options = SessionOptions::new(ExecutionProvider::Cpu);
        assert!(!options.xnnpack);
        assert_eq!(options.xnnpack_threads, None);
        assert!(options.check_xnnpack(false).is_ok());
        assert!(options.check_xnnpack(true).is_ok());
        let xnn = options.clone().with_xnnpack(true);
        assert!(xnn.check_xnnpack(true).is_ok());
        assert!(xnn.check_xnnpack(false).unwrap_err().contains("Android"));
        let wrong = SessionOptions::new(ExecutionProvider::DirectMl).with_xnnpack(true);
        assert!(wrong.check_xnnpack(true).unwrap_err().contains("CPU"));
        // The plain-CPU stage never inherits XNNPACK.
        assert!(!xnn.cpu_for_stage().xnnpack);
    }

    #[test]
    fn xnnpack_owns_the_pool_and_labels_fallbacks() {
        let plain = SessionOptions::new(ExecutionProvider::Cpu).with_threads(Some(3));
        assert_eq!(plain.session_intra_threads(), Some(3));
        let xnn = plain
            .clone()
            .with_xnnpack(true)
            .with_xnnpack_threads(Some(5));
        assert_eq!(xnn.session_intra_threads(), Some(1));
        assert_eq!(xnn.xnnpack_pool_threads(), 5);
        assert_eq!(xnn.requested_label(), "Cpu+Xnnpack");
        assert_eq!(plain.requested_label(), "Cpu");
        // Falls back to `threads`, never to zero.
        assert_eq!(plain.with_xnnpack(true).xnnpack_pool_threads(), 3);
        let zero = SessionOptions::new(ExecutionProvider::Cpu)
            .with_xnnpack(true)
            .with_xnnpack_threads(Some(0));
        assert_eq!(zero.xnnpack_pool_threads(), 1);
        let unset = SessionOptions::new(ExecutionProvider::Cpu).with_xnnpack(true);
        assert!(unset.xnnpack_pool_threads() >= 1);
    }

    #[test]
    fn kleidiai_stays_on_by_default_and_with_kleidiai_false_turns_it_off() {
        let options = SessionOptions::new(ExecutionProvider::Cpu);
        assert!(!options.disable_kleidiai);
        assert!(!options.kleidiai_disabled(true, false));
        assert!(!options.kleidiai_disabled(false, false));
        let off = options.clone().with_kleidiai(false);
        assert!(off.disable_kleidiai);
        assert!(off.kleidiai_disabled(true, false));
        assert!(!off.kleidiai_disabled(false, false), "x86_64 is untouched");
        let on = off.with_kleidiai(true);
        assert!(!on.disable_kleidiai);
        assert!(!on.kleidiai_disabled(true, false));
    }

    #[test]
    fn a_forced_runtime_cannot_be_re_enabled_by_a_session() {
        let on = SessionOptions::new(ExecutionProvider::CoreMl).with_kleidiai(true);
        assert!(on.kleidiai_disabled(true, true));
        assert!(on.cpu_for_stage().kleidiai_disabled(true, true));
        // The stage fallback keeps an explicit disable.
        let off = SessionOptions::new(ExecutionProvider::CoreMl).with_kleidiai(false);
        assert!(off.cpu_for_stage().disable_kleidiai);
    }

    #[test]
    fn default_options_keep_ort_defaults_for_directml() {
        // Today's behaviour: neither flag set, so DirectML sessions are built
        // exactly as before this option existed.
        let options = SessionOptions::new(ExecutionProvider::DirectMl);
        assert!(!options.directml_sequential);
        assert!(!options.directml_memory_pattern);
        assert_eq!(
            options.directml_session_flags(),
            DirectMlSessionFlags::default()
        );
    }

    #[test]
    fn true_flags_map_to_disabled_parallel_execution_and_memory_pattern() {
        assert_eq!(
            directml(true, false).directml_session_flags(),
            DirectMlSessionFlags {
                parallel_execution: Some(false),
                memory_pattern: None,
            }
        );
        assert_eq!(
            directml(false, true).directml_session_flags(),
            DirectMlSessionFlags {
                parallel_execution: None,
                memory_pattern: Some(false),
            }
        );
        assert_eq!(
            directml(true, true).directml_session_flags(),
            DirectMlSessionFlags {
                parallel_execution: Some(false),
                memory_pattern: Some(false),
            }
        );
    }

    #[test]
    fn flags_do_not_apply_to_other_providers_or_cpu_stages() {
        for provider in [ExecutionProvider::Cpu, ExecutionProvider::CoreMl] {
            let options = SessionOptions::new(provider)
                .with_directml_sequential(true)
                .with_directml_memory_pattern(true);
            assert_eq!(
                options.directml_session_flags(),
                DirectMlSessionFlags::default(),
                "{provider:?}"
            );
        }
        let staged = directml(true, true).cpu_for_stage();
        assert_eq!(staged.provider, ExecutionProvider::Cpu);
        assert_eq!(
            staged.directml_session_flags(),
            DirectMlSessionFlags::default()
        );
        // The flags themselves survive staging; only the provider changes.
        assert!(staged.directml_sequential && staged.directml_memory_pattern);
    }
}
