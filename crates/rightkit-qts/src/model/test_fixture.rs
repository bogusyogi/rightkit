//! Tiny local GGUF fixtures; no model artifacts or network access.
use qts_ggml::sys;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(crate) struct TestGguf(pub PathBuf);

impl TestGguf {
    pub(crate) fn write(
        integers: &[(&str, u32)],
        strings: &[(&str, &str)],
        tensors: &[(&str, &[i64], &[f32])],
    ) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "rightkit-qts-{}-{}.gguf",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let filename = CString::new(path.to_string_lossy().as_ref()).unwrap();
        unsafe {
            let gguf = sys::gguf_init_empty();
            let ctx = sys::ggml_init(sys::ggml_init_params {
                mem_size: 1024 * 1024,
                mem_buffer: std::ptr::null_mut(),
                no_alloc: false,
            });
            assert!(!gguf.is_null() && !ctx.is_null());
            for &(key, value) in integers {
                sys::gguf_set_val_u32(gguf, CString::new(key).unwrap().as_ptr(), value);
            }
            for &(key, value) in strings {
                sys::gguf_set_val_str(
                    gguf,
                    CString::new(key).unwrap().as_ptr(),
                    CString::new(value).unwrap().as_ptr(),
                );
            }
            for &(name, dims, values) in tensors {
                assert_eq!(dims.iter().product::<i64>() as usize, values.len());
                let tensor = sys::ggml_new_tensor(
                    ctx,
                    sys::ggml_type_GGML_TYPE_F32,
                    dims.len() as i32,
                    dims.as_ptr(),
                );
                assert!(!tensor.is_null());
                sys::ggml_set_name(tensor, CString::new(name).unwrap().as_ptr());
                std::ptr::copy_nonoverlapping(
                    values.as_ptr(),
                    (*tensor).data.cast::<f32>(),
                    values.len(),
                );
                sys::gguf_add_tensor(gguf, tensor);
            }
            assert!(sys::gguf_write_to_file(gguf, filename.as_ptr(), false));
            sys::gguf_free(gguf);
            sys::ggml_free(ctx);
        }
        Self(path)
    }
}

impl Drop for TestGguf {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
