# rightkit-agent-local-models

Local model primitives for an agent host (CodeRight and the RightKit agent server): what this machine can run, and a bounded seam for running it.

- `probe_hardware_with_adapter`: a `HardwareReport` (memory, GPUs, detected inference runtimes) from `rightkit-models`' hardware and runtime probes, with every child process launched through a caller-owned `rightkit-sandbox` `ProcessLaunchAdapter`.
- `model_catalog`: a `LocalModelCatalog` of `LocalModelRecommendation`s, each with a tiered RAM and VRAM `FitVerdict`.
- `provider_route_suggestion`: a `ProviderRouteSuggestion` for a local OpenAI-compatible endpoint.
- The local inference seam: `LocalInferenceLimits`, `LocalInferenceRequest`, `validate_inference_request`, the `LocalInferenceBackend` trait, `infer_with_backend` and `infer_local_model`. There are no model weights or inference engine here; without a product-supplied backend the seam returns `LocalInferenceError::NoLocalModelAvailable`.

**Use it** in an agent host that offers local models as a provider. **Do not use it** to install or store models: that is `rightkit-models`, which this crate uses for hardware fit and runtime detection.

## Features

None. The whole crate is the agent local model layer.

## Origin

The code moved here unchanged from `rightkit-models` 0.5.x, where it was the `agent_local_models` module behind the `agent_local_models` feature (see CHANGELOG.md).

## License

MIT OR Apache-2.0.
