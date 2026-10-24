# rightkit-tts-qwen-sys 0.1.2 registry consumer

Standalone consumer, outside the workspace, pinning `rightkit-tts-qwen-sys =0.1.2` exactly with its
default features (empty: the plain CPU build). It runs on Windows and macOS in hosted qualification,
where the crate's checked-in bindings for `x86_64-pc-windows-msvc` and `aarch64-apple-darwin` are
used. The library crate is named `ggml_sys`.

What it proves, in plain sentences, through the raw FFI surface:

- The sealed archive builds its vendored ggml with CMake on the host and links it statically. The
  program reads the ggml version string (`0.9.8`), the commit string, and the `GGML_MAX_DIMS` and
  `GGUF_VERSION` constants.
- Type metrics from the library agree with the bindings: name, block size, bytes per block and
  `ggml_is_quantized` for F32, F16, Q8_0 and Q4_0, and `ggml_row_size` for a plain and a quantised
  row.
- CPU graph compute works: a two-tensor add, and a dot product through `ggml_mul_mat` run on two
  threads (1*4 + 2*5 + 3*6 = 32).
- Quantisation works: one block of 32 values goes through `ggml_quantize_chunk` to Q8_0 (34 bytes)
  and back through `ggml_cast` within a tolerance of 0.05.
- GGUF metadata works in memory and through the file reader: a string and a `u32` key are set, found,
  read, typed, removed and counted; the serialised metadata starts with the `GGUF` magic and version
  3; the same bytes written to a temporary file are read back by `gguf_init_from_file` with both
  keys intact; and bytes that are not GGUF are refused with a null context. ggml prints one
  "invalid magic characters" line to stderr for that last case; that line is expected.

What it deliberately does not prove: any model load, the Metal, Vulkan and BLAS backends and the
`native` feature (all off here), the `regenerate-bindings` feature (it needs libclang and writes into
the crate directory), CPU-feature detection results (host-specific), or the safe API, which belongs
to `rightkit-tts-qwen`.

State: two small files in the OS temporary directory, each removed straight after use. No network,
no process spawn, no GPU, no user data. The first build compiles ggml with CMake and needs a C and
C++ compiler and CMake on the host, as the crate's own build does; the run itself takes well under
a second. Run only by the CI consumer lane.
