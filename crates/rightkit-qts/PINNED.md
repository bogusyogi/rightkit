# Vendored QTS (pinned)

Source: https://github.com/yet-another-ai/qts commit `0f66e61429aede9ff2b5d03fdd3de6d8d25535f7` (v0.2.0), Apache-2.0.
ggml (in rightkit-qts-ggml-sys): https://github.com/ggml-org/ggml commit `2fb6431f67dd505584a9fefe94fd2866b944c85f` (v0.9.8), MIT (`vendor/ggml/LICENSE`).
Upstream `qts` on crates.io is 0.1.0 only, so v0.2.0 is republished as `rightkit-qts`, `rightkit-qts-ggml`, `rightkit-qts-ggml-sys` (crate versions 0.1.0 track upstream 0.2.0).

Changes from upstream: workspace inheritance replaced by literals; crates renamed (library names `qts`, `qts_ggml`, `ggml_sys` unchanged);
default features empty; `ort` pinned `=2.0.0-rc.12` (`load-dynamic`, `api-24`); benches/dev-deps/examples/tests dropped;
unbuilt ggml backends removed (cuda, sycl, opencl, hip, musa, cann, webgpu, openvino, hexagon, zdnn, zendnn, virtgpu, rpc) with their cargo features and build.rs links;
`links` renamed `rightkit_ggml`; build.rs GenRight host-tool discovery retained. Kept: cpu, metal, vulkan, blas.
