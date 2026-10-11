use rightkit_tts::models;
use rightkit_tts::worker::{run, WorkerConfig};
use rightkit_tts::worker_args;

fn main() {
    // Fail closed: unknown arguments, a bad `--mode`, or an unresolvable model folder exit 64.
    let args = match worker_args::parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(64);
        }
    };
    // `--idle-secs` wins over RIGHTKIT_TTS_IDLE_SECS and GENRIGHT_QTS_IDLE_SECS.
    let idle_timeout = args.idle_timeout();
    let model_dir = match args.model_dir {
        Some(dir) => dir,
        None => match models::model_dir_in(args.data_root.as_deref()) {
            Ok(dir) if !dir.as_os_str().is_empty() => dir,
            _ => {
                eprintln!(
                    "cannot resolve the model folder: pass --model-dir or set RIGHTKIT_TTS_MODEL_DIR"
                );
                std::process::exit(64);
            }
        },
    };
    let vocoder = args
        .vocoder
        .unwrap_or_else(|| model_dir.join("qwen3-tts-vocoder.onnx"));
    let backend = args
        .backend
        .unwrap_or_else(|| models::default_backend().to_string());
    run(&WorkerConfig {
        model_dir,
        backend,
        mode: args.mode,
        vocoder,
        data_root: args.data_root,
        ort_runtime: args.ort_runtime,
        mode_locations: args.mode_locations,
        idle_timeout,
    })
}
