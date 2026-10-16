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

    /// Same options forced to CPU, for dispatch-bound stages (mel, decoder/joint).
    pub fn cpu_for_stage(&self) -> Self {
        Self {
            provider: ExecutionProvider::Cpu,
            ..self.clone()
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
            Err(error) if self.cpu_fallback && self.provider != ExecutionProvider::Cpu => {
                let session = self.cpu_for_stage().build(&commit, what)?;
                Ok(BuiltSession {
                    session,
                    provider: ExecutionProvider::Cpu,
                    fallback: Some(format!("{:?} -> Cpu: {error}", self.provider)),
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
        // No session may exist before the environment carries this crate's options (telemetry
        // off): ort would otherwise create the environment with its defaults, telemetry on.
        environment::ensure_environment()
            .map_err(|e| SessionError::Runtime(format!("environment: {e}")))?;
        let mut builder = ort::session::Session::builder().map_err(rt("session builder"))?;
        let shared_pool = environment::shared_pool_active();

        let directml = self.provider == ExecutionProvider::DirectMl;
        if directml {
            builder = builder
                .with_independent_thread_pool()
                .map_err(|e| map_ort_error("independent DirectML pool", e))?;
        }
        // Explicit CPU threads need a private pool; ORT ignores them otherwise.
        if self.provider == ExecutionProvider::Cpu && self.threads.is_some() && shared_pool {
            builder = builder
                .with_independent_thread_pool()
                .map_err(|e| map_ort_error("independent CPU stage", e))?;
        }
        if let Some(threads) = self.threads {
            builder = builder
                .with_intra_threads(threads)
                .map_err(|e| map_ort_error("intra threads", e))?;
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
        builder = self.register_provider(builder)?;
        commit(&mut builder).map_err(|e| SessionError::Runtime(format!("load {what}: {e}")))
    }

    /// Register the requested accelerator. ort rc.13 compiles each EP only
    /// behind its Cargo feature, which rightkit-onnx enables per OS (CoreML on
    /// Apple, DirectML on Windows); `supported_on_this_os` already rejected
    /// the other combinations, so the non-hosting arms are unreachable.
    fn register_provider(
        &self,
        builder: ort::session::builder::SessionBuilder,
    ) -> Result<ort::session::builder::SessionBuilder, SessionError> {
        match self.provider {
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
