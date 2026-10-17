//! Windows minidump helper: `rightkit-dump-helper --pid <pid> --output <file>`.
//! The logic lives in [`rightkit_crash::dump_helper`].

fn main() -> std::process::ExitCode {
    rightkit_crash::dump_helper::run(std::env::args_os().skip(1))
}
