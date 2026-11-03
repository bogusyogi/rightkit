//! Parse a command line and run the command it names.

use crate::cli::{
    self, AdaptCommand, Command, ContextCommand, MemoryCommand, ReceiptsCommand, TranscriptCommand,
};
use crate::modes::{self, DispatchOutcome};
use crate::{EXIT_INTERNAL_ERROR, EXIT_NOT_AVAILABLE, EXIT_OK, EXIT_USER_ERROR};
use std::ffi::OsString;

/// Run one command line (argument zero is the program name) and return the exit code.
///
/// A `hook` or `receipts confirm` command line always returns 0: whatever the host passed, a
/// hook that fails would fail the agent turn it is attached to (`confirm` runs from the host app's
/// Stop hook), so its parse errors become one line on stderr.
pub fn run<I, T>(args: I) -> i32
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let word = |index: usize| args.get(index).and_then(|arg| arg.to_str());
    let always_ok = if word(1) == Some("hook") {
        Some("hook")
    } else if word(1) == Some("receipts") && word(2) == Some("confirm") {
        Some("receipts confirm")
    } else {
        None
    };
    let parsed = match cli::parse(args) {
        Ok(parsed) => parsed,
        Err(error) if always_ok.is_some() => {
            let line = error.to_string();
            eprintln!(
                "membrane: {}: {}",
                always_ok.unwrap_or("hook"),
                line.lines().next().unwrap_or("invalid arguments")
            );
            return EXIT_OK;
        }
        Err(error) => {
            // Help and version go to stdout with code 0; usage errors go to stderr with 2.
            let _ = error.print();
            return if error.use_stderr() {
                EXIT_USER_ERROR
            } else {
                EXIT_OK
            };
        }
    };
    match execute(parsed.command) {
        DispatchOutcome::Ok => EXIT_OK,
        DispatchOutcome::UserError(error) => {
            eprintln!("membrane: {error}");
            EXIT_USER_ERROR
        }
        DispatchOutcome::NotAvailable(error) => {
            eprintln!("membrane: {error}");
            EXIT_NOT_AVAILABLE
        }
        DispatchOutcome::InternalError(error) => {
            eprintln!("membrane: internal: {error}");
            EXIT_INTERNAL_ERROR
        }
        DispatchOutcome::Exit(code) => code,
    }
}

fn execute(command: Command) -> DispatchOutcome {
    match command {
        Command::Context {
            command: ContextCommand::Pull(args),
        } => modes::context_pull(&args),
        Command::Memory {
            command: MemoryCommand::Push(args),
        } => modes::memory_push(&args),
        Command::Memory {
            command: MemoryCommand::Search(args),
        } => modes::memory_search(&args),
        Command::Memory {
            command: MemoryCommand::Delete(args),
        } => modes::memory_delete(&args),
        Command::Memory {
            command: MemoryCommand::Durable(rest),
        } => modes::memory_durable(&rest),
        Command::Adapt {
            command: AdaptCommand::Observe(args),
        } => modes::adapt_observe(&args),
        Command::Adapt {
            command: AdaptCommand::Recall(args),
        } => modes::adapt_recall(&args),
        Command::Adapt {
            command: AdaptCommand::Apply(args),
        } => modes::adapt_apply(&args),
        Command::Adapt {
            command: AdaptCommand::TasteReview(args),
        } => modes::adapt_taste_review(&args),
        Command::Adapt {
            command: AdaptCommand::Status(args),
        } => modes::adapt_status(&args),
        Command::Adapt {
            command: AdaptCommand::AdjudicateTaste(args),
        } => modes::adapt_adjudicate_taste(&args),
        Command::Adapt {
            command: AdaptCommand::MineSession(args),
        } => modes::adapt_mine_session(&args),
        Command::Transcript {
            command: TranscriptCommand::Append(args),
        } => modes::transcript_append(&args),
        Command::Transcript {
            command: TranscriptCommand::Tip(args),
        } => modes::transcript_tip(&args),
        Command::History(args) => modes::history(&args.words),
        Command::Blueprint(args) => modes::blueprint(&args),
        Command::Status(args) => modes::status(&args),
        Command::Where(args) => modes::where_binary(&args),
        Command::Hook(args) => modes::hook_event(&args),
        Command::Receipts {
            command: ReceiptsCommand::Confirm(args),
        } => modes::receipts_confirm(&args),
        Command::Receipts {
            command: ReceiptsCommand::List(args),
        } => modes::receipts_list(&args),
        Command::Receipts {
            command: ReceiptsCommand::Mark(args),
        } => modes::receipts_mark(&args),
        Command::Serve => modes::serve(),
        Command::Init(args) => modes::init(&args),
    }
}
