# Changelog

## 0.1.3

- Adds `LOCAL_SERVER_ENDPOINTS: [&str; 3]`, the three localhost base URLs the catalog points at (Ollama, LM Studio, llama.cpp). The catalog now reads them from the constant; the URLs are unchanged, so there is no behaviour change. A host can name the same endpoints the catalog uses instead of repeating the strings.

## 0.1.2

- Depends on rightkit-sandbox 0.7.0. `probe_hardware_with_adapter` takes the 0.7.0 `ProcessLaunchAdapter` and `ProcessLaunchContext`; hosts must depend on the same rightkit-sandbox version.

## 0.1.1

- Taken from the CodeRight app: the fit check passes the detected VRAM as `Gpu::vram_mib: Some(mb)` (was `None`), so exact-MiB fit checks apply. Behaviour change for existing callers: a model within rounding distance of the VRAM limit is now judged on exact MiB.

## 0.1.0 (unpublished)

- New crate. The agent host's local model primitives (hardware report through a sandboxed launch adapter, local model catalog and fit, provider route suggestion, bounded local inference seam) moved here from `rightkit-models` 0.5.9, where they were the `agent_local_models` module behind the `agent_local_models` feature (moved from CodeRight `coderight-local-models`). The 2026-11-11 shape review (docs/audits/2026-11-11-shape-matrix/reports/family-foundation-runtime.md) found it is CodeRight code that pulled `rightkit-sandbox` and `tokio` into a foundation crate.
- Paths: `rightkit_models::agent_local_models::X` is now `rightkit_agent_local_models::X`. `FitVerdict` is still re-exported (it is `rightkit_models::FitVerdict`).
- Features: there is no `agent_local_models` feature; the whole crate is the local model layer.
- No behaviour change.
