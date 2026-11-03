//! ORT session construction and execution-provider selection.

use std::path::Path;

use crate::onnx_tdt::{Error, Result};

/// Intra-op threads for the Windows decoder/joint session (capped by the
/// machine's available parallelism).
const WINDOWS_DECODER_THREADS: usize = 6;

/// Which ORT execution provider to build sessions with.
///
/// The variants are gated to the platforms that can actually supply them, so an
/// unavailable provider is a compile error rather than a runtime fallback that
/// silently changes performance characteristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionProvider {
    #[default]
    Cpu,
    #[cfg(target_os = "windows")]
    DirectMl,
    #[cfg(target_os = "macos")]
    CoreMl,
}

#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    pub provider: ExecutionProvider,
    /// DirectML adapter index. Onboarding persists a choice on multi-GPU
    /// machines; `None` lets the runtime pick.
    pub device_id: Option<i32>,
    /// Intra-op threads. Windows CPU stages share pool; encoder keeps automatic
    /// private threads. Explicit sizes opt out.
    pub threads: Option<usize>,
    /// Let the CPU intra-op pool spin between parallel sections. Only the
    /// Windows decoder/joint stage turns this on (see `cpu_only_for_stage`);
    /// every other CPU session, and the shared pool, keep spinning off.
    pub spinning: bool,
}

impl SessionOptions {
    pub fn new(provider: ExecutionProvider) -> Self {
        Self {
            provider,
            device_id: None,
            threads: None,
            spinning: false,
        }
    }

    pub fn with_device_id(mut self, device_id: Option<i32>) -> Self {
        self.device_id = device_id;
        self
    }

    /// The same options with the provider forced to CPU. Used for the mel and
    /// decoder/joint stages, which are dispatch-bound rather than compute-bound.
    pub(crate) fn cpu_only_for_stage(&self, stage: &str) -> Self {
        // Windows decoder/joint: a private pool that spins, 6 intra threads.
        // Paired AB/BA on the Dell (Ultra, beam 2, RTX): spin + 8 threads beat
        // the shared non-spinning pool 6/6 (run 17.9 -> 10.4 ms per window);
        // 6 threads matched 8 (+0.6 ms, not significant) with ~30 % less spin
        // tail. Spinning costs nothing at idle (0 % CPU from 2 s after the last
        // call) and ~2 core-seconds per dictation.
        let decoder_spins = cfg!(target_os = "windows") && stage == "DECODER";
        let threads = self.threads.or_else(|| {
            decoder_spins.then(|| {
                std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(1)
                    .min(WINDOWS_DECODER_THREADS)
            })
        });
        Self {
            provider: ExecutionProvider::Cpu,
            threads,
            spinning: decoder_spins,
            ..self.clone()
        }
    }

    pub(crate) fn build(&self, path: &Path) -> Result<rightkit_onnx::ort::session::Session> {
        self.build_inner(path, false, false)
    }

    /// Build the encoder. `static_shape` says the graph declares a fixed time
    /// dimension, which on DirectML requires the fusion workaround below.
    pub(crate) fn build_encoder(
        &self,
        path: &Path,
        static_shape: bool,
    ) -> Result<rightkit_onnx::ort::session::Session> {
        self.build_inner(path, static_shape, true)
    }

    fn build_inner(
        &self,
        path: &Path,
        static_shape: bool,
        encoder: bool,
    ) -> Result<rightkit_onnx::ort::session::Session> {
        if !path.is_file() {
            return Err(Error::MissingArtifact(path.display().to_string()));
        }
        #[cfg(target_os = "windows")]
        let shared_pool = crate::onnx_tdt::environment::windows_shared_pool_active()?;
        #[cfg(not(target_os = "windows"))]
        let shared_pool = false;
        let result = self.build_with_pool(path, static_shape, encoder, shared_pool);
        #[cfg(target_os = "windows")]
        if let Err(Error::Runtime(error)) = &result {
            if shared_pool && error.contains("EnvCreatedWithGlobalThreadPools() was false") {
                crate::onnx_tdt::environment::use_private_pools(error);
                return self.build_with_pool(path, static_shape, encoder, false);
            }
        }
        result
    }

    fn build_with_pool(
        &self,
        path: &Path,
        static_shape: bool,
        encoder: bool,
        shared_pool: bool,
    ) -> Result<rightkit_onnx::ort::session::Session> {
        let mut builder = rightkit_onnx::ort::session::Session::builder(
            rightkit_onnx::ort_environment()
                .map_err(|e| Error::Runtime(format!("ort environment: {e}")))?,
        )
        .map_err(|e| Error::Runtime(format!("session builder: {e}")))?;

        // DirectML keeps its existing independent pool.
        if self.is_directml() {
            builder = builder
                .with_independent_thread_pool()
                .map_err(|e| Error::Runtime(format!("independent DirectML pool: {e}")))?;
        }

        // Explicit CPU thread settings require a private pool; ORT otherwise
        // ignores them when a global pool exists.
        #[cfg(target_os = "windows")]
        let private_cpu_pool = self.threads.is_some()
            || !shared_pool
            || encoder;
        #[cfg(not(target_os = "windows"))]
        let private_cpu_pool = self.threads.is_some() && shared_pool;
        if matches!(self.provider, ExecutionProvider::Cpu) && private_cpu_pool {
            builder = builder
                .with_independent_thread_pool()
                .map_err(|e| Error::Runtime(format!("independent CPU stage: {e}")))?;
        }

        #[cfg(target_os = "windows")]
        if matches!(self.provider, ExecutionProvider::Cpu) {
            // Spinning only takes effect on a private pool; the shared pool is
            // created with spinning off in `environment.rs`.
            builder = builder
                .with_intra_op_spinning(self.spinning && private_cpu_pool)
                .map_err(|e| Error::Runtime(format!("CPU intra spinning: {e}")))?
                .with_inter_op_spinning(false)
                .map_err(|e| Error::Runtime(format!("CPU inter spinning: {e}")))?;
            if self.spinning {
                tracing::info!(
                    model = %path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                    intra_threads = self.threads,
                    private_pool = private_cpu_pool,
                    spinning = private_cpu_pool,
                    "Windows CPU decoder session"
                );
            }
        }

        // DirectML graph-fusion bug (onnxruntime-directml 1.24.4): the fused
        // kernel for a STATIC-shape conformer encoder silently produces
        // near-zero output on DML — CPU and dynamic-shape are both correct — so
        // the TDT joint emits blank at every frame and the transcript comes back
        // EMPTY, with no error and nothing in the log. Confirmed
        // precision-independent (fp32/fp16/4-bit all collapse) and triggered by
        // shape, via a same-bytes static pin of a proven dynamic model.
        //
        // Disabling only the DML fusion pass restores CPU-parity output and
        // keeps every other optimization. Level1 is belt-and-suspenders: it
        // prevents the CPU-side Level3 fusions (QuickGelu and friends) that feed
        // the DML partition compiler, which left a residual collapse when
        // `disable_graph_fusion` was set alone.
        //
        // Costs roughly 2x encoder time, which is why it is scoped to exactly
        // static + DirectML. REMOVAL CONDITION: an onnxruntime-directml release
        // that fixes static-conformer fusion — drop it once a static DML decode
        // passes the corpus without it.
        if static_shape && self.is_directml() {
            builder = builder
                .with_config_entry("ep.dml.disable_graph_fusion", "1")
                .map_err(|e| Error::Runtime(format!("disable_graph_fusion: {e}")))?
                .with_optimization_level(
                    rightkit_onnx::ort::session::builder::GraphOptimizationLevel::Level1,
                )
                .map_err(|e| Error::Runtime(format!("optimization level: {e}")))?;
        }

        if let Some(threads) = self.threads {
            builder = builder
                .with_intra_threads(threads)
                .map_err(|e| Error::Runtime(format!("intra threads: {e}")))?;
        }
        #[cfg(target_os = "windows")]
        if !shared_pool
            && !encoder
            && self.threads.is_none()
            && matches!(self.provider, ExecutionProvider::Cpu)
        {
            builder = builder
                .with_intra_threads(crate::onnx_tdt::environment::windows_cpu_threads())
                .map_err(|e| Error::Runtime(format!("fallback CPU threads: {e}")))?;
        }

        builder = self.apply_provider(builder)?;

        let session = builder
            .commit_from_file(path)
            .map_err(|e| Error::Runtime(format!("load {}: {e}", path.display())))?;
        self.log_execution_provider(encoder);
        Ok(session)
    }

    /// One structured line per session load. DirectML is registered with
    /// `error_on_failure()`, so reaching this point means DirectML registered.
    /// ORT may still place individual nodes on CPU; that is not observable here.
    /// CoreML is registered without `error_on_failure()`, so its result is unknown.
    fn log_execution_provider(&self, encoder: bool) {
        let requested = match self.provider {
            ExecutionProvider::Cpu => "cpu",
            #[cfg(target_os = "windows")]
            ExecutionProvider::DirectMl => "directml",
            #[cfg(target_os = "macos")]
            ExecutionProvider::CoreMl => "coreml",
        };
        let registered = match self.provider {
            ExecutionProvider::Cpu => "cpu",
            #[cfg(target_os = "windows")]
            ExecutionProvider::DirectMl => "directml",
            #[cfg(target_os = "macos")]
            ExecutionProvider::CoreMl => "unknown",
        };
        tracing::info!(
            event = "asr_execution_provider",
            requested,
            registered,
            encoder,
            "ASR session execution provider"
        );
    }

    fn is_directml(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            matches!(self.provider, ExecutionProvider::DirectMl)
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    #[allow(unused_mut, unused_variables)]
    fn apply_provider(
        &self,
        mut builder: rightkit_onnx::ort::session::builder::SessionBuilder,
    ) -> Result<rightkit_onnx::ort::session::builder::SessionBuilder> {
        match self.provider {
            ExecutionProvider::Cpu => {}
            #[cfg(target_os = "windows")]
            ExecutionProvider::DirectMl => {
                let mut ep = rightkit_onnx::ort::ep::DirectML::default();
                if let Some(device_id) = self.device_id {
                    ep = ep.with_device_id(device_id);
                }
                builder = builder
                    .with_execution_providers([ep.build().error_on_failure()])
                    .map_err(|e| Error::Runtime(format!("directml: {e}")))?;
            }
            #[cfg(target_os = "macos")]
            ExecutionProvider::CoreMl => {
                builder = builder
                    .with_execution_providers([rightkit_onnx::ort::ep::CoreML::default().build()])
                    .map_err(|e| Error::Runtime(format!("coreml: {e}")))?;
            }
        }
        Ok(builder)
    }
}
