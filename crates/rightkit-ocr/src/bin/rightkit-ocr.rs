//! `rightkit-ocr` — worker host and recognition CLI.
//!
//!   rightkit-ocr --rightkit-ocr-worker          serve one request (stdin PNG/JPEG -> JSON)
//!   rightkit-ocr recognize <image> [--backend auto|native|onnx]
//!                          [--isolated] [--model-dir DIR --ort-runtime PATH]
//!
//! Prints one JSON object: backend, fallbackReason, lines[].

use std::path::PathBuf;

use rightkit_ocr::{
    worker, BackendPreference, Isolation, Ocr, OcrArtifactManifest, OcrOptions, OnnxConfig,
    WorkerCommand,
};

fn main() {
    if worker::run_if_requested(worker::DEFAULT_WORKER_ARG) {
        return;
    }
    if let Err(e) = run() {
        eprintln!("rightkit-ocr: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("recognize") {
        return Err("usage: rightkit-ocr recognize <image> [--backend auto|native|onnx] [--isolated] [--model-dir DIR --ort-runtime PATH]".into());
    }
    let image = PathBuf::from(args.next().ok_or("missing image path")?);
    let mut preference = BackendPreference::Auto;
    let mut isolated = false;
    let (mut model_dir, mut runtime) = (None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--backend" => {
                preference = match args.next().as_deref() {
                    Some("auto") => BackendPreference::Auto,
                    Some("native") => BackendPreference::Native,
                    Some("onnx") => BackendPreference::Onnx,
                    other => return Err(format!("bad --backend {other:?}")),
                }
            }
            "--isolated" => isolated = true,
            "--model-dir" => model_dir = args.next().map(PathBuf::from),
            "--ort-runtime" => runtime = args.next().map(PathBuf::from),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let mut options = OcrOptions::new(preference);
    if let Some(dir) = model_dir {
        let manifest = OcrArtifactManifest::pp_ocr_v6();
        let mut cfg = OnnxConfig::new(dir, manifest);
        cfg.ort_runtime = runtime;
        options = options.with_onnx(cfg);
    }
    if isolated {
        options = options.with_isolation(Isolation::Worker(WorkerCommand::current_exe(
            worker::DEFAULT_WORKER_ARG,
        )?));
    }
    let mut ocr = Ocr::new(options);
    let result = ocr.recognize_file(&image).map_err(|e| e.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?
    );
    Ok(())
}
