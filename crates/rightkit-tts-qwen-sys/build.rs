use std::env;
use std::path::{Path, PathBuf};

include!("bindings_stamp.rs");

fn main() {
    let target = env::var("TARGET").expect("TARGET");
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    let ggml_root = env::var("GGML_SRC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("vendor/ggml"));
    let ggml_root = normalize_source_path(ggml_root);
    let include = ggml_root.join("include");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=wrapper.h");
    println!("cargo:rerun-if-changed=bindings_stamp.rs");
    println!("cargo:rerun-if-env-changed=GGML_SRC");
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    println!("cargo:rerun-if-env-changed=BLA_VENDOR");
    println!("cargo:rerun-if-env-changed=GGML_BLAS_VENDOR");
    validate_features(&target);

    if feature_enabled("vulkan") {
        println!(
            "cargo:warning=ggml-sys: Vulkan builds require a Vulkan SDK/loader and `glslc` (for example `libvulkan-dev` and `glslc` on Linux)"
        );
    }

    // GenRight: managed builds cannot inject env vars (RightKit), so discover
    // host tools here instead of relying on CMAKE_GENERATOR / VULKAN_SDK / LIBCLANG_PATH.
    genright_discover_host_tools();

    let mut cfg = cmake::Config::new(&ggml_root);
    cfg.profile("Release");
    if cfg!(windows) && env::var_os("CMAKE_GENERATOR").is_none() && have_command("ninja") {
        // Older CMake cannot name the installed Visual Studio generator; Ninja plus
        // cc-rs's MSVC environment works with any CMake.
        cfg.generator("Ninja");
    }
    cfg.define("BUILD_SHARED_LIBS", "OFF");
    cfg.define("GGML_STATIC", "ON");
    cfg.define("GGML_BUILD_EXAMPLES", "OFF");
    cfg.define("GGML_BUILD_TESTS", "OFF");
    // GenRight: ccache needs host env (LOCALAPPDATA) that managed builds do not pass.
    cfg.define("GGML_CCACHE", "OFF");

    if feature_enabled("native") {
        cfg.define("GGML_NATIVE", "ON");
    } else {
        cfg.define("GGML_NATIVE", "OFF");
    }

    // Metal is Apple-only; silently skip on other targets even if the feature is enabled.
    if feature_enabled("metal") && target.contains("apple") {
        cfg.define("GGML_METAL", "ON");
        cfg.define("GGML_METAL_EMBED_LIBRARY", "ON");
    } else {
        cfg.define("GGML_METAL", "OFF");
    }

    if feature_enabled("blas") {
        cfg.define("GGML_BLAS", "ON");
        if let Some(blas_vendor) = env::var("GGML_BLAS_VENDOR")
            .ok()
            .or_else(|| env::var("BLA_VENDOR").ok())
            .filter(|value| !value.trim().is_empty())
        {
            cfg.define("BLA_VENDOR", &blas_vendor);
        }
        if target.contains("apple") {
            cfg.define("GGML_ACCELERATE", "ON");
        }
    } else {
        cfg.define("GGML_BLAS", "OFF");
    }

    map_feature_cmake(&mut cfg, "vulkan", "GGML_VULKAN");

    let dst = cfg.build();
    let lib_dir = find_lib_dir(&dst, &out_dir).unwrap_or_else(|| {
        panic!(
            "ggml-sys: could not locate static libs under cmake output {:?} or {:?}/build",
            dst, out_dir
        )
    });
    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    // Link order: dependents before their dependencies (GNU ld).
    println!("cargo:rustc-link-lib=static=ggml");
    if feature_enabled("metal") && target.contains("apple") {
        println!("cargo:rustc-link-lib=static=ggml-metal");
    }
    if feature_enabled("vulkan") {
        println!("cargo:rustc-link-lib=static=ggml-vulkan");
        emit_vulkan_loader_links(&target);
    }
    if feature_enabled("blas") {
        println!("cargo:rustc-link-lib=static=ggml-blas");
    }

    println!("cargo:rustc-link-lib=static=ggml-cpu");
    println!("cargo:rustc-link-lib=static=ggml-base");

    if feature_enabled("metal") && target.contains("apple") {
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=MetalKit");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
    if target.contains("apple") {
        println!("cargo:rustc-link-lib=framework=Accelerate");
    }

    if target.contains("apple") {
        println!("cargo:rustc-link-lib=c++");
    } else if target.contains("windows") && target.contains("msvc") {
        // C++ runtime linked via ggml's MSVC build flags.
        println!("cargo:rustc-link-lib=advapi32");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }

    if target.contains("linux") {
        println!("cargo:rustc-link-lib=gomp");
        println!("cargo:rustc-link-lib=pthread");
        println!("cargo:rustc-link-lib=m");
        println!("cargo:rustc-link-lib=dl");
    }

    generate_bindings(&ggml_root, &include, &out_dir, &manifest_dir, &target);
}

fn feature_enabled(name: &str) -> bool {
    env::var(format!(
        "CARGO_FEATURE_{}",
        name.to_ascii_uppercase().replace('-', "_")
    ))
    .is_ok()
}

fn normalize_source_path(path: PathBuf) -> PathBuf {
    let path = path
        .canonicalize()
        .unwrap_or_else(|e| panic!("ggml source path missing or invalid ({path:?}): {e}"));

    #[cfg(windows)]
    {
        if let Some(stripped) = path
            .to_str()
            .and_then(|raw| raw.strip_prefix(r"\\?\"))
            .map(PathBuf::from)
        {
            return stripped;
        }
    }

    path
}

fn map_feature_cmake(cfg: &mut cmake::Config, feature: &str, cmake_opt: &str) {
    if feature_enabled(feature) {
        cfg.define(cmake_opt, "ON");
    }
}

fn validate_features(target: &str) {
    if feature_enabled("metal") && !target.contains("apple") {
        println!("cargo:warning=ggml-sys: `metal` feature ignored on non-Apple target ({target})");
    }
}

fn emit_vulkan_loader_links(target: &str) {
    for dir in vulkan_search_dirs(target) {
        if dir.exists() {
            println!("cargo:rustc-link-search=native={}", dir.display());
        }
    }

    let lib = if target.contains("apple") {
        "dylib=vulkan"
    } else if target.contains("windows") {
        "vulkan-1"
    } else {
        "vulkan"
    };

    println!("cargo:rustc-link-lib={lib}");
}

fn vulkan_search_dirs(target: &str) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(sdk) = env::var("VULKAN_SDK") {
        let sdk = PathBuf::from(sdk);
        if target.contains("windows") {
            dirs.push(sdk.join("Lib"));
        } else {
            dirs.push(sdk.join("lib"));
            if target.contains("apple") {
                dirs.push(sdk.join("macOS").join("lib"));
            }
        }
    }
    if target.contains("apple") {
        dirs.push(PathBuf::from("/opt/homebrew/lib"));
        dirs.push(PathBuf::from("/usr/local/lib"));
    }
    dirs
}

fn find_lib_dir(dst: &Path, out_dir: &Path) -> Option<PathBuf> {
    let candidates = [
        dst.join("src"),
        dst.join("lib"),
        dst.to_path_buf(),
        out_dir.join("build").join("src"),
        out_dir.join("build").join("lib"),
        out_dir.join("build").join("Release").join("src"),
        out_dir.join("build").join("Debug").join("src"),
    ];
    candidates.into_iter().find(|p| has_ggml(p))
}

fn has_ggml(dir: &Path) -> bool {
    dir.join("libggml.a").exists()
        || dir.join("libggml.lib").exists()
        || dir.join("ggml.lib").exists()
}

/// Checked-in bindings are `bindings-<target>.rs` beside Cargo.toml (see VERSIONS.md).
/// With `regenerate-bindings`, bindgen rewrites the checked-in file for this target
/// from the vendored headers; otherwise the checked-in file is copied into OUT_DIR.
#[cfg(feature = "regenerate-bindings")]
fn generate_bindings(
    ggml_root: &Path,
    include: &Path,
    out_dir: &Path,
    manifest_dir: &Path,
    target: &str,
) {
    let body = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{}", include.display()))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .allowlist_function("ggml_.*")
        .allowlist_function("gguf_.*")
        .allowlist_type("ggml_.*")
        .allowlist_type("gguf_.*")
        .allowlist_var("GGML_.*")
        .allowlist_var("GGUF_.*")
        .size_t_is_usize(true)
        .generate()
        .expect("bindgen failed on ggml headers")
        .to_string();

    let ggml = ggml_version_from_cmake(
        &std::fs::read_to_string(ggml_root.join("CMakeLists.txt")).expect("ggml CMakeLists.txt"),
    )
    .expect("ggml version in CMakeLists.txt");
    let headers = ggml_headers_digest(include).expect("digest ggml headers");
    let text = format!(
        "{}\n// Generated by rust-bindgen from vendor/ggml/include. Regenerate: see VERSIONS.md.\n{body}",
        stamp_line(&ggml, &headers, target)
    );

    let checked_in = manifest_dir.join(format!("bindings-{target}.rs"));
    std::fs::write(&checked_in, &text).expect("write checked-in bindings");
    std::fs::write(out_dir.join("bindings.rs"), &text).expect("write OUT_DIR bindings.rs");
    println!(
        "cargo:warning=ggml-sys: regenerated {} (ggml {ggml}, headers {headers})",
        checked_in.display()
    );
}

#[cfg(not(feature = "regenerate-bindings"))]
fn generate_bindings(
    _ggml_root: &Path,
    _include: &Path,
    out_dir: &Path,
    manifest_dir: &Path,
    target: &str,
) {
    let checked_in = manifest_dir.join(format!("bindings-{target}.rs"));
    if !checked_in.is_file() {
        panic!(
            "ggml-sys: no checked-in bindings for {target} ({}). Build with `--features regenerate-bindings` on a host with libclang to generate them (see VERSIONS.md).",
            checked_in.display()
        );
    }
    std::fs::copy(&checked_in, out_dir.join("bindings.rs")).expect("copy checked-in bindings.rs");
}

// ---- GenRight additions (see ../../PINNED.md) ----

fn have_command(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn set_if_unset(key: &str, value: impl AsRef<std::ffi::OsStr>) {
    if env::var_os(key).is_none_or(|v| v.is_empty()) {
        env::set_var(key, value);
    }
}

fn genright_discover_host_tools() {
    // A PATH `cmake` may resolve to a thin shim (e.g. the `cmake` Python wheel's
    // script) whose interpreter is unreachable inside a managed/sandboxed build;
    // point CMAKE at a real binary the shim wraps when one exists on disk.
    if env::var_os("CMAKE").is_none() {
        let mut candidates: Vec<PathBuf> = vec![
            PathBuf::from("/opt/homebrew/bin/cmake"),
            PathBuf::from("/usr/local/bin/cmake"),
        ];
        if let Some(home) = env::var_os("HOME") {
            let lib = PathBuf::from(&home).join(".local/lib");
            if let Ok(rd) = std::fs::read_dir(&lib) {
                for e in rd.flatten() {
                    candidates.push(e.path().join("site-packages/cmake/data/bin/cmake"));
                }
            }
        }
        if cfg!(windows) {
            if let Some(local) = env::var_os("LOCALAPPDATA") {
                let pyroot = PathBuf::from(&local).join("Programs\\Python");
                if let Ok(rd) = std::fs::read_dir(&pyroot) {
                    for e in rd.flatten() {
                        candidates.push(
                            e.path()
                                .join("Lib\\site-packages\\cmake\\data\\bin\\cmake.exe"),
                        );
                    }
                }
            }
            candidates.push(PathBuf::from("C:\\Program Files\\CMake\\bin\\cmake.exe"));
        }
        if let Some(found) = candidates.into_iter().find(|p| p.is_file()) {
            set_if_unset("CMAKE", found);
        }
    }
    if !cfg!(windows) {
        return;
    }
    // Latest Vulkan SDK under C:\VulkanSDK (loader import lib + glslc).
    if env::var_os("VULKAN_SDK").is_none() {
        if let Ok(rd) = std::fs::read_dir("C:\\VulkanSDK") {
            let mut sdks: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.join("Lib").join("vulkan-1.lib").exists())
                .collect();
            sdks.sort();
            if let Some(sdk) = sdks.pop() {
                set_if_unset("VULKAN_SDK", &sdk);
            }
        }
    }
    if let Some(sdk) = env::var_os("VULKAN_SDK") {
        let bin = PathBuf::from(sdk).join("Bin");
        let mut paths = vec![bin];
        if let Some(existing) = env::var_os("PATH") {
            paths.extend(env::split_paths(&existing));
        }
        if let Ok(joined) = env::join_paths(paths) {
            env::set_var("PATH", joined);
        }
    }
    // libclang for bindgen: LLVM, or the `libclang` Python wheel.
    if env::var_os("LIBCLANG_PATH").is_none() {
        let mut dirs: Vec<PathBuf> = vec![PathBuf::from("C:\\Program Files\\LLVM\\bin")];
        for var in ["APPDATA", "LOCALAPPDATA"] {
            if let Some(base) = env::var_os(var) {
                let python = PathBuf::from(base).join(if var == "APPDATA" {
                    "Python"
                } else {
                    "Programs\\Python"
                });
                if let Ok(rd) = std::fs::read_dir(&python) {
                    for e in rd.flatten() {
                        dirs.push(e.path().join("site-packages").join("clang").join("native"));
                    }
                }
            }
        }
        if let Some(dir) = dirs.into_iter().find(|d| d.join("libclang.dll").exists()) {
            set_if_unset("LIBCLANG_PATH", dir);
        }
    }
}
