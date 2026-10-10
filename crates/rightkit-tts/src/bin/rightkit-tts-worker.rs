fn main() {
    // Fail closed: an unresolvable data root is an error unless --model-dir names the folder.
    let mut model_dir = rightkit_tts::models::try_model_dir().unwrap_or_default();
    let mut backend = rightkit_tts::models::default_backend().to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--model-dir" => model_dir = args.next().expect("--model-dir needs a path").into(),
            "--backend" => backend = args.next().expect("--backend needs a value"),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(64);
            }
        }
    }
    if model_dir.as_os_str().is_empty() {
        eprintln!(
            "cannot resolve the model folder: pass --model-dir or set RIGHTKIT_TTS_MODEL_DIR"
        );
        std::process::exit(64);
    }
    rightkit_tts::worker::run(&model_dir, &backend)
}
