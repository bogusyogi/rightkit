// rightkit-tts-qwen-sys 0.1.2 consumed by exact registry pin (default features, which is the
// plain CPU build), outside the workspace.
//
// Proves the sealed archive builds its vendored ggml with CMake on the host and links it, and that
// the checked-in bindings for this target call into it: the ggml version string, tensor type
// metrics, a two-node compute graph on the CPU, a quantise and dequantise round trip through the
// Q8_0 type, a dot product through ggml_mul_mat, and a GGUF metadata container that is written to
// memory, read back from a file and freed. The library crate is named `ggml_sys`. Raw FFI only.
// Does not prove: any model load, the Metal, Vulkan or BLAS backends and the `native` feature (all
// are off here), the `regenerate-bindings` feature, or the safe API in rightkit-tts-qwen.
// State: one small GGUF file in the OS temp directory, removed at the end. No network, no
// process spawn, no GPU, no user data.
use ggml_sys::*;
use std::ffi::{c_char, CStr, CString};

fn text(ptr: *const c_char) -> String {
    assert!(!ptr.is_null(), "ggml returned a null string");
    unsafe { CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn new_context() -> *mut ggml_context {
    let params = ggml_init_params {
        mem_size: 16 * 1024 * 1024,
        mem_buffer: std::ptr::null_mut(),
        no_alloc: false,
    };
    let ctx = unsafe { ggml_init(params) };
    assert!(!ctx.is_null(), "ggml_init returned a null context");
    ctx
}

fn version_and_type_metrics() {
    assert_eq!(text(unsafe { ggml_version() }), "0.9.8");
    assert!(!text(unsafe { ggml_commit() }).is_empty());
    assert_eq!(GGML_MAX_DIMS, 4);
    assert_eq!(GGUF_VERSION, 3);
    unsafe {
        // (type, name, block size, bytes per block, quantised)
        for (kind, name, block, bytes, quantised) in [
            (ggml_type_GGML_TYPE_F32, "f32", 1, 4, false),
            (ggml_type_GGML_TYPE_F16, "f16", 1, 2, false),
            (ggml_type_GGML_TYPE_Q8_0, "q8_0", 32, 34, true),
            (ggml_type_GGML_TYPE_Q4_0, "q4_0", 32, 18, true),
        ] {
            assert_eq!(text(ggml_type_name(kind)), name);
            assert_eq!(ggml_blck_size(kind), block);
            assert_eq!(ggml_type_size(kind), bytes);
            assert_eq!(ggml_is_quantized(kind), quantised);
        }
        assert_eq!(ggml_row_size(ggml_type_GGML_TYPE_Q8_0, 64), 68);
        assert_eq!(ggml_row_size(ggml_type_GGML_TYPE_F32, 10), 40);
    }
}

fn add_graph() {
    unsafe {
        let ctx = new_context();
        let a = ggml_new_tensor_1d(ctx, ggml_type_GGML_TYPE_F32, 1);
        let b = ggml_new_tensor_1d(ctx, ggml_type_GGML_TYPE_F32, 1);
        assert!(!ggml_set_f32(a, 2.0).is_null());
        assert!(!ggml_set_f32(b, 3.0).is_null());
        let sum = ggml_add(ctx, a, b);
        let graph = ggml_new_graph(ctx);
        ggml_build_forward_expand(graph, sum);
        assert_eq!(
            ggml_graph_compute_with_ctx(ctx, graph, 1),
            ggml_status_GGML_STATUS_SUCCESS
        );
        assert!((ggml_get_f32_1d(sum, 0) - 5.0).abs() < 1e-6);
        ggml_free(ctx);
    }
}

fn dot_product_through_mul_mat() {
    unsafe {
        let ctx = new_context();
        let a = ggml_new_tensor_2d(ctx, ggml_type_GGML_TYPE_F32, 3, 1);
        let b = ggml_new_tensor_2d(ctx, ggml_type_GGML_TYPE_F32, 3, 1);
        assert_eq!(ggml_nelements(a), 3);
        for (i, (x, y)) in [(1.0f32, 4.0f32), (2.0, 5.0), (3.0, 6.0)]
            .into_iter()
            .enumerate()
        {
            ggml_set_f32_1d(a, i as i32, x);
            ggml_set_f32_1d(b, i as i32, y);
        }
        let product = ggml_mul_mat(ctx, a, b);
        let graph = ggml_new_graph(ctx);
        ggml_build_forward_expand(graph, product);
        assert_eq!(
            ggml_graph_compute_with_ctx(ctx, graph, 2),
            ggml_status_GGML_STATUS_SUCCESS
        );
        assert_eq!(ggml_nelements(product), 1);
        assert!(
            (ggml_get_f32_1d(product, 0) - 32.0).abs() < 1e-4,
            "1*4 + 2*5 + 3*6"
        );
        ggml_free(ctx);
    }
}

fn quantised_round_trip() {
    unsafe {
        let ctx = new_context();
        let quantised = ggml_new_tensor_1d(ctx, ggml_type_GGML_TYPE_Q8_0, 32);
        assert!(!quantised.is_null());
        let source: Vec<f32> = (0..32).map(|i| i as f32 * 0.125).collect();
        let bytes = ggml_nbytes(quantised);
        assert_eq!(bytes, 34, "one Q8_0 block");
        let mut packed = vec![0u8; bytes];
        let written = ggml_quantize_chunk(
            ggml_type_GGML_TYPE_Q8_0,
            source.as_ptr(),
            packed.as_mut_ptr().cast(),
            0,
            1,
            32,
            std::ptr::null(),
        );
        assert_eq!(written, packed.len());
        std::ptr::copy_nonoverlapping(
            packed.as_ptr(),
            ggml_get_data(quantised).cast(),
            packed.len(),
        );
        let back = ggml_cast(ctx, quantised, ggml_type_GGML_TYPE_F32);
        let graph = ggml_new_graph(ctx);
        ggml_build_forward_expand(graph, back);
        assert_eq!(
            ggml_graph_compute_with_ctx(ctx, graph, 1),
            ggml_status_GGML_STATUS_SUCCESS
        );
        let mut recovered = vec![0.0f32; 32];
        std::ptr::copy_nonoverlapping(ggml_get_data_f32(back), recovered.as_mut_ptr(), 32);
        for (expected, actual) in source.iter().zip(&recovered) {
            assert!((expected - actual).abs() < 0.05, "{expected} vs {actual}");
        }
        ggml_free(ctx);
    }
}

fn gguf_metadata_round_trip() {
    let architecture = CString::new("general.architecture").unwrap();
    let count = CString::new("fixture.block_count").unwrap();
    let missing = CString::new("fixture.absent").unwrap();
    let value = CString::new("consumer-fixture").unwrap();
    unsafe {
        let ctx = gguf_init_empty();
        assert!(!ctx.is_null());
        assert_eq!(gguf_get_n_kv(ctx), 0);
        gguf_set_val_str(ctx, architecture.as_ptr(), value.as_ptr());
        gguf_set_val_u32(ctx, count.as_ptr(), 7);
        assert_eq!(gguf_get_n_kv(ctx), 2);
        assert_eq!(gguf_get_version(ctx), GGUF_VERSION);
        assert_eq!(gguf_find_key(ctx, missing.as_ptr()), -1);
        let key = gguf_find_key(ctx, architecture.as_ptr());
        assert!(key >= 0);
        assert_eq!(text(gguf_get_key(ctx, key)), "general.architecture");
        assert_eq!(text(gguf_get_val_str(ctx, key)), "consumer-fixture");
        assert_eq!(text(gguf_type_name(gguf_get_kv_type(ctx, key))), "str");
        let number = gguf_find_key(ctx, count.as_ptr());
        assert_eq!(gguf_get_val_u32(ctx, number), 7);

        // Serialise the metadata to memory: it starts with the GGUF magic and version 3.
        let size = gguf_get_meta_size(ctx);
        let mut meta = vec![0u8; size];
        gguf_get_meta_data(ctx, meta.as_mut_ptr().cast());
        assert_eq!(&meta[..4], b"GGUF");
        assert_eq!(u32::from_le_bytes(meta[4..8].try_into().unwrap()), 3);
        assert_eq!(gguf_get_alignment(ctx), GGUF_DEFAULT_ALIGNMENT as usize);
        assert_eq!(gguf_remove_key(ctx, count.as_ptr()), number);
        assert_eq!(gguf_get_n_kv(ctx), 1);
        gguf_free(ctx);

        // Read the same bytes back through the file reader.
        let path =
            std::env::temp_dir().join(format!("rk-ggml-consumer-{}.gguf", std::process::id()));
        std::fs::write(&path, &meta).expect("write fixture GGUF");
        let c_path = CString::new(path.to_str().expect("utf-8 temp path")).unwrap();
        let reader = gguf_init_from_file(
            c_path.as_ptr(),
            gguf_init_params {
                no_alloc: true,
                ctx: std::ptr::null_mut(),
            },
        );
        std::fs::remove_file(&path).expect("remove fixture GGUF");
        assert!(
            !reader.is_null(),
            "the GGUF reader accepted the written metadata"
        );
        assert_eq!(gguf_get_n_kv(reader), 2);
        let key = gguf_find_key(reader, architecture.as_ptr());
        assert_eq!(text(gguf_get_val_str(reader, key)), "consumer-fixture");
        assert_eq!(
            gguf_get_val_u32(reader, gguf_find_key(reader, count.as_ptr())),
            7
        );
        gguf_free(reader);

        // Bytes that are not GGUF are refused with a null context, not a crash.
        let junk = std::env::temp_dir().join(format!("rk-ggml-junk-{}.gguf", std::process::id()));
        std::fs::write(&junk, b"not a gguf file at all").expect("write junk");
        let c_junk = CString::new(junk.to_str().expect("utf-8 temp path")).unwrap();
        let refused = gguf_init_from_file(
            c_junk.as_ptr(),
            gguf_init_params {
                no_alloc: true,
                ctx: std::ptr::null_mut(),
            },
        );
        std::fs::remove_file(&junk).expect("remove junk");
        assert!(refused.is_null());
    }
}

fn main() {
    version_and_type_metrics();
    add_graph();
    dot_product_through_mul_mat();
    quantised_round_trip();
    gguf_metadata_round_trip();
    println!("OK rightkit-tts-qwen-sys 0.1.2 (registry consumer)");
}
