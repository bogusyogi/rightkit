fn main() {
    let mut model_dir = rightkit_tts::models::model_dir();
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
    rightkit_tts::worker::run(&model_dir, &backend)
}
