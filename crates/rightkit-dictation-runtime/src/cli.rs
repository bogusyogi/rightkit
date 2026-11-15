//! The installed command line for file transcription (`<program> transcribe`), taken from HeardRight (#309).
//!
//! It runs only when the host turned it on with [`crate::host::configure_cli`]; the program,
//! product and message prefix come from there.
//!
//! The CLI is a speech client. It never loads a model: it submits the file to the machine-wide
//! speech service (`speech_service`), which queues it behind dictation. With the app running that
//! is the shell's engine; with the app closed the CLI starts this same executable as a headless
//! `--speech-service` host, then submits as above. The host exits after
//! [`crate::speech_service::HEADLESS_IDLE_EXIT`] idle.
//!
//! Everything here before `run_transcribe` is pure (argv parsing, formats, the exit-code map).

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_speech_client::{
    start_installed_engine, transcribe_via_service_policy, ClientError, EngineLaunch,
    EngineSelector, FileTranscriptResult, HeardRightLauncher, HostRole, Launch, Progress,
    QueueReason, TranscribeOptions,
};

/// Exit codes. Documented in `docs/ENGINE_CLI.md`; agents key off them.
pub const EXIT_OK: i32 = 0;
pub const EXIT_OTHER: i32 = 1;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_INPUT: i32 = 3;
pub const EXIT_NO_MODEL: i32 = 4;
pub const EXIT_SERVICE: i32 = 5;
pub const EXIT_CANCELLED: i32 = 130;

/// How long the CLI waits for a host it started to publish its discovery record. The record
/// appears only after the model is warm, and a first CoreML/ANE compile can take minutes. The
/// spawn gives no child handle, so a host that dies at start is noticed only at this bound.
const HOST_START_WAIT: Duration = Duration::from_secs(15 * 60);
/// Stall bound for a host without the job-queue contract (liveness-watched hosts ignore it).
/// Not a job limit: the service's own queue and liveness rules are the only limits on a v1.3 host.
const STALL_NON_QUEUE: Duration = Duration::from_secs(6 * 60 * 60);

fn cli() -> crate::host::CliIdentity {
    crate::host::cli_identity()
        .copied()
        .unwrap_or(crate::host::CliIdentity {
            program: "rightkit-engine",
            product: "RightKit",
            message_prefix: "rightkit",
        })
}

fn usage_text() -> String {
    let cli = cli();
    format!(
        "\
Usage:
  {program} transcribe <file> [--format txt|srt|vtt|json] [--out <path>]
  {program} help [transcribe]
  {program} --version

transcribe sends <file> to the {product} speech service (the running app, or a headless host it
starts) and prints the transcript. It queues behind dictation and never loads a model itself.

Options:
  --format <f>   txt (default), srt, vtt, or json. srt, vtt and json need word timings, which
                 only the Parakeet model provides.
  --out <path>   Write the transcript to <path> atomically; stdout stays empty.

Exit codes: 0 ok, 1 other error, 2 usage, 3 input missing or unreadable, 4 no model installed,
5 speech service busy, unavailable or timed out, 130 cancelled.",
        program = cli.program,
        product = cli.product
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Txt,
    Srt,
    Vtt,
    Json,
}

impl Format {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "txt" => Some(Self::Txt),
            "srt" => Some(Self::Srt),
            "vtt" => Some(Self::Vtt),
            "json" => Some(Self::Json),
            _ => None,
        }
    }

    /// Whether the output needs the word timings Parakeet returns (Whisper returns none).
    pub fn needs_word_timings(self) -> bool {
        !matches!(self, Self::Txt)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscribeArgs {
    pub file: PathBuf,
    pub format: Format,
    pub out: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Transcribe(TranscribeArgs),
}

/// A CLI failure, already sorted into its exit-code class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    Usage(String),
    Input(String),
    NoModel(String),
    Service(String),
    Cancelled,
    Other(String),
}

impl CliError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => EXIT_USAGE,
            Self::Input(_) => EXIT_INPUT,
            Self::NoModel(_) => EXIT_NO_MODEL,
            Self::Service(_) => EXIT_SERVICE,
            Self::Cancelled => EXIT_CANCELLED,
            Self::Other(_) => EXIT_OTHER,
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::Usage(m)
            | Self::Input(m)
            | Self::NoModel(m)
            | Self::Service(m)
            | Self::Other(m) => m.clone(),
            Self::Cancelled => "cancelled".to_string(),
        }
    }
}

pub fn usage() -> String {
    usage_text()
}

/// Parse argv (without the program name). `None` means this is not a CLI invocation and the
/// engine's other roles (`--speech-service`, `calibrate`, the stdio launch) own it.
pub fn parse(args: &[OsString]) -> Option<Result<Command, CliError>> {
    let first = args.first()?.to_str().unwrap_or("");
    match first {
        "--version" | "-V" => Some(Ok(Command::Version)),
        "--help" | "-h" | "help" => Some(Ok(Command::Help)),
        "transcribe" => Some(parse_transcribe(&args[1..])),
        _ => None,
    }
}

fn parse_transcribe(args: &[OsString]) -> Result<Command, CliError> {
    let usage = |message: String| CliError::Usage(format!("{message}\n\n{}", usage_text()));
    let mut file: Option<PathBuf> = None;
    let mut format = Format::Txt;
    let mut out: Option<PathBuf> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let text = arg.to_str();
        match text {
            // `transcribe --help` / `-h` prints usage, whatever else is on the line.
            Some("--help" | "-h") => return Ok(Command::Help),
            Some("--format") => {
                let value = iter
                    .next()
                    .ok_or_else(|| usage("--format needs a value".into()))?;
                let value = value.to_str().unwrap_or("");
                format = Format::parse(value).ok_or_else(|| {
                    usage(format!(
                        "unknown format `{value}`; use txt, srt, vtt or json"
                    ))
                })?;
            }
            Some("--out") => {
                let value = iter
                    .next()
                    .ok_or_else(|| usage("--out needs a path".into()))?;
                if out.replace(PathBuf::from(value)).is_some() {
                    return Err(usage("--out given twice".into()));
                }
            }
            Some(flag) if flag.starts_with("--format=") => {
                let value = flag.strip_prefix("--format=").unwrap_or_default();
                format = Format::parse(value).ok_or_else(|| {
                    usage(format!(
                        "unknown format `{value}`; use txt, srt, vtt or json"
                    ))
                })?;
            }
            Some(flag) if flag.starts_with("--out=") => {
                let value = flag.strip_prefix("--out=").unwrap_or_default();
                if out.replace(PathBuf::from(value)).is_some() {
                    return Err(usage("--out given twice".into()));
                }
            }
            Some(flag) if flag.starts_with('-') && flag.len() > 1 => {
                return Err(usage(format!("unknown option `{flag}`")));
            }
            _ => {
                if file.replace(PathBuf::from(arg)).is_some() {
                    return Err(usage("only one input file is accepted".into()));
                }
            }
        }
    }
    let file = file.ok_or_else(|| usage("transcribe needs an input file".into()))?;
    if out.as_ref().is_some_and(|o| o.as_os_str().is_empty()) {
        return Err(usage("--out needs a non-empty path".into()));
    }
    Ok(Command::Transcribe(TranscribeArgs { file, format, out }))
}

/// Sort a speech-client error into its exit-code class.
pub fn classify(error: &ClientError) -> CliError {
    let text = error.to_string();
    match error {
        ClientError::InputUnreadable { .. } | ClientError::StageLimitExceeded { .. } => {
            CliError::Input(text)
        }
        ClientError::Engine { code, message } => {
            if code == "E_PERMISSION" || message.starts_with("file not found") {
                CliError::Input(message.clone())
            } else if message.contains("missing model artifact") {
                CliError::NoModel(format!("no speech model installed: {message}"))
            } else if matches!(
                code.as_str(),
                "model_lease_unavailable"
                    | "E_QUEUE_FULL"
                    | rightkit_speech_client::ERROR_ENGINE_UNAVAILABLE
                    | rightkit_speech_client::ERROR_NOT_ALLOWED
            ) {
                CliError::Service(text)
            } else {
                CliError::Other(text)
            }
        }
        ClientError::NoService
        | ClientError::NoLiveHost
        | ClientError::HandoffRetry
        | ClientError::HostGone
        | ClientError::Timeout
        | ClientError::Unsupported(_)
        | ClientError::ProtocolMismatch { .. }
        | ClientError::Refused { .. } => CliError::Service(text),
        ClientError::Cancelled => CliError::Cancelled,
        ClientError::Io(_) => CliError::Other(text),
    }
}

/// The bytes to print or write for `format`, or the reason the result cannot honour it.
///
/// `srt`/`vtt`/`json` require Parakeet word timings: a host serving Whisper (its result label
/// names it) or returning no words for non-empty text refuses with a service error.
pub fn render(result: &FileTranscriptResult, format: Format) -> Result<String, CliError> {
    if format.needs_word_timings() {
        let whisper = result
            .engine
            .as_deref()
            .is_some_and(|label| label.to_ascii_lowercase().contains("whisper"));
        if whisper {
            return Err(CliError::Service(
                format!(
                    "the speech host is serving Whisper, which returns no word timings; use --format txt or switch {} to Parakeet",
                    cli().product
                ),
            ));
        }
        if result.transcript.words.is_empty() && !result.transcript.text.trim().is_empty() {
            return Err(CliError::Service(
                "the speech host returned no word timings; use --format txt".into(),
            ));
        }
    }
    let transcript = &result.transcript;
    Ok(match format {
        Format::Txt => format!("{}\n", transcript.text.trim_end()),
        Format::Srt => transcript.srt.clone(),
        Format::Vtt => transcript.vtt.clone(),
        Format::Json => {
            let mut text = serde_json::to_string_pretty(&serde_json::json!({
                "text": transcript.text,
                "words": transcript.words,
                "srt": transcript.srt,
                "vtt": transcript.vtt,
                "engine": result.engine,
                "language": result.language,
            }))
            .map_err(|error| CliError::Other(format!("encode json: {error}")))?;
            text.push('\n');
            text
        }
    })
}

/// Run argv. `None`: not a CLI invocation. `Some(code)`: the process exit code.
pub fn dispatch(args: &[OsString]) -> Option<i32> {
    crate::host::cli_identity()?;
    let parsed = parse(args)?;
    attach_parent_console();
    let command = match parsed {
        Ok(command) => command,
        Err(error) => return Some(fail(&error)),
    };
    Some(match command {
        Command::Help => {
            print_stdout(&usage_text());
            EXIT_OK
        }
        Command::Version => {
            print_stdout(crate::ipc::engine_version());
            EXIT_OK
        }
        Command::Transcribe(args) => match run_transcribe(&args) {
            Ok(()) => EXIT_OK,
            Err(error) => fail(&error),
        },
    })
}

fn fail(error: &CliError) -> i32 {
    let _ = writeln!(
        std::io::stderr(),
        "{}: {}",
        cli().message_prefix,
        error.message()
    );
    error.exit_code()
}

fn print_stdout(text: &str) {
    // A closed pipe (`| head`) is not an error worth a different exit code.
    let _ = writeln!(std::io::stdout(), "{text}");
}

/// The input as the host will see it: it resolves relative paths against its own working
/// directory, never ours.
fn absolute_input(file: &Path) -> Result<PathBuf, CliError> {
    std::path::absolute(file)
        .map_err(|error| CliError::Input(format!("cannot resolve {}: {error}", file.display())))
}

fn run_transcribe(args: &TranscribeArgs) -> Result<(), CliError> {
    let input = absolute_input(&args.file)?;
    // Read one byte in THIS process: a denied or missing file is exit 3 before any service work.
    rightkit_dictation_inference::file_transcribe::preflight_read(&input)
        .map_err(CliError::Input)?;
    // Fail on an unwritable --out before a long transcription, not after.
    if let Some(out) = &args.out {
        check_out_dir(out)?;
    }

    let suite_root = rightkit_dictation_models::shared::store::paths::suite_root();
    let mut options = TranscribeOptions::new(STALL_NON_QUEUE).on_progress(progress_reporter());
    if args.format.needs_word_timings() {
        // The service ignores the selector today (it serves its launch backend); `render` checks
        // the result label, so a Whisper host still ends in exit 5.
        options = options.engine(EngineSelector::parakeet());
    }
    let submit = |options: &mut TranscribeOptions| {
        transcribe_via_service_policy(
            &suite_root,
            HostRole::HeardRightHeadless,
            &input,
            options,
            Launch::Never,
            &HeardRightLauncher,
        )
    };
    let mut result = submit(&mut options);
    if matches!(result, Err(ClientError::NoLiveHost)) {
        start_own_host(&suite_root)?;
        // Same request id, so a resubmit is the same job.
        result = submit(&mut options);
    }
    let rendered = render(&result.map_err(|error| classify(&error))?, args.format)?;
    match &args.out {
        Some(out) => write_atomic(out, rendered.as_bytes()),
        None => {
            let mut stdout = std::io::stdout().lock();
            // A closed pipe is the reader's choice.
            let _ = stdout.write_all(rendered.as_bytes());
            let _ = stdout.flush();
            Ok(())
        }
    }
}

/// App closed: start this executable as a headless speech host. It serves Parakeet from the
/// derived models root, so no Parakeet install is exit 4 here, before anything is spawned.
fn start_own_host(suite_root: &Path) -> Result<(), CliError> {
    // The engine's roots, as `entry::run` installs them for a service host.
    crate::engine_paths::install();
    if crate::entry::resolve_speech_service_parakeet_dir().is_none() {
        return Err(CliError::NoModel(format!(
            "no speech model installed: open {} once to finish model setup",
            cli().product
        )));
    }
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|error| CliError::Other(format!("cannot locate this executable: {error}")))?;
    let _ = writeln!(
        std::io::stderr(),
        "{}: starting the speech host (first load can take minutes)",
        cli().message_prefix
    );
    // `HeardRightLauncher` cannot do this: `plan_host` answers HostMyself for HeardRight roles.
    start_installed_engine(
        suite_root,
        &exe,
        &EngineLaunch::heardright(),
        HOST_START_WAIT,
    )
    .map_err(|error| classify(&error))
}

fn check_out_dir(out: &Path) -> Result<(), CliError> {
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    if parent.is_dir() {
        Ok(())
    } else {
        Err(CliError::Usage(format!(
            "--out directory does not exist: {}",
            parent.display()
        )))
    }
}

/// Temp file in the destination directory, then rename: a reader never sees a partial file.
fn write_atomic(out: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let fail = |error: std::io::Error| CliError::Other(format!("write {}: {error}", out.display()));
    let mut temp = out.as_os_str().to_owned();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let written = std::fs::File::create(&temp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let result = written.and_then(|()| std::fs::rename(&temp, out));
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp);
        return Err(fail(error));
    }
    Ok(())
}

/// Queue and run state on stderr, one line per change, so an agent can see why it is waiting.
fn progress_reporter() -> impl FnMut(&Progress) + Send + 'static {
    let mut last = String::new();
    move |progress| {
        let line = match progress {
            Progress::Queued {
                position, reason, ..
            } => match reason {
                QueueReason::DictationActive => {
                    format!("queued behind dictation (position {position})")
                }
                QueueReason::BehindJobs => {
                    format!("queued behind other jobs (position {position})")
                }
                QueueReason::LoadingModel => "waiting for the model to load".to_string(),
                QueueReason::Other(why) => format!("queued: {why}"),
            },
            Progress::Running { .. } => "transcribing".to_string(),
            Progress::Reconnecting { .. } | Progress::HandoffRetry { .. } => {
                "speech host changed; resubmitting the same job".to_string()
            }
            _ => return,
        };
        if line != last {
            let _ = writeln!(std::io::stderr(), "{}: {line}", cli().message_prefix);
            last = line;
        }
    }
}

/// The engine is a GUI-subsystem program on Windows (no console of its own), so a CLI run from a
/// terminal attaches to the parent's console and points any missing standard handle at it.
/// Redirected handles (pipes, files) are left alone. No-op on other platforms.
#[cfg(windows)]
fn attach_parent_console() {
    use std::os::windows::io::IntoRawHandle;

    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: u32) -> isize;
        fn SetStdHandle(which: u32, handle: isize) -> i32;
    }
    // Fails harmlessly when the parent has no console (an agent's pipes, a service).
    // SAFETY: plain Win32 calls with no pointers; the handles set are leaked on purpose so they
    // live for the process.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let current = GetStdHandle(which);
            if current != 0 && current != -1 {
                continue;
            }
            if let Ok(console) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                SetStdHandle(which, console.into_raw_handle() as isize);
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}
