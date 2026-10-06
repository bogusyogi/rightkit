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

    /// Same options forced to CPU, for dispatch-bound stages (mel, decoder/joint).
    pub fn cpu_for_stage(&self) -> Self {
        Self {
            provider: ExecutionProvider::Cpu,
            ..self.clone()
        }
    }

    pub fn build_from_file(&self, path: &Path) -> Result<ort::session::Session, SessionError> {
        if !path.is_file() {
            return Err(SessionError::MissingArtifact(path.to_path_buf()));
        }
        self.build(|b| b.commit_from_file(path), &path.display().to_string())
    }

    pub fn build_from_memory(&self, bytes: &[u8]) -> Result<ort::session::Session, SessionError> {
        self.build(|b| b.commit_from_memory(bytes), "<memory>")
    }

    fn build(
        &self,
        commit: impl FnOnce(
            &mut ort::session::builder::SessionBuilder,
        ) -> ort::Result<ort::session::Session>,
        what: &str,
    ) -> Result<ort::session::Session, SessionError> {
        if !self.provider.supported_on_this_os() {
            return Err(SessionError::ProviderUnsupported(self.provider));
        }
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
        if self.directml_static_shape_workaround && directml {
            builder = builder
                .with_config_entry("ep.dml.disable_graph_fusion", "1")
                .map_err(|e| map_ort_error("disable_graph_fusion", e))?
                .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level1)
                .map_err(|e| map_ort_error("optimization level", e))?;
        }
        builder = match self.provider {
            ExecutionProvider::Cpu => builder,
            ExecutionProvider::DirectMl => {
                let mut ep = ort::ep::DirectML::default();
                if let Some(id) = self.device_id {
                    ep = ep.with_device_id(id);
                }
                builder
                    .with_execution_providers([ep.build().error_on_failure()])
                    .map_err(|e| map_ort_error("directml", e))?
            }
            ExecutionProvider::CoreMl => builder
                .with_execution_providers([ort::ep::CoreML::default().build().error_on_failure()])
                .map_err(|e| map_ort_error("coreml", e))?,
        };
        commit(&mut builder).map_err(|e| SessionError::Runtime(format!("load {what}: {e}")))
    }
}

fn rt(label: &str) -> impl FnOnce(ort::Error) -> SessionError {
    let label = label.to_owned();
    move |error| SessionError::Runtime(format!("{label}: {error}"))
}

fn map_ort_error<R>(label: &str, error: ort::Error<R>) -> SessionError {
    SessionError::Runtime(format!("{label}: {error}"))
}
