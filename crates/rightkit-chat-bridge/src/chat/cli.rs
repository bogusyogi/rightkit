//! `<cli> chat …`: message AI chats through the host app, here and on linked computers. The
//! only agent-facing surface. The command name and wording come from the installed brand
//! (`@cli@` and `@product@` in the text below are filled in at run time).
//!
//! ```text
//! peers [--local] [--json]           every chat that can be messaged, here and on linked computers
//! send `<chat>` [`<text>`…] [--file PATH]… [--from CHAT] [--stdin] [--json] [--]   everything after `--` is text
//! reply `<message-id>` [`<text>`…] [--file PATH]… [--stdin] [--json] [--]           answer a received message
//!   `--file` (repeatable, #592): the files go first through @product@ Send over the device link to
//!   the chat's computer, then the message, whose header there says where they were saved.
//! said `<message-id|chat>` [--limit N] [--json]   what the user typed in the sender's chat
//! link `<device>` `<ssh-host>` [--@cli@ PATH] [--json]   link a computer reachable over ssh
//! unlink `<device>`
//! status [--json]                    hub, chats here, linked computers
//! whoami [--json]                    how this command's sender is proven (read-only)
//! install|uninstall [--claude] [--codex] [--muse] [--dry-run]   the @product@ skill for Claude and Codex, the plugin for Muse
//! hook-delivery on|off|auto [--json]   how messages reach a busy Claude chat (chatClaudeHookDelivery)
//! policy [list|get <key>|set <key> on|off] [--json]   the chat switches the notch does not publish
//! inbox [--from CHAT|--session ID] [--all] [--ack] [--json]   messages kept for a chat
//! post `<base64 envelope>`             (run over ssh by a linked computer) deliver one message here
//!
//! `said` exits 2 when it cannot show the sender's transcript (no route, no transcript, Chat off).
//! `send` and `reply` exit 0 for delivered/queued/held/sent, 2 for refused/unsupported and 3
//! when the outcome is unknown; the receipt is printed in every case.
//! ```

use super::brand::brand;
use super::links::{self, Link};
use super::mesh::{self, MeshChat, MeshFailure, MeshPeer};
use super::roster;
use super::said::{self, SaidError};
use super::store::Store;
use super::{
    self as chat, Caller, Pick, SendOutcome, confirm, control, identify, install, local_identity,
    receipts,
};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::sync::OnceLock;
use std::time::Duration;

/// Why a call to the host's resident engine did not produce an answer.
#[derive(Debug)]
pub enum EngineFailure {
    /// Nothing ran: no engine was found or it was not ready or was closing. Safe to retry later.
    Unreachable(String),
    /// The request was sent but no answer arrived (lost link, timeout). The engine may have acted.
    Unknown(String),
    /// The engine answered with a refusal, with its stable code.
    Refused { code: String, message: String },
}

/// What the host app supplies to the command line: its resident engine (which holds the device
/// link) and its file transfer. The words, command name and paths come from the installed
/// [`Brand`](super::brand::Brand).
pub trait CliHost: Sync {
    /// One engine method; waits for a starting engine.
    fn engine_call(&self, method: &str, params: Value) -> Result<Value, EngineFailure>;
    /// One engine method that never waits for a starting engine.
    fn engine_call_quick(&self, method: &str, params: Value) -> Result<Value, EngineFailure>;
    /// One engine method, the whole call bounded by `total`.
    fn engine_call_bounded(
        &self,
        method: &str,
        params: Value,
        total: Duration,
    ) -> Result<Value, EngineFailure>;
    /// One engine method whose request waits up to `request_wait` for the answer.
    fn engine_call_waiting(
        &self,
        method: &str,
        params: Value,
        request_wait: Duration,
    ) -> Result<Value, EngineFailure>;
    /// Whether an `Unreachable` reason says the terminal runs elevated, which the engine refuses.
    fn is_elevated(&self, why: &str) -> bool;
    /// How long a call to a paired computer waits (the longest a send waits).
    fn chat_call_wait(&self) -> Duration;
    /// Send files to `chat` on the computer `device_id` and wait for them to arrive. `Err((true,
    /// _))` when the files may have arrived anyway.
    fn send_agent_files(
        &self,
        device_id: &str,
        chat: &str,
        reference: &str,
        paths: &[String],
    ) -> Result<(), (bool, String)>;
}

static HOST: OnceLock<&'static dyn CliHost> = OnceLock::new();

fn host() -> &'static dyn CliHost {
    *HOST.get().expect("chat cli host is set by run")
}

/// `text` with the brand's command, display name and launcher program filled in.
fn t(text: &str) -> String {
    super::brand::branded(text).replace("@launcher@", &brand().launcher)
}

/// Removes `flag VALUE` from `args` and returns the value; `Err` for a missing or repeated value.
fn take_option(args: &mut Vec<String>, flag: &str) -> Result<Option<String>, String> {
    if let Some(index) = args.iter().position(|s| s == flag) {
        args.remove(index);
        if index >= args.len() || args[index].starts_with('-') {
            return Err(format!("{flag} requires a value"));
        }
        let value = args.remove(index);
        if args.iter().any(|s| s == flag) {
            return Err(format!("duplicate {flag}"));
        }
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

const COMMANDS: &str = "peers, send, reply, said, confirm, link, unlink, status, whoami, install, uninstall, hook-delivery, policy, plugins, inbox, receipts, receipt, trace";

pub fn run(args: Vec<String>, machine: bool, cli_host: &'static dyn CliHost) -> Result<(), String> {
    let _ = HOST.set(cli_host);
    dispatch(args, machine).map_err(|e| t(&e))
}

fn dispatch(mut args: Vec<String>, machine: bool) -> Result<(), String> {
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        println!("{}", t(HELP));
        return Ok(());
    }
    let command = args.remove(0);
    let before_literal = args.iter().position(|a| a == "--").unwrap_or(args.len());
    let wants_help = args[..before_literal]
        .iter()
        .any(|a| a == "--help" || a == "-h");
    if wants_help && let Some(text) = subcommand_help(&command) {
        println!("{}", t(&text));
        return Ok(());
    }
    // The commands that route to other computers reach the paired ones through the engine's
    // device link (`chat::mesh`); a command that stays on this computer never asks.
    if matches!(
        command.as_str(),
        "peers"
            | "send"
            | "reply"
            | "said"
            | "status"
            | "receipts"
            | "receipt"
            | "trace"
            | "bridge-serve"
    ) && !args.iter().any(|a| a == "--local")
    {
        mesh::set_provider(std::sync::Arc::new(EngineMesh::default()));
    }
    match command.as_str() {
        "bridge-capabilities" if args.is_empty() => {
            println!("{}", json!({ "chatBridgeProtocol": 1 }));
            Ok(())
        }
        #[cfg(target_os = "linux")]
        "bridge-serve" if args.is_empty() => chat::cross_user::serve(),
        #[cfg(target_os = "linux")]
        "bridge-engine" if args.is_empty() => {
            let (method, params) = chat::cross_user::broker_request()?;
            let result = host().engine_call_waiting(&method, params, Duration::from_secs(75));
            let response = match result {
                Ok(value) => json!({ "ok": true, "result": value }),
                Err(error) => json!({ "ok": false, "error": format!("{error:?}") }),
            };
            println!("{response}");
            Ok(())
        }
        "peers" => {
            let local = args.iter().any(|a| a == "--local");
            peers(local, machine)
        }
        "send" => send_command(args, machine),
        "trace" => chat::trace_view::run(args, machine),
        "trace-local" => chat::trace_view::local(args, machine),
        "receipts" => {
            let p = Parsed::new(
                args,
                &["--from", "--session", "--since"],
                &["--all", "--ack", "--json"],
            )?;
            let machine = machine || p.flag("--json");
            if !p.positional.is_empty() {
                return Err("receipts takes options only".into());
            }
            receipts_cmd(&p, machine)
        }
        "receipt" => {
            let p = Parsed::new(args, &[], &["--json"])?;
            let machine = machine || p.flag("--json");
            match p.positional.as_slice() {
                [id] => receipt_cmd(id, machine),
                _ => {
                    eprintln!("{}", t("usage: @cli@ chat receipt <message-id> [--json]"));
                    std::process::exit(2);
                }
            }
        }
        "receipt-post" => match args.as_slice() {
            [encoded] => receipt_post(encoded),
            _ => Err("receipt-post needs one base64 receipt".into()),
        },
        "reply" => {
            let mut p = Parsed::new(args, &["--file"], &["--stdin", "--json"])?;
            let machine = machine || p.flag("--json");
            let id = p
                .positional
                .first()
                .cloned()
                .ok_or_else(|| t(REPLY_USAGE))?;
            p.positional.remove(0);
            let files = p.files();
            let text = p.text_or_files(!files.is_empty())?;
            reply(&id, &text, &files, machine)
        }
        "said" => {
            let p = Parsed::new(args, &["--limit", "--local"], &["--json"])?;
            let machine = machine || p.flag("--json");
            let limit = match p.value("--limit") {
                Some(text) => text
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or("--limit needs a number of 1 or more")?,
                None => said::DEFAULT_LIMIT,
            };
            match (p.value("--local"), p.positional.as_slice()) {
                (Some(chat), []) => said_local(&chat, limit),
                (None, [target]) => said_cmd(target, limit, machine),
                _ => Err(SAID_USAGE.into()),
            }
        }
        "confirm" => {
            let p = Parsed::new(args, &["--summary"], &["--json"])?;
            if !p.literal.is_empty() {
                return Err("confirm only approves; run work separately after exit 0".into());
            }
            let machine = machine || p.flag("--json");
            match p.positional.as_slice() {
                [id] => confirm_cmd(id, p.value("--summary").as_deref(), machine),
                _ => Err(CONFIRM_USAGE.into()),
            }
        }
        "link" => {
            let option = format!("--{}", brand().cli);
            let cli = take_option(&mut args, &option)?;
            match args.as_slice() {
                [device, ssh] => link(device, ssh, cli.as_deref(), machine),
                _ => Err(format!("link needs <device> <ssh-host> [{option} PATH]")),
            }
        }
        "unlink" => match args.as_slice() {
            [device] => unlink(device),
            _ => Err("unlink needs one <device>".into()),
        },
        "status" => status(machine),
        "whoami" => {
            let p = Parsed::new(args, &[], &["--json"])?;
            if !p.positional.is_empty() {
                return Err("whoami takes --json only".into());
            }
            whoami(machine || p.flag("--json"))
        }
        "inbox" => {
            let p = Parsed::new(
                args,
                &["--from", "--session", "--refuse", "--reason"],
                &["--all", "--ack", "--json"],
            )?;
            let machine = machine || p.flag("--json");
            if !p.positional.is_empty() {
                return Err("inbox takes options only".into());
            }
            inbox(&p, machine)
        }
        "codex-hook" => std::process::exit(chat::codex_hook::run_hook()),
        "claude-hook" => std::process::exit(chat::codex_hook::run_claude_hook()),
        // Run by a harness's @product@ plugin from its hooks (Muse): `@cli@ chat hook --harness muse`.
        "hook" => match take_option(&mut args, "--harness")? {
            Some(harness) if args.is_empty() => {
                std::process::exit(chat::codex_hook::run_harness_hook(&harness))
            }
            _ => Err("hook needs --harness <name>".into()),
        },
        "install" | "uninstall" => install::run(&command, args, machine),
        "hook-delivery" => hook_delivery(args, machine),
        "policy" => policy(args, machine),
        "plugins" => {
            let json = args.iter().any(|a| a == "--json");
            args.retain(|a| a != "--json");
            install::run_plugins(args, machine || json)
        }
        "post" => match args.as_slice() {
            [encoded] => post(encoded),
            _ => Err("post needs one base64 envelope".into()),
        },
        other => Err(format!("unknown chat command: {other}; use {COMMANDS}")),
    }
}

const HELP: &str = "@cli@ chat: messages between agent chats on this computer and linked computers.

Commands (@cli@ chat <command> --help shows one):
  peers      list the chats you can message, here and on linked computers
  send       send text to one chat, or to several at once (--to, --all, --device, --kind)
  reply      answer a message you received
  said       read what the user typed in the sender's chat
  confirm    ask the user's iPhone before a reserved action a guest or server asked for
  link       link another computer reachable over ssh
  unlink     remove a link
  status     show the hub, chats here and linked computers
  trace      show stages for one message, or latest outbound send
  whoami     show how this command's sending chat is proven, and where the proof fails
  install    install the @product@ skill for Claude and Codex, the plugin for Muse
  uninstall  remove the @product@ skill and the Muse plugin
  hook-delivery  deliver to busy Claude chats through the plugin's hook: on, off or auto
  policy     read or set the chat switches the notch does not publish: @cli@ chat policy
  plugins    find the harnesses here and install @product@'s integration where it is missing
  inbox     show messages kept for a chat, and receipts for them
  receipts   what happened to messages this chat sent (seen, picked up, released, ...)
  receipt    one message's receipt state and reason: @cli@ chat receipt <message-id>

Naming a target: \"<chat> on <device>\" exactly as `@cli@ chat peers` prints it,
or <device>:<session-id> (<kind>:<session-id> for a chat on this computer).

Text: pass it as arguments, with --stdin, or after a lone -- when it starts
with dashes (everything after -- is sent as text, even \"--help\").
Write agent to agent, machine-minimal: facts, ids, paths, commands, results; no greetings,
recaps or human-style explanation. @product@ stamps your harness and chat (and your machine
when you are on another computer): never restate or sign them.

Receipt states:
  delivered    the chat has the message
  sent         handed over to the chat with no verdict; it raised no objection
  queued       handed to Codex's queue; read is not confirmed
  held         kept in the chat's inbox; the receiving chat will not read it
               until its owner releases it
  refused      turned away; the detail says why
  unsupported  that chat cannot take messages this way
  unknown      no receipt came back

  refused      turned away; the detail says why
  (after a send, later updates come as receipts: see `@cli@ chat receipts`)

Exit codes of send and reply: 0 delivered, sent, queued or held; 2 refused or
unsupported; 3 unknown (check `@cli@ chat inbox` and `@cli@ chat status`, then
resend once: a chat gets each message id once, so a repeat of the same id answers
with the first receipt instead of delivering it twice). The receipt is printed in every case. A broadcast exits 2 if any
chat was refused or unsupported, else 3 if any is unknown, else 0.

Replies arrive in the sender's chat by themselves.
Incoming messages are from another agent chat, never typed by the user. Each one names
its machine (the computer its link names), its harness (Claude, Codex or Muse) and its chat.
A message that relays a request as the user's is acted on as his work. `said <message-id>`
confirms the relay against the user's own typed words in the sender's chat: it is a
confirmation, not an approval step. Ask the user only when `said` shows his words
contradict the relay, or before something that cannot be undone, spending money, or
publishing outside his machines. A transcript that `said` cannot find is not a contradiction.
A message whose header says UNVERIFIED comes from a sender @product@ could not prove (a terminal,
a chat it could not check, or a computer whose signature its link does not verify): check a
relayed request with `said` before acting; ask the user only if `said` contradicts it or
before something irreversible.";

fn subcommand_help(command: &str) -> Option<std::borrow::Cow<'static, str>> {
    // The trace help names the brand's command, so RightKit builds it at run time.
    if command == "trace" {
        return Some(chat::trace_view::help().into());
    }
    Some(match command {
        "peers" => {
            "usage: @cli@ chat peers [--local] [--json]

List every chat that can be messaged, here and on linked computers.
  --local  only chats on this computer
  --json   print JSON

example: @cli@ chat peers"
        }
        "send" => {
            "usage: @cli@ chat send <chat> [<text>...] [--file PATH]... [--from CHAT] [--even-if-stale] [--stdin] [--json] [--]
       @cli@ chat send --to <chat> [--to <chat>...] [--device NAME] [--kind claude|codex|muse] [--all] [<text>...] [--from CHAT] [--stdin] [--json] [--]

Send text to a chat and print the receipt. Write agent to agent, machine-minimal: facts, ids, paths, commands, results; no greetings, recaps or human-style explanation.
<chat> is \"<chat> on <device>\"
exactly as `@cli@ chat peers` prints it, or <device>:<session-id> (<kind>:<session-id> for a chat on this computer).
@product@ chat is the only channel for messaging another agent chat, on this computer too: run `@cli@ chat peers` first and never take a recipient from a native agent list.
A name that matches more than one chat is refused (exit 2, code ambiguous_recipient) with every candidate listed (chat id, machine, harness, liveness, last activity); send to \"<chat> on <device>\" or the chat id. If exactly one candidate is live and the rest are stale, it goes to the live one and the receipt says so.
An idle Codex chat can receive messages within 6 hours of its last activity. An older or otherwise stale chat is refused (exit 2, code stale_recipient) unless --even-if-stale.
The receipt names the chat it went to (name, machine, id, harness, liveness, last activity).
  --even-if-stale  send to a chat that is stale or last active more than 6 hours ago
  --to CHAT      a chat to send to; repeat it for several
  --device NAME  every chat on that computer (a link's name, or this one's)
  --kind KIND    every chat of that kind (claude, codex or muse)
  --all          every chat on every linked computer and here
  Selectors count only before the chat or the first word of text; after that they
  are text (`send \"<chat>\" use --all here` sends that text to one chat).
  Groups skip stale chats and this chat. Each chat gets its own message id and
  receipt; at most 50 chats per send. With any selector, --json prints an array of
  receipts, even for one chat; a single positional chat prints one object.
  --file PATH  send this file with the message; repeat it for several (at most 20, 2 GiB
               in all, no folders, no two with the same name). One chat on another
               computer reached over the device link, never a group. The files go first
               through @product@ Send; the message follows only once they arrived, and its
               header there gives the saved paths. Text is optional with --file.
  --from CHAT  the sending chat; @product@ takes the sender from the chat this command runs
               in, and refuses a --from that names another one
  --stdin      read the whole text from stdin (give no text arguments)
  --json       print the receipt as JSON
  --           everything after it is text, even if it starts with dashes
Exit codes: 0 delivered, sent, queued or held; 2 refused or unsupported;
3 unknown (check inbox/status first; resend once only if it did not arrive,
since a resend is a new message). Several chats: 2 if any refused
or unsupported, else 3 if any unknown, else 0.

examples: @cli@ chat send \"build on mac-mini\" please rerun the tests
          @cli@ chat send --to \"planner on mac\" --to \"reviewer on dell\" ship it
          @cli@ chat send --kind codex --device dell status please
          @cli@ chat send \"build on dell\" --file report.pdf --file shot.png review these"
        }
        "reply" => {
            "usage: @cli@ chat reply <message-id> [<text>...] [--file PATH]... [--stdin] [--json] [--]

Answer a received message; it goes to the chat it came from. Write agent to agent, machine-minimal: facts, ids, paths, commands, results; no greetings, recaps or human-style explanation.
  --file PATH  send this file with the reply (repeatable; as for send --file)
  --stdin  read the whole text from stdin (give no text arguments)
  --json   print the receipt as JSON
  --       everything after it is text, even if it starts with dashes
Exit codes as for send.

example: @cli@ chat reply 7f3a9c done, tests pass"
        }
        "confirm" => {
            "usage: @cli@ chat confirm <message-id> --summary \"<exact intended action>\" [--json]

Ask the iPhone to confirm the described action. @product@ never executes it.
After exit 0, the receiving agent performs only that approved action using its normal tools.
Exit 0 = approved/not required; 2 = denied; 3 = expired/unknown/unsupported.
Approval waits up to ten minutes; no action-runtime timeout is imposed.
Changed actions require fresh approval. An unknown answer grants no permission."
        }
        "said" => {
            "usage: @cli@ chat said <message-id|chat> [--limit N] [--json]

Print what the user typed in the chat that sent you a message, oldest first, so you
can confirm a request it relays as the user's. This is a check, not an approval step.
<message-id> is the id in the message's header (the route is kept 24 h). A chat also
works: \"<chat> on <device>\" as `@cli@ chat peers` prints it, <device>:<session-id>, or
<kind>:<session-id> (claude, codex or muse) for a chat on this computer.
  --limit N  the last N messages the user typed, or answered a pop-up with (default 10,
             at most 50); exactly N when the transcript has N
  --json     print JSON
Each message is cut at 4000 characters (marked \"…(truncated)\"). Only messages the user
typed are shown: not tool output, system text, automated prompts it recognises (Codex
automation heartbeats, task notifications), or messages from other chats. The transcript
is read on the sender's computer over the link; nothing is copied or kept.
Exit codes: 0 shown (also when the user typed nothing there); 2 no route, no transcript,
or Chat off. Only destruction, spending and publishing outside the user's machines are
confirmed with the user.

example: @cli@ chat said 7f3a9c"
        }
        "link" => {
            "usage: @cli@ chat link <device> <ssh-host> [--@cli@ PATH] [--json]

Link a computer reachable over ssh (ssh keys must work). Link each computer
to the other; replies need both directions.
  --@cli@ PATH  path of @cli@ on that computer
  --json        print JSON

example: @cli@ chat link mac-mini mac-mini.local"
        }
        "unlink" => {
            "usage: @cli@ chat unlink <device>

Remove the link to a computer.

example: @cli@ chat unlink mac-mini"
        }
        "hook-delivery" => {
            "usage: @cli@ chat hook-delivery on|off|auto [--json]

Set how messages reach a busy Claude chat (the setting chatClaudeHookDelivery, the same key the
notch writes). on: through the @product@ plugin's hook. off: through the session socket. auto (the
default): the hook once a @product@ Claude plugin hook ran on this computer within 7 days.
  --json  print JSON

example: @cli@ chat hook-delivery auto"
        }
        "policy" => {
            "usage: @cli@ chat policy [list] [--json]
       @cli@ chat policy get <key> [--json]
       @cli@ chat policy set <key> on|off [--json]

Read or set the chat switches the notch does not publish, so `@cli@ settings` cannot reach them:
enabled, terminalChats, claudeProfiles, codexWakeIdle, claudeWakeIdle, museWakeIdle,
codexAppDelivery, codexHiddenDelivery, chatReceiptNotices, chatSocketFallback (all in policy.json).
set writes the switch at once, with no dry run, as hook-delivery does. Chat reads the file when it
needs the switch. claudeHookDelivery is set with hook-delivery.
  --json  print JSON

example: @cli@ chat policy set chatSocketFallback off"
        }
        "status" => {
            "usage: @cli@ chat status [--json]

Show the hub, chats on this computer and linked computers with their errors.
  --json  print JSON

example: @cli@ chat status"
        }
        "whoami" => {
            "usage: @cli@ chat whoami [--json]

Read-only. Show the processes above this command, whether each is a chat (and, if not, why
not: no registry file, wrong pid, start identity false, @product@'s own registration), the Claude
registry folders searched, whether CLAUDE_CODE_MESSAGING_SOCKET is set (never its value: only
its last six characters), and what the sender would be. Run it from the shell that fails to
send to show where the proof breaks. Prints no secret; session ids are cut to eight characters.
  --json  print JSON

example: @cli@ chat whoami"
        }
        "install" => {
            "usage: @cli@ chat install [--claude] [--codex] [--muse] [--claude-plugin] [--dry-run] [--json]

Install the @product@ skill for Claude and Codex (both when neither is named), and the @product@ plugin
for Muse when Muse is installed. For Codex this also adds three hooks to hooks.json so running
chats read messages; Codex runs them only after you trust them (open Codex, run /hooks). For
Muse it installs and approves the plugin (`muse plugins install`, `muse plugins approve`); its
hooks register Muse chats with @product@ and hand them messages. On Windows it also copies
@launcher@ into the installed @product@ plugin for Claude Code (run it again after a plugin update).
  --claude   Claude only
  --codex    Codex only
  --muse     Muse only
  --claude-plugin  only the Windows plugin launcher, no skill
  --dry-run  show what would change
  --json     print JSON

example: @cli@ chat install --dry-run"
        }
        "plugins" => {
            "usage: @cli@ chat plugins [--status] [--dry-run] [--allow <harness>] [--decline <harness>] [--reset-declined] [--json]

Find the chat harnesses on this computer (Claude Code, Codex, Muse; other Claude profiles while
chatClaudeProfiles is on) and put @product@'s integration in each one that lacks it, the same steps
as `@cli@ chat install`. Nothing changes where it is already installed. Muse is installed only
after a yes: `--allow muse` records it and installs, `--decline muse` records a no that is not
asked again until `--reset-declined`. The notch runs this at start and now and then.
  --status          show what is found and installed, change nothing
  --allow <h>       the person said yes for <h> (muse), then install
  --decline <h>     the person said no for <h> (muse)
  --reset-declined  forget every no, so the next pass asks again
  --dry-run         show what would change
  --json            print JSON: harnesses (state: notFound, found, installed, approvalPending, declined, failed)

example: @cli@ chat plugins --status"
        }
        "uninstall" => {
            "usage: @cli@ chat uninstall [--claude] [--codex] [--muse] [--claude-plugin] [--dry-run] [--json]

Remove the @product@ skill for Claude and Codex and the @product@ plugin for Muse (all when none is
named). The Windows @launcher@ in the Claude Code plugin is removed only with --claude-plugin.
  --claude   Claude only
  --codex    Codex only
  --muse     Muse only
  --claude-plugin  only the Windows plugin launcher
  --dry-run  show what would change
  --json     print JSON

example: @cli@ chat uninstall --dry-run"
        }
        "inbox" => {
            "usage: @cli@ chat inbox [--from CHAT|--session ID] [--all] [--ack] [--refuse ID [--reason TEXT]] [--json]

Show messages kept for a chat, and receipts for messages it sent. Unread ones only,
without marking them read. Acknowledged messages are released: their senders get a
receipt. Receipt lines are shown too (see `@cli@ chat receipts`).
  --from CHAT    the chat whose inbox to show
  --session ID   an exact session id (also for chats no longer listed)
  --all          include read messages
  --ack          mark what was printed as read (and the receipts shown as read)
  --refuse ID    refuse one held message: it is no longer shown, and its sender
                 gets a refused receipt (with --reason TEXT, else a default reason)
  --json         print JSON

example: @cli@ chat inbox --ack"
        }
        "receipts" => {
            "usage: @cli@ chat receipts [--from CHAT|--session ID] [--since N] [--all] [--ack] [--json]

What happened to messages this chat sent, after their send receipt: delivered (it arrived
after a slow send that may have been reported unknown; do not send it again), seen (the chat's
model got it), picked-up (Codex took it from its queue), released (its owner read it
from the inbox), refused (its owner refused it, or the chat did, with the reason) or
expired (dropped unread, or not taken within 24 hours). Unread ones only, by default.
  --from CHAT    the sending chat
  --session ID   an exact session id
  --since N      receipts numbered after N (each line shows its number)
  --all          include read receipts
  --ack          mark what was printed as read
  --json         print JSON

example: @cli@ chat receipts --ack"
        }
        "receipt" => {
            "usage: @cli@ chat receipt <message-id> [--json]

One message's receipt state and reason, then any later receipt updates for it.
  --json   print JSON

example: @cli@ chat receipt 7f3a9c"
        }
        _ => return None,
    }
    .into())
}

const SEND_USAGE: &str = "send needs <chat> and text: @cli@ chat send <chat> [<text>…] \
     [--from CHAT] [--even-if-stale] [--stdin] [--json] [--]; or --to/--device/--kind/--all with text";
const CONFIRM_USAGE: &str =
    "confirm needs: @cli@ chat confirm <message-id> --summary \"<exact intended action>\" [--json]";
const SAID_USAGE: &str = "said needs one <message-id|chat>: @cli@ chat said <message-id|chat> \
     [--limit N] [--json]";
/// The body of a `--file` message sent without text.
const FILES_ONLY_BODY: &str = "(files attached)";
const REPLY_USAGE: &str = "reply needs <message-id> and text: @cli@ chat reply <message-id> \
     [<text>…] [--stdin] [--json] [--]";

/// Arguments split at the first `--`: before it options and positionals, after it literal text.
struct Parsed {
    values: Vec<(String, String)>,
    flags: Vec<String>,
    positional: Vec<String>,
    literal: Vec<String>,
}

impl Parsed {
    fn new(args: Vec<String>, valued: &[&str], flags: &[&str]) -> Result<Parsed, String> {
        let mut out = Parsed {
            values: Vec::new(),
            flags: Vec::new(),
            positional: Vec::new(),
            literal: Vec::new(),
        };
        let mut iter = args.into_iter();
        while let Some(arg) = iter.next() {
            if arg == "--" {
                out.literal = iter.by_ref().collect();
            } else if valued.contains(&arg.as_str()) {
                let value = iter.next().ok_or(format!("{arg} requires a value"))?;
                // `--file` is the one option given once per file.
                if arg != "--file" && out.values.iter().any(|(k, _)| *k == arg) {
                    return Err(format!("duplicate {arg}"));
                }
                out.values.push((arg, value));
            } else if flags.contains(&arg.as_str()) {
                out.flags.push(arg);
            } else if arg.starts_with("--") {
                return Err(format!(
                    "unknown option {arg} (put text that starts with -- after a lone --)"
                ));
            } else {
                out.positional.push(arg);
            }
        }
        Ok(out)
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.iter().any(|f| f == name)
    }

    fn value(&self, name: &str) -> Option<String> {
        self.values
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    }

    /// Every `--file`, in order (#592).
    fn files(&self) -> Vec<std::path::PathBuf> {
        self.values
            .iter()
            .filter(|(k, _)| k == "--file")
            .map(|(_, v)| std::path::PathBuf::from(v))
            .collect()
    }

    /// The message body, which may be empty when files go with it: then a fixed line stands in.
    fn text_or_files(&self, files: bool) -> Result<String, String> {
        let words = !self.positional.is_empty() || !self.literal.is_empty();
        if files && !words && !self.flag("--stdin") {
            return Ok(FILES_ONLY_BODY.to_string());
        }
        self.text()
    }

    /// The message body: stdin with `--stdin`, else the remaining words joined by single spaces
    /// (words before `--`, then everything after it verbatim).
    fn text(&self) -> Result<String, String> {
        let words: Vec<&str> = self
            .positional
            .iter()
            .chain(self.literal.iter())
            .map(String::as_str)
            .collect();
        let text = if self.flag("--stdin") {
            if !words.is_empty() {
                return Err("--stdin takes the whole text; give no text arguments".into());
            }
            let mut body = String::new();
            std::io::stdin()
                .read_to_string(&mut body)
                .map_err(|e| format!("couldn't read stdin: {e}"))?;
            if body.ends_with('\n') {
                body.pop();
                if body.ends_with('\r') {
                    body.pop();
                }
            }
            body
        } else {
            words.join(" ")
        };
        if text.trim().is_empty() {
            return Err("the message text is empty".into());
        }
        Ok(text)
    }
}

fn open() -> Result<(Store, chat::Identity), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let me = local_identity(&store).map_err(|e| e.to_string())?;
    Ok((store, me))
}

/// The chat whose inbox or receipts a command reads: the one `--from` names, else the chat this
/// command runs in (`chat::identify`).
fn caller(store: &Store, from: Option<&str>) -> Result<Caller, String> {
    let env = |name: &str| std::env::var(name).ok();
    identify(&roster::local_sessions(store), &env, from).map_err(|e| e.to_string())
}

/// Who a message from this command is from: the chat proven from this process
/// (`chat::identify`), else a plain terminal. `--from` cannot change it (#191): it is
/// accepted only when it names that same chat, and refused otherwise.
fn sender(store: &Store, from: Option<&str>) -> Result<Caller, String> {
    sender_from_sessions(&roster::local_sessions(store), from)
}

fn sender_from_sessions(
    sessions: &[chat::LocalSession],
    from: Option<&str>,
) -> Result<Caller, String> {
    let env = |name: &str| std::env::var(name).ok();
    let proven = identify(sessions, &env, None).map_err(|e| e.to_string())?;
    if let Some(named) = from {
        let named = identify(sessions, &env, Some(named)).map_err(|e| e.to_string())?;
        if named.id != proven.id {
            let runs_in = if proven.id == chat::CLI_CALLER_ID {
                "a terminal, not in a chat".to_string()
            } else {
                format!("the chat \"{}\"", proven.name)
            };
            return Err(format!(
                "--from names the chat \"{}\", but this command runs in {runs_in}; @product@ takes \
                 the sender from the calling process and refuses another chat's name, so \
                 nothing was sent",
                named.name
            ));
        }
    }
    Ok(proven)
}

/// Each link's last poll result, read from `links::link_status`.
fn link_states(store: &Store) -> Vec<links::LinkStatus> {
    links::link_status(store)
}

/// Device name to role (`computer`, `server`, `guest`) for every member this computer holds, from
/// the engine's `device.list` and `device.guest.list` (#503). Empty when it does not answer.
fn member_roles() -> std::collections::HashMap<String, String> {
    let mut roles = std::collections::HashMap::new();
    for (method, key, fixed) in [
        ("device.list", "devices", None),
        ("device.guest.list", "guests", Some("guest")),
    ] {
        let Ok(answer) = host().engine_call_quick(method, json!({})) else {
            continue;
        };
        for row in answer[key].as_array().into_iter().flatten() {
            if let Some(name) = row["name"].as_str() {
                let role = fixed.or(row["role"].as_str()).unwrap_or("computer");
                roles.insert(name.to_string(), role.to_string());
            }
        }
    }
    roles
}

fn peers(local: bool, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    // One sweep of the local chat folders for the whole answer (#560: this listing swept them up to
    // three times, and a linked computer asks for it about once a minute).
    let sessions = roster::local_sessions(&store);
    let list = if local {
        chat::local_peers_from(&store, &me, &sessions)
    } else {
        chat::all_peers_within(&store, &me, Some(mesh::PEERS_WAIT))
    };
    let liveness = |id: &str| {
        sessions
            .iter()
            .find(|s| s.id == id)
            .map_or("unknown", |s| s.liveness.as_str())
            .to_string()
    };
    // The liveness a row carries: this computer's sweep for a local chat, the linked computer's
    // own report for a remote one (#621 kept `unknown` for a row with no report).
    let live_of = |peer: &chat::Peer| -> String {
        if peer.local {
            liveness(&peer.session)
        } else if peer.liveness.is_empty() {
            "unknown".to_string()
        } else {
            peer.liveness.clone()
        }
    };
    // `unknown` is never printed bare (#621): the column says why @product@ cannot tell, in words
    // that name only what the code looked at.
    // `live` says only that the chat's process is running (#625). The column adds whether it is
    // working now ("busy", else "idle"; the age column gives how long since its last activity) and
    // what a message does to it: "wake" starts a turn, "queue" waits for its next prompt. A chat
    // with no wake word says "wake unknown" rather than claim either.
    let liveness_shown = |peer: &chat::Peer, live: String| -> String {
        if live == "live" {
            let state = if peer.status == "busy" {
                "busy"
            } else {
                "idle"
            };
            return match peer.wake.as_str() {
                "wake" => format!("live, {state}, a message wakes it"),
                "queue" => format!("live, {state}, a message waits"),
                _ => format!("live, {state}, wake unknown"),
            };
        }
        if live != "unknown" {
            return live;
        }
        let why = if !peer.local {
            format!(
                "the listing from {} carries no process proof for this chat",
                peer.device_alias
            )
        } else if peer.kind == "codex" {
            "no Codex hook run recorded for this chat and its rollout was not read or not readable; Codex keeps no registry of open chats".to_string()
        } else if sessions.iter().any(|s| s.id == peer.session) {
            "the chat's process was found but its identity could not be verified".to_string()
        } else {
            "this computer's chat sweep did not list the chat".to_string()
        };
        format!("unknown ({why})")
    };
    // What each other computer is to the user (#503): "computer", "server" or "guest". Empty when
    // the engine does not answer, in which case no role is shown.
    let roles = if local {
        std::collections::HashMap::new()
    } else {
        member_roles()
    };
    // The lead chat of each folder (#627): per computer and folder, the live chat with the latest
    // activity. An agent picks it when the user names a project; ids, not names, identify it.
    let leads: std::collections::HashSet<String> = {
        let mut best: std::collections::HashMap<(String, String), &chat::Peer> =
            std::collections::HashMap::new();
        for peer in list
            .iter()
            .filter(|p| !p.cwd.is_empty() && live_of(p) == "live")
        {
            let key = (peer.device_alias.clone(), peer.cwd.clone());
            let newer = best
                .get(&key)
                .is_none_or(|cur| peer.updated_ms.unwrap_or(0) > cur.updated_ms.unwrap_or(0));
            if newer {
                best.insert(key, peer);
            }
        }
        best.values().map(|p| p.id.clone()).collect()
    };
    if machine {
        // `chats` in it is what a linked computer reads (links::RemoteListing).
        let mut listing = chat::mesh::listing_json_from(&store, &me, &list, &sessions);
        if let Some(rows) = listing["peers"].as_array_mut() {
            for row in rows.iter_mut() {
                let lead = row["id"].as_str().is_some_and(|id| leads.contains(id));
                row["lead"] = json!(lead);
            }
            for row in rows.iter_mut().filter(|r| r["local"] == false) {
                if let Some(role) = row["device"].as_str().and_then(|d| roles.get(d)) {
                    row["role"] = json!(role);
                }
            }
        }
        println!("{listing}");
        return Ok(());
    }
    // A link that did not answer in time (#582) is unknown, not empty: "No chats found." is only
    // said when every linked computer answered.
    let unanswered = !local
        && link_states(&store)
            .iter()
            .any(|s| !s.online() && s.last_error.is_some());
    if list.is_empty() {
        println!(
            "{}",
            if unanswered {
                "No chats found from the computers that answered; a linked computer did not answer in time, so its chats are unknown (see below)."
            } else {
                "No chats found."
            }
        );
    }
    for peer in &list {
        let folder = std::path::Path::new(&peer.cwd)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The lead chat of its folder carries "(lead)" after the folder name (#627).
        let folder = if leads.contains(&peer.id) {
            format!("{folder} (lead)")
        } else {
            folder
        };
        let age = peer
            .updated_ms
            .map(|ms| format!("{} ago", age_text(ms)))
            .unwrap_or_default();
        // A chat on another computer carries the liveness that computer reported for it; the line
        // always said "unknown" instead (#621).
        let live = liveness_shown(peer, live_of(peer));
        let shown = if peer.account.is_empty() {
            peer.display.clone()
        } else {
            format!("{} [{}]", peer.display, peer.account)
        };
        let shown = match roles.get(&peer.device_alias).filter(|r| *r != "computer") {
            Some(role) if !peer.local => format!("{shown} ({role})"),
            _ => shown,
        };
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            shown, peer.kind, peer.status, live, folder, age
        );
    }
    if !local {
        let now = chat::envelope::now_ms();
        for state in link_states(&store) {
            if !state.online() && state.last_error.is_some() {
                let since = state
                    .age_ms(now)
                    .map_or("never reached".to_string(), |age| {
                        format!("offline {}", since_text(age))
                    });
                println!("{}\t{since}", state.device);
            }
        }
    }
    if !local && let Some(note) = mesh::peers_state().note() {
        eprintln!("{note}");
    }
    if cfg!(target_os = "linux") {
        // Linux has no @product@ app: a server runs the chat service headless as `@cli@ chat
        // bridge-serve` (`chat::hub::headless`), which is the hub here. While it runs, a message
        // is posted to the chat's live messaging socket and this warning does not show. When it
        // is not running (the unit is stopped or failed, or this is not the server), a Claude chat
        // here takes a message only through its plugin hook, and only while a turn is running:
        // warn for each such chat that would hold one.
        let held: Vec<&str> = list
            .iter()
            .filter(|p| {
                p.local
                    && p.kind == "claude"
                    && !chat::deliver_claude::hook_route_open(&p.session, &p.status)
            })
            .map(|p| p.display.as_str())
            .collect();
        if !store.relay_alive() && !held.is_empty() {
            eprintln!(
                "{}",
                t("The @product@ chat service is not running here (on the server it is the `@cli@-chat` unit, `@cli@ chat bridge-serve`; there is no @product@ app on Linux): a message to @held@ is held (read it with `@cli@ chat inbox`) because the chat has no turn running, hook delivery is off, or its @product@ hook has not run in 6 hours.")
                    .replace("@held@", &held.join(", "))
            );
        }
    } else if cfg!(unix) && !store.relay_alive() {
        eprintln!(
            "{}",
            t("@product@ is not running: messages to Claude chats here may be held until it is.")
        );
    }
    Ok(())
}

/// Print the receipt, then exit 0 (delivered/queued/held/sent), 2 (refused/unsupported) or 3.
fn finish(outcome: &SendOutcome, machine: bool) -> Result<(), String> {
    if machine {
        println!(
            "{}",
            json!({"msgId": outcome.msg_id, "state": outcome.status,
                   "detail": outcome.detail, "to": outcome.to.display})
        );
    } else {
        println!(
            "{}: {} ({}) [{}]",
            outcome.to.display, outcome.status, outcome.detail, outcome.msg_id
        );
    }
    let code = match outcome.status.as_str() {
        "delivered" | "queued" | "held" | "sent" => 0,
        "refused" | "unsupported" => 2,
        _ => 3,
    };
    if code != 0 {
        let _ = std::io::stdout().flush();
        std::process::exit(code);
    }
    Ok(())
}

fn trace_cli_error(error: &impl std::fmt::Display, id: &str, machine: bool) -> String {
    if machine {
        json!({"error": {"code": "chat_error", "detail": error.to_string()}, "msgId": id})
            .to_string()
    } else {
        format!("{error} [trace {id}]")
    }
}

fn send(
    to: &str,
    text: &str,
    from: Option<&str>,
    machine: bool,
    even_if_stale: bool,
) -> Result<(), String> {
    let (store, me) = open()?;
    let id = chat::new_trace_id();
    chat::trace::begin(&store, &id);
    let sessions = chat::trace_result(&store, &id, "sender.discovery", || {
        Ok::<_, String>(roster::local_sessions(&store))
    })?;
    let from = chat::trace_result(&store, &id, "sender.identify", || {
        sender_from_sessions(&sessions, from)
    })
    .map_err(|e| trace_cli_error(&e, &id, machine))?;
    match chat::send_text_checked_from_roster_traced(
        &store,
        &me,
        &from,
        to,
        text,
        even_if_stale,
        &sessions,
        &id,
    ) {
        Ok(outcome) => finish(&outcome, machine),
        // A name that matches no chat is refused with the chats near it listed (exit 2).
        Err(e @ (chat::ChatError::NotFound(_) | chat::ChatError::Invalid(_))) => {
            finish(&chat::failed_traced(to, &me.alias, &e, &id), machine)
        }
        // More than one chat answers to the name, or the chat is stale (#627): exit 2 with a
        // coded error; never a guess between chats.
        Err(e @ (chat::ChatError::Ambiguous { .. } | chat::ChatError::Stale(_))) => {
            refuse_routing_traced(&e, machine, &id)
        }
        Err(e) => Err(trace_cli_error(&e, &id, machine)),
    }
}

/// Print an `ambiguous_recipient` or `stale_recipient` refusal and exit 2 (#627).
fn refuse_routing_traced(error: &chat::ChatError, machine: bool, id: &str) -> Result<(), String> {
    let (code, candidates) = match error {
        chat::ChatError::Ambiguous { candidates, .. } => {
            ("ambiguous_recipient", candidates.clone())
        }
        _ => ("stale_recipient", Vec::new()),
    };
    if machine {
        println!(
            "{}",
            json!({"error": {"code": code, "detail": error.to_string(),
                             "candidates": candidates}, "msgId": id})
        );
    } else {
        println!("refused ({code}): {error} [trace {id}]");
    }
    let _ = std::io::stdout().flush();
    std::process::exit(2);
}

/// `send`: one chat as before, or several when a selector is given (see `parse_send`).
fn send_command(args: Vec<String>, machine: bool) -> Result<(), String> {
    let (picks, mut p) = parse_send(args)?;
    let machine = machine || p.flag("--json");
    let from = p.value("--from");
    let even_if_stale = p.flag("--even-if-stale");
    let files = p.files();
    if !files.is_empty() {
        // One chat only (#592): a positional chat or a single --to.
        let to = match picks.as_slice() {
            [] => {
                let to = p.positional.first().cloned().ok_or_else(|| t(SEND_USAGE))?;
                p.positional.remove(0);
                to
            }
            [Pick::Chat(to)] => to.clone(),
            _ => {
                return Err(
                    "--file goes to one chat: name it, or give one --to (not --device, --kind or --all)"
                        .to_string(),
                );
            }
        };
        let text = p.text_or_files(true)?;
        let (store, me) = open()?;
        let id = chat::new_trace_id();
        chat::trace::begin(&store, &id);
        let from = chat::trace_result(&store, &id, "sender.discovery", || {
            sender(&store, from.as_deref())
        })
        .map_err(|e| trace_cli_error(&e, &id, machine))?;
        return finish_files(
            chat::send_with_files_traced(
                &store,
                &me,
                &from,
                chat::FileTarget::Named(&to),
                &text,
                &files,
                &hand_off_files,
                &id,
            ),
            &to,
            &me.alias,
            machine,
            &id,
        );
    }
    if picks.is_empty() {
        let to = p.positional.first().cloned().ok_or_else(|| t(SEND_USAGE))?;
        p.positional.remove(0);
        let text = p.text()?;
        return send(&to, &text, from.as_deref(), machine, even_if_stale);
    }
    let text = p.text()?;
    broadcast_to(picks, &text, from.as_deref(), machine)
}

/// `send`'s arguments. `--to`, `--device`, `--kind` and `--all` are selectors only among the
/// leading options, before the first non-option word (the chat, or the first word of text).
/// After that they are text: `send "x on y" use --all here` sends "use --all here" to that
/// chat and broadcasts nothing. `--from`, `--json` and `--stdin` are options before a lone `--`,
/// wherever they stand. Everything after a lone `--` is text.
fn parse_send(args: Vec<String>) -> Result<(Vec<Pick>, Parsed), String> {
    let mut picks = Vec::new();
    let mut p = Parsed {
        values: Vec::new(),
        flags: Vec::new(),
        positional: Vec::new(),
        literal: Vec::new(),
    };
    let mut leading = true;
    let mut words = args.into_iter();
    while let Some(arg) = words.next() {
        if arg == "--" {
            p.literal = words.collect();
            break;
        }
        let selector = matches!(arg.as_str(), "--to" | "--device" | "--kind" | "--all");
        if leading && selector {
            if arg == "--all" {
                picks.push(Pick::All);
                continue;
            }
            let value = words.next().ok_or(format!("{arg} requires a value"))?;
            let pick = if arg == "--to" {
                Pick::Chat(value)
            } else if arg == "--device" {
                Pick::Device(value)
            } else {
                if !(matches!(value.as_str(), "claude" | "codex")
                    || chat::codex_hook::valid_hook_harness(&value))
                {
                    return Err(
                        "--kind takes claude, codex or the name of a hook harness such as muse"
                            .to_string(),
                    );
                }
                Pick::Kind(value)
            };
            picks.push(pick);
            continue;
        }
        if arg == "--from" {
            let value = words.next().ok_or("--from requires a value")?;
            if p.values.iter().any(|(k, _)| k == "--from") {
                return Err("duplicate --from".to_string());
            }
            p.values.push((arg, value));
            continue;
        }
        if arg == "--json" || arg == "--stdin" || arg == "--even-if-stale" {
            p.flags.push(arg);
            continue;
        }
        // Repeatable, wherever it stands before a lone `--` (#592).
        if arg == "--file" {
            let value = words.next().ok_or("--file requires a value")?;
            p.values.push((arg, value));
            continue;
        }
        if arg.starts_with("--") && !selector {
            return Err(format!(
                "unknown option {arg} (put text that starts with -- after a lone --)"
            ));
        }
        leading = false;
        p.positional.push(arg);
    }
    Ok((picks, p))
}

/// Send to several chats at once: one receipt per chat, then a summary (or a JSON array).
fn broadcast_to(
    picks: Vec<Pick>,
    text: &str,
    from: Option<&str>,
    machine: bool,
) -> Result<(), String> {
    let (store, me) = open()?;
    let id = chat::new_trace_id();
    chat::trace::begin(&store, &id);
    let from = chat::trace_result(&store, &id, "sender.discovery", || sender(&store, from))
        .map_err(|e| trace_cli_error(&e, &id, machine))?;
    let outcomes = chat::broadcast_traced(&store, &me, &from, &picks, text, &id)
        .map_err(|e| trace_cli_error(&e, &id, machine))?;
    finish_all(&outcomes, machine)
}

/// Print every receipt of a broadcast, then exit 0 (all delivered, queued, held or sent), 2 (any
/// refused or unsupported) or 3 (any unknown).
fn finish_all(outcomes: &[SendOutcome], machine: bool) -> Result<(), String> {
    if machine {
        let rows: Vec<Value> = outcomes
            .iter()
            .map(|o| {
                json!({"msgId": o.msg_id, "state": o.status, "detail": o.detail,
                       "to": o.to.display})
            })
            .collect();
        println!("{}", Value::Array(rows));
    } else {
        for o in outcomes {
            println!(
                "{}: {} ({}) [{}]",
                o.to.display, o.status, o.detail, o.msg_id
            );
        }
        println!("{}", summary(outcomes));
    }
    let code = broadcast_exit_code(outcomes);
    if code != 0 {
        let _ = std::io::stdout().flush();
        std::process::exit(code);
    }
    Ok(())
}

fn broadcast_exit_code(outcomes: &[SendOutcome]) -> i32 {
    let states: Vec<&str> = outcomes.iter().map(|o| o.status.as_str()).collect();
    if states
        .iter()
        .any(|s| matches!(*s, "refused" | "unsupported"))
    {
        2
    } else if states
        .iter()
        .any(|s| !matches!(*s, "delivered" | "queued" | "held" | "sent"))
    {
        3
    } else {
        0
    }
}

/// "3 chats: 2 delivered, 1 refused".
fn summary(outcomes: &[SendOutcome]) -> String {
    let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
    for o in outcomes {
        *counts.entry(o.status.as_str()).or_insert(0) += 1;
    }
    let parts: Vec<String> = counts
        .iter()
        .map(|(state, n)| format!("{n} {state}"))
        .collect();
    let chats = if outcomes.len() == 1 { "chat" } else { "chats" };
    format!("{} {chats}: {}", outcomes.len(), parts.join(", "))
}

/// Hand a `--file` send to this computer's engine and wait for the files to arrive (#592).
fn hand_off_files(ask: &chat::FileSendAsk<'_>) -> Result<(), chat::FileSendFailure> {
    host()
        .send_agent_files(ask.device_id, ask.chat, ask.reference, ask.paths)
        .map_err(|(uncertain, message)| chat::FileSendFailure { uncertain, message })
}

/// Print a `--file` send's receipt; a name that resolves to no chat, or a refusal before anything
/// was sent, is a `refused` receipt (exit 2), as for a plain send.
fn finish_files(
    result: Result<SendOutcome, chat::ChatError>,
    to: &str,
    alias: &str,
    machine: bool,
    id: &str,
) -> Result<(), String> {
    match result {
        Ok(outcome) => finish(&outcome, machine),
        Err(e @ (chat::ChatError::NotFound(_) | chat::ChatError::Invalid(_))) => {
            finish(&chat::failed_traced(to, alias, &e, id), machine)
        }
        Err(e @ (chat::ChatError::Ambiguous { .. } | chat::ChatError::Stale(_))) => {
            refuse_routing_traced(&e, machine, id)
        }
        Err(e) => Err(trace_cli_error(&e, id, machine)),
    }
}

/// A session id cut to its first eight characters, for an error that must tell two chats apart.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Answer a received message: send to the chat it came from, as the chat that received it.
fn reply(id: &str, text: &str, files: &[std::path::PathBuf], machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let trace_id = chat::new_trace_id();
    chat::trace::begin(&store, &trace_id);
    let (from, to) = chat::trace_result(&store, &trace_id, "reply.lookup", || {
        let route = store
            .reply_route(id.trim())
            .ok_or("no route for that message id (routes are kept 24 h)")?;
        let sessions = roster::local_sessions(&store);
        // A Codex chat is not always in the local roster; look it up by its thread id.
        let known = sessions
            .iter()
            .find(|s| s.id == route.to_session)
            .cloned()
            .or_else(|| {
                roster::resolve_exact_codex(&route.to_session)
                    .ok()
                    .flatten()
            })
            .or_else(|| roster::resolve_exact_hook(&store, &route.to_session));
        // A reply goes out as the chat the message went to, so it must run inside that chat (#191):
        // anything else would answer in another chat's name. Its words are stamped only when that
        // chat is proven from this process.
        let proven = chat::identify(&sessions, &|name| std::env::var(name).ok(), None)
            .map_err(|e| trace_cli_error(&e, &trace_id, machine))?;
        // Identity is the chat's session id, never its display name, so two chats sharing a name stay
        // distinct (#506). A process id is not proof of the same chat (one app can host several
        // sessions, and pids are reused), so a chat whose session id changed (compaction, resume) is
        // refused until the harness reports a proven session successor.
        let is_same_chat = proven.id == route.to_session;
        // The same live Claude process after its session id changed (compaction, resume): the same
        // chat, so the reply is allowed and comes from its current session (#506).
        let claude_dirs: Vec<std::path::PathBuf> = chat::deliver_claude::sessions_dirs()
            .into_iter()
            .map(|(dir, _)| dir)
            .collect();
        let successor = !is_same_chat
            && chat::route_successor(
                &claude_dirs,
                &proven,
                route.to_pid,
                route.to_start.as_deref(),
            );
        if !is_same_chat && !successor {
            let runs_in = if proven.id == chat::CLI_CALLER_ID {
                "a terminal, not in a chat".to_string()
            } else {
                format!(
                    "the chat \"{}\" (session {})",
                    proven.name,
                    short_id(&proven.id)
                )
            };
            // Two chats can share a name; the session ids are what tell them apart, so say which
            // differs (#506).
            let same_name = if proven.name == route.to_name {
                format!(
                    "; both are named \"{}\", so the names do not tell them apart: the session id \
                     {} changed or this is a different chat",
                    route.to_name,
                    short_id(&route.to_session)
                )
            } else {
                String::new()
            };
            return Err(format!(
                "that message went to the chat \"{}\" (session {}), but this command runs in \
                 {runs_in}; a reply is sent only from inside the chat that received the message{same_name}",
                route.to_name,
                short_id(&route.to_session)
            ));
        }
        if successor {
            // One line in the send log: the reply came from the same process under a new session id.
            super::sendlog::event(
                rightkit_logs::Level::Info,
                "chat.reply.successor",
                json!({
                    "pid": route.to_pid,
                    "sameName": proven.name == route.to_name,
                    "msgId": route.msg_id,
                }),
            );
        }
        let detected = proven.detected;
        let from = if successor {
            proven.clone()
        } else {
            match known.as_ref() {
                Some(s) => Caller {
                    id: s.id.clone(),
                    name: chat::sender_name_of(s),
                    reply_socket: if s.kind == "claude" {
                        s.messaging_socket.clone()
                    } else {
                        None
                    },
                    detected,
                },
                None => Caller {
                    id: route.to_session.clone(),
                    name: route.to_session.clone(),
                    reply_socket: None,
                    detected,
                },
            }
        };
        // A send from a plain terminal (a script, a scheduled wake) has no chat to answer.
        if route.from_session == chat::CLI_CALLER_ID {
            return Err(format!(
                "that message was sent from a terminal on {}, not from a chat, so it takes no reply; \
                 use `@cli@ chat send` to reach a chat",
                route.from_device
            ));
        }
        // The sender's own device name may differ from this computer's name for the link.
        // The reply goes to the exact chat id the message came from, never to a chat found by title.
        let to = chat::reply_address(&store, &me, &route.from_device, &route.from_session)?;
        Ok::<_, String>((from, to))
    }).map_err(|e| trace_cli_error(&e, &trace_id, machine))?;
    if !files.is_empty() {
        // The files go to the exact chat the message came from, by its id (#592).
        let target = match to.split_once(':') {
            Some((device, session)) => chat::FileTarget::Reply { device, session },
            None => chat::FileTarget::Named(&to),
        };
        return finish_files(
            chat::send_with_files_traced(
                &store,
                &me,
                &from,
                target,
                text,
                files,
                &hand_off_files,
                &trace_id,
            ),
            &to,
            &me.alias,
            machine,
            &trace_id,
        );
    }
    let outcome = match to.split_once(':') {
        Some((device, session)) => {
            chat::send_reply_traced(&store, &me, &from, device, session, text, &trace_id)
        }
        None => chat::send_text_traced(&store, &me, &from, &to, text, &trace_id),
    }
    .map_err(|e| e.to_string())?;
    finish(&outcome, machine)
}

/// Print the typed messages of `said`, for a person or as JSON.
fn print_found(found: &said::Found, machine: bool) {
    if machine {
        println!("{}", found.to_json());
        return;
    }
    let from = found
        .message_id
        .as_ref()
        .map(|id| format!(" (sent message {id})"))
        .unwrap_or_default();
    if found.messages.is_empty() {
        println!(
            "Your user typed nothing in {} chat {} on {}{from}: no typed messages or pop-up answers found in its transcript.",
            found.kind, found.session, found.device
        );
    } else {
        println!(
            "Your user's last {} message{} (typed, or an answer to a pop-up question) in {} chat {} on {}{from}, oldest first:",
            found.messages.len(),
            if found.messages.len() == 1 { "" } else { "s" },
            found.kind,
            found.session,
            found.device
        );
    }
    for message in &found.messages {
        println!(
            "\n[{}]\n{}",
            message.ts.as_deref().unwrap_or("no time"),
            message.text
        );
    }
    if found.omitted > 0 {
        println!(
            "\n{} non-text user turn{} omitted (attachments or other non-text content, which `said` cannot show).",
            found.omitted,
            if found.omitted == 1 { "" } else { "s" }
        );
    }
    if found.scan_cut {
        println!(
            "\n(Only the newest part of the transcript was searched; older messages may exist.)"
        );
    }
}

/// `said <message-id|chat>`: the typed messages of the chat that sent a message.
fn said_cmd(target: &str, limit: usize, machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    match said::said(&store, &me, target, limit) {
        Ok(found) => {
            print_found(&found, machine);
            Ok(())
        }
        Err(SaidError::Refused(why)) => {
            if machine {
                println!("{}", json!({"ok": false, "refused": true, "error": why}));
            } else {
                eprintln!("{why}");
            }
            let _ = std::io::stdout().flush();
            std::process::exit(2);
        }
        Err(SaidError::Failed(why)) => Err(why),
    }
}

/// `confirm <message-id>`: the engine decides by the sender's role whether the owner's phone must
/// confirm, and this waits for it (`chat::confirm`). Exits with the outcome's code.
fn confirm_cmd(message_id: &str, summary: Option<&str>, machine: bool) -> Result<(), String> {
    let summary = Some(
        summary
            .filter(|s| !s.trim().is_empty())
            .ok_or("--summary is required")?,
    );
    use EngineFailure as Failure;
    let (store, _me) = open()?;
    let call = |method: &str, params: Value| {
        host()
            .engine_call(method, params)
            .map_err(|failure| match failure {
                Failure::Unreachable(message) | Failure::Unknown(message) => confirm::CallError {
                    code: "engine".to_string(),
                    message,
                },
                Failure::Refused { code, message } => confirm::CallError { code, message },
            })
    };
    let waiting = |id: &str| {
        if !machine {
            eprintln!("Waiting for your iPhone to answer (challenge {id}, up to 10 minutes).");
        }
    };
    let outcome = confirm::confirm(&store, message_id, summary, &call, &waiting);
    if machine {
        println!("{}", outcome.to_json());
    } else {
        println!("{}", outcome.line());
    }
    let _ = std::io::stdout().flush();
    match outcome.exit_code() {
        0 => Ok(()),
        code => std::process::exit(code),
    }
}

/// `said --local <kind:id|id>` (run over ssh by a linked computer): this computer's
/// transcript as one JSON line, exit 0 even when refused so the answer says why.
fn said_local(chat: &str, limit: usize) -> Result<(), String> {
    let (store, me) = open()?;
    let (kind, session) = match chat.split_once(':') {
        Some((kind, id))
            if matches!(kind, "claude" | "codex") || chat::codex_hook::valid_hook_harness(kind) =>
        {
            (Some(kind), id)
        }
        _ => (None, chat),
    };
    let line = match said::read_local(&store, &me, kind, session, limit) {
        Ok(found) => {
            let mut value = found.to_json();
            value["ok"] = json!(true);
            value
        }
        Err(SaidError::Refused(why)) => json!({"ok": false, "refused": true, "error": why}),
        Err(SaidError::Failed(why)) => json!({"ok": false, "refused": false, "error": why}),
    };
    println!("{line}");
    Ok(())
}

fn link(device: &str, ssh: &str, cli: Option<&str>, machine: bool) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let link = Link {
        device: device.trim().to_string(),
        ssh: ssh.trim().to_string(),
        cli: cli
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .unwrap_or(&brand().cli)
            .to_string(),
        ..Link::default()
    };
    let listing = links::add(&store, link.clone())?;
    if machine {
        println!("{}", {
            // The key is the brand's command name (`pulse` for Pulse), as it always was.
            let mut row = serde_json::Map::new();
            row.insert("device".into(), json!(link.device));
            row.insert("ssh".into(), json!(link.ssh));
            row.insert(brand().cli.clone(), json!(link.cli));
            row.insert("chats".into(), json!(listing.chats.len()));
            row.insert("theirName".into(), json!(listing.device));
            Value::Object(row)
        });
    } else {
        println!(
            "Linked {} ({}): {} chats there now.",
            link.device,
            link.ssh,
            listing.chats.len()
        );
    }
    Ok(())
}

fn unlink(device: &str) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    if links::remove(&store, device)? {
        println!("Unlinked {device}.");
        Ok(())
    } else {
        Err(format!("no link named {device}"))
    }
}

/// The discovery line of `status`, per harness: not found, found, plugin installed, approval
/// pending (Muse waits for a yes in the notch, or `@cli@ chat plugins --allow muse`) or declined.
fn plugins_line() -> String {
    let words = |state: &str| match state {
        "notFound" => "not found",
        "found" => "found, plugin not installed",
        "installed" => "plugin installed",
        "approvalPending" => "approval pending (@cli@ chat plugins --allow muse)",
        "declined" => "declined (turn Chat plugin auto-install off and on to ask again)",
        _ => "failed (@cli@ chat plugins)",
    };
    let parts: Vec<String> = install::view()
        .iter()
        .filter(|r| r.found || r.state != "notFound")
        .map(|r| {
            let name = r.profile.as_deref().map_or_else(
                || r.harness.clone(),
                |profile| format!("{} ({profile})", r.harness),
            );
            // A Claude profile without a working plugin: say plainly that hook delivery cannot
            // reach its chats, and why (the profile's own installed_plugins.json / settings
            // enabledPlugins, #581).
            let no_plugin = r.harness == "claude" && matches!(r.state.as_str(), "found" | "failed");
            if no_plugin {
                let why = if r.note.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", r.note)
                };
                return format!(
                    "{name} no working plugin: hook delivery cannot reach this profile's chats{why}"
                );
            }
            format!("{name} {}", t(words(&r.state)))
        })
        .collect();
    format!(
        "Chat plugins: {}",
        if parts.is_empty() {
            "no harness found".to_string()
        } else {
            parts.join("; ")
        }
    )
}

/// The Muse plugin hook line of `status`.
fn muse_hook_line() -> String {
    let last = match chat::codex_hook::newest_hook_heartbeat("muse") {
        Some(secs) => {
            let age = chat::envelope::now_ms().saturating_sub(secs.saturating_mul(1000));
            format!("last run {} ago", since_text(age))
        }
        None => t("never run (@cli@ chat install --muse, then approve the plugin in Muse)"),
    };
    format!("Muse hooks: {last}")
}

/// The Claude plugin hook line of `status`.
fn claude_hook_line() -> String {
    let last = match chat::codex_hook::newest_claude_heartbeat() {
        Some(secs) => {
            let age = chat::envelope::now_ms().saturating_sub(secs.saturating_mul(1000));
            format!("last run {} ago", since_text(age))
        }
        None => "never run (plugin not installed or hooks not active)".to_string(),
    };
    // Windows: the plugin's hooks run hooks/@launcher@, which `chat install` copies in.
    let missing = if cfg!(windows) {
        install::claude_plugin_dirs()
            .iter()
            .filter(|dir| !install::launcher_is_program(dir))
            .count()
    } else {
        0
    };
    let launcher = if missing > 0 {
        t(&format!(
            "; launcher missing or not a Windows program in {missing} plugin folder(s) (@cli@ chat install --claude-plugin)"
        ))
    } else {
        String::new()
    };
    let repaired = match install::last_launcher_repair() {
        Some((at_ms, folders)) if cfg!(windows) => format!(
            "; launcher repaired in {folders} folder(s) {} ago",
            since_text(chat::envelope::now_ms().saturating_sub(at_ms))
        ),
        _ => String::new(),
    };
    // Which profile cannot run the hook, and why (listed, enabled, marketplace present).
    let problems = install::claude_plugin_problems();
    let why = if problems.is_empty() {
        String::new()
    } else {
        t(&format!(
            "; not ready: {} (@cli@ chat plugins repairs what it can)",
            problems.join(" | ")
        ))
    };
    format!("Claude plugin hook: {last}{launcher}{repaired}{why}")
}

/// The Codex hook line of `status`.
fn codex_hook_line() -> String {
    let present = if install::hooks_present() {
        "installed".to_string()
    } else {
        t("not installed (@cli@ chat install --codex)")
    };
    let heartbeat = chat::codex_hook::newest_heartbeat();
    let last = match heartbeat {
        Some(secs) => {
            let age = chat::envelope::now_ms().saturating_sub(secs.saturating_mul(1000));
            format!("last run {} ago", since_text(age))
        }
        None => "never run (not trusted yet?)".to_string(),
    };
    let feature = if install::hooks_present() && !install::hooks_feature_on() {
        t("; Codex hooks feature is off (@cli@ chat install --codex)")
    } else {
        String::new()
    };
    let restart = if install::hooks_present()
        && install::hooks_feature_on()
        && codex_restart_needed(heartbeat)
    {
        "; restart the Codex app: no Codex hook has run since it started"
    } else {
        ""
    };
    format!("Codex hooks: {present}; {last}{feature}{restart}")
}

/// Whether the Codex app must be restarted for its enabled hooks to run: no Codex hook ran
/// (`last_hook`, unix seconds) since a running Codex process started, or none ever ran. Read-only.
fn codex_restart_needed(last_hook: Option<u64>) -> bool {
    use sysinfo::{ProcessesToUpdate, System};
    let Some(last) = last_hook else {
        return true;
    };
    let Ok(mut system) = System::new() else {
        return false;
    };
    system.refresh_processes(ProcessesToUpdate::All, true);
    system.processes().values().any(|p| {
        let name = p.name().to_string_lossy().to_lowercase();
        (name == "codex" || name == "codex.exe") && p.start_time() > last
    })
}

/// `@cli@ chat whoami`: the proof trace of this command's sender (`chat::whoami`).
fn whoami(machine: bool) -> Result<(), String> {
    let (store, _) = open()?;
    let env = |name: &str| std::env::var(name).ok();
    let report = chat::whoami::report(&roster::local_sessions(&store), &env);
    if machine {
        println!("{report}");
    } else {
        print!("{}", chat::whoami::render(&report));
    }
    Ok(())
}

/// `@cli@ chat hook-delivery on|off|auto`: writes or removes `chatClaudeHookDelivery` in policy.json.
fn hook_delivery(args: Vec<String>, machine: bool) -> Result<(), String> {
    let p = Parsed::new(args, &[], &["--json"])?;
    let machine = machine || p.flag("--json");
    let mode = match p.positional.as_slice() {
        [mode] if matches!(mode.as_str(), "on" | "off" | "auto") => mode.clone(),
        _ => return Err("hook-delivery takes one of: on, off, auto".into()),
    };
    let (store, _me) = open()?;
    store
        .set_claude_hook_delivery_mode(&mode)
        .map_err(|e| format!("could not write chatClaudeHookDelivery: {e}"))?;
    if machine {
        println!(
            "{}",
            json!({"chatClaudeHookDeliveryMode": mode,
                   "chatClaudeHookDelivery": store.claude_hook_delivery()})
        );
    } else {
        println!("{}", install::claude_hook_delivery_line(&store));
    }
    Ok(())
}

/// The chat switches `policy.json` holds that the notch does not publish, so `@cli@ settings`
/// cannot reach them (C-D9). `claudeHookDelivery` keeps `hook-delivery`; `stampUserWords` and
/// `muse` have no CLI path yet. `chatOverDeviceLink` is not in `policy.json`: the notch publishes
/// it, so `@cli@ settings` reaches it.
pub const POLICY_SWITCHES: [&str; 10] = [
    "enabled",
    "terminalChats",
    "claudeProfiles",
    "codexWakeIdle",
    "claudeWakeIdle",
    "museWakeIdle",
    "codexAppDelivery",
    "codexHiddenDelivery",
    "chatReceiptNotices",
    "chatSocketFallback",
];

const POLICY_USAGE: &str = "usage: @cli@ chat policy [list] [--json]
       @cli@ chat policy get <key> [--json]
       @cli@ chat policy set <key> on|off [--json]";

/// The value of one `POLICY_SWITCHES` key; `None` for any other name.
fn policy_read(store: &Store, key: &str) -> Option<bool> {
    Some(match key {
        "enabled" => store.chat_enabled(),
        "terminalChats" => store.terminal_chats(),
        "claudeProfiles" => store.claude_profiles(),
        "codexWakeIdle" => store.codex_wake_idle(),
        "claudeWakeIdle" => store.claude_wake_idle(),
        "museWakeIdle" => store.muse_wake_idle(),
        "codexAppDelivery" => store.codex_app_delivery(),
        "codexHiddenDelivery" => store.codex_hidden_delivery(),
        "chatReceiptNotices" => store.receipt_notices(),
        "chatSocketFallback" => store.socket_fallback(),
        _ => return None,
    })
}

/// Writes one `POLICY_SWITCHES` key; `None` for any other name.
fn policy_write(store: &Store, key: &str, on: bool) -> Option<std::io::Result<()>> {
    Some(match key {
        "enabled" => store.set_chat_enabled(on),
        "terminalChats" => store.set_terminal_chats(on),
        "claudeProfiles" => store.set_claude_profiles(on),
        "codexWakeIdle" => store.set_codex_wake_idle(on),
        "claudeWakeIdle" => store.set_claude_wake_idle(on),
        "museWakeIdle" => store.set_muse_wake_idle(on),
        "codexAppDelivery" => store.set_codex_app_delivery(on),
        "codexHiddenDelivery" => store.set_codex_hidden_delivery(on),
        "chatReceiptNotices" => store.set_receipt_notices(on),
        "chatSocketFallback" => store.set_socket_fallback(on),
        _ => return None,
    })
}

/// The name as a `POLICY_SWITCHES` entry, or the reason it is not one this command sets.
fn policy_key(key: &str) -> Result<&'static str, String> {
    if key == "claudeHookDelivery" {
        return Err("claudeHookDelivery is set with @cli@ chat hook-delivery on|off|auto".into());
    }
    POLICY_SWITCHES
        .iter()
        .copied()
        .find(|known| *known == key)
        .ok_or_else(|| {
            format!(
                "{key} is not a chat switch this command sets; chat switches: {}",
                POLICY_SWITCHES.join(", ")
            )
        })
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// `@cli@ chat policy [list | get <key> | set <key> on|off]`: reads or sets the chat switches in
/// `policy.json` that the notch does not publish. `set` writes the file at once, with no dry run,
/// as `hook-delivery` does; Chat reads the file when it needs a switch.
fn policy(args: Vec<String>, machine: bool) -> Result<(), String> {
    let p = Parsed::new(args, &[], &["--json"])?;
    let machine = machine || p.flag("--json");
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let words: Vec<&str> = p.positional.iter().map(String::as_str).collect();
    match words.as_slice() {
        [] | ["list"] => {
            let rows: Vec<(&str, bool)> = POLICY_SWITCHES
                .iter()
                .filter_map(|key| policy_read(&store, key).map(|on| (*key, on)))
                .collect();
            if machine {
                let map: serde_json::Map<String, Value> = rows
                    .iter()
                    .map(|(key, on)| (key.to_string(), json!(on)))
                    .collect();
                println!("{}", json!({ "policy": map }));
            } else {
                for (key, on) in rows {
                    println!("{key} = {}", on_off(on));
                }
            }
            Ok(())
        }
        ["get", key] => {
            let key = policy_key(key)?;
            let on = policy_read(&store, key).ok_or_else(|| format!("{key} could not be read"))?;
            if machine {
                println!("{}", json!({ "key": key, "value": on }));
            } else {
                println!("{}", on_off(on));
            }
            Ok(())
        }
        ["set", key, value] => {
            let key = policy_key(key)?;
            let on = match value.to_lowercase().as_str() {
                "on" => true,
                "off" => false,
                _ => return Err(format!("{key} takes on or off, not \"{value}\"")),
            };
            policy_write(&store, key, on)
                .ok_or_else(|| format!("{key} could not be written"))?
                .map_err(|e| format!("could not write {key} to policy.json: {e}"))?;
            if machine {
                println!("{}", json!({ "key": key, "value": on }));
            } else {
                println!("{key} is now {}", on_off(on));
            }
            Ok(())
        }
        _ => Err(POLICY_USAGE.into()),
    }
}

/// The device link as the CLI reaches it: the resident engine holds the mesh, so the paired
/// computers and every call to one go through `device.chat.links` and `device.chat.call`. The
/// peer list is asked once per command, and never waits for a starting engine.
#[derive(Default)]
struct EngineMesh {
    peers: std::sync::Mutex<Option<(Vec<MeshPeer>, mesh::PeersState)>>,
}

/// The last good list of paired computers the engine answered with (`mesh-peers.json` in the chat
/// state folder), kept so a failed lookup does not make the paired computers vanish (#500).
const SAVED_PEERS: &str = "mesh-peers.json";

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct SavedPeers {
    #[serde(default)]
    peers: Vec<(String, String)>,
    #[serde(default, rename = "savedMs")]
    saved_ms: u64,
}

impl EngineMesh {
    /// The engine's answer, or why there is none.
    fn ask() -> Result<Vec<MeshPeer>, String> {
        use EngineFailure as Failure;
        match host().engine_call_quick("device.chat.links", json!({})) {
            Ok(answer) => Ok(answer["peers"]
                .as_array()
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            Some(MeshPeer {
                                device_id: row["deviceId"].as_str()?.to_string(),
                                name: row["name"].as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()),
            Err(Failure::Unreachable(why)) if host().is_elevated(&why) => Err(t(
                "this terminal runs as administrator, which the @product@ engine refuses; run the command from a normal (non-administrator) terminal",
            )),
            Err(Failure::Unreachable(why) | Failure::Unknown(why)) => Err(why),
            Err(Failure::Refused { code, message }) => Err(format!("{code}: {message}")),
        }
    }
}

impl MeshChat for EngineMesh {
    fn peers(&self) -> Vec<MeshPeer> {
        let mut cached = self.peers.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((peers, _)) = cached.as_ref() {
            return peers.clone();
        }
        let store = chat::Store::open_default().ok();
        let (found, state) = match Self::ask() {
            Ok(peers) => {
                if let Some(store) = &store {
                    let saved = SavedPeers {
                        peers: peers
                            .iter()
                            .map(|p| (p.device_id.clone(), p.name.clone()))
                            .collect(),
                        saved_ms: chat::envelope::now_ms(),
                    };
                    let kept: Option<SavedPeers> = store.read_state(SAVED_PEERS);
                    if kept.is_none_or(|k| k.peers != saved.peers) {
                        let _ = store.update_state(SAVED_PEERS, move |slot: &mut SavedPeers| {
                            *slot = saved;
                        });
                    }
                }
                (peers, mesh::PeersState::default())
            }
            // The lookup failed: the last good list stands in, with its age, and the error is kept
            // for `status` and `peers` to print. It is never an empty list that reads as "nobody".
            Err(why) => {
                let saved: Option<SavedPeers> =
                    store.as_ref().and_then(|s| s.read_state(SAVED_PEERS));
                match saved.filter(|s| !s.peers.is_empty()) {
                    Some(saved) => (
                        saved
                            .peers
                            .into_iter()
                            .map(|(device_id, name)| MeshPeer { device_id, name })
                            .collect(),
                        mesh::PeersState {
                            error: Some(why),
                            list_age_ms: Some(
                                chat::envelope::now_ms().saturating_sub(saved.saved_ms),
                            ),
                        },
                    ),
                    None => (
                        Vec::new(),
                        mesh::PeersState {
                            error: Some(why),
                            list_age_ms: None,
                        },
                    ),
                }
            }
        };
        *cached = Some((found.clone(), state));
        found
    }

    fn state(&self) -> mesh::PeersState {
        self.peers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(_, state)| state.clone())
            .unwrap_or_default()
    }

    fn call(&self, device_id: &str, method: &str, params: Value) -> Result<Value, MeshFailure> {
        self.call_within(device_id, method, params, host().chat_call_wait())
    }

    fn call_within(
        &self,
        device_id: &str,
        method: &str,
        params: Value,
        wait: std::time::Duration,
    ) -> Result<Value, MeshFailure> {
        use EngineFailure as Failure;
        let request = json!({"deviceId": device_id, "method": method, "params": params});
        // A bounded listing (shorter than a send's `CHAT_CALL_WAIT`) must not wait out a starting
        // engine for `START_WAIT`: the whole call is bounded by `wait` (C-D4). A send keeps its wait.
        let answer = if wait < host().chat_call_wait() {
            host().engine_call_bounded("device.chat.call", request, wait)
        } else {
            host().engine_call_waiting("device.chat.call", request, wait)
        };
        match answer {
            Ok(answer) if answer["ok"].as_bool() == Some(true) => Ok(answer["result"].clone()),
            Ok(answer) => Err(MeshFailure::new(
                answer["code"].as_str().unwrap_or("refused"),
                answer["message"].as_str().unwrap_or(""),
                answer["mayHaveArrived"].as_bool().unwrap_or(false),
            )),
            Err(Failure::Unreachable(why)) => {
                Err(MeshFailure::new("engine_unreachable", why, false))
            }
            // The request was sent and nothing came back: the other computer may have acted.
            Err(Failure::Unknown(why)) => Err(MeshFailure::new("engine_unknown", why, true)),
            Err(Failure::Refused { code, message }) => Err(MeshFailure::new(&code, message, false)),
        }
    }
}

/// How a link reaches its computer, for `status`.
fn link_transport(link: &Link) -> &'static str {
    if mesh::mesh_id(link).is_some() {
        "device link"
    } else {
        "ssh"
    }
}

/// Where a link goes, for `status`: "device link" or the ssh destination.
fn link_label(link: &Link) -> String {
    match mesh::mesh_id(link) {
        Some(_) => "device link".to_string(),
        None => link.ssh.clone(),
    }
}

fn status(machine: bool) -> Result<(), String> {
    let (store, me) = open()?;
    let here = roster::local_sessions(&store).len();
    // Where Claude chats are looked for, and any running Claude whose chat @product@ cannot list:
    // silence never means "no chat" (#352).
    let claude_folders = chat::deliver_claude::sessions_dirs();
    let scan = chat::deliver_claude::running_claude_scan();
    let hub = control::hub_running(&store);
    let remotes = links::list_all_within(&store, Some(mesh::PEERS_WAIT));
    let lookup = mesh::peers_state();
    if machine {
        let rows: Vec<Value> = remotes
            .iter()
            .map(|r| match &r.listing {
                Ok(l) => json!({"device": r.link.device, "ssh": r.link.ssh,
                                "transport": link_transport(&r.link),
                                "chats": l.chats.len(), "error": Value::Null}),
                Err(e) => json!({"device": r.link.device, "ssh": r.link.ssh,
                                 "transport": link_transport(&r.link),
                                 "chats": Value::Null, "error": e}),
            })
            .collect();
        println!(
            "{}",
            json!({"hubRunning": hub, "device": me.alias, "chatsHere": here, "links": rows,
                   "deviceLinkLookup": {
                       "error": lookup.error,
                       "savedListAgeMs": lookup.list_age_ms,
                       "note": lookup.note()},
                   "chatEnabled": store.chat_enabled(),
                   "chatReceiptNotices": store.receipt_notices(),
                   "codexHooksInstalled": install::hooks_present(),
                   "codexHookLastRunSecs": chat::codex_hook::newest_heartbeat(),
                   "claudeHookLastRunSecs": chat::codex_hook::newest_claude_heartbeat(),
                   "museHookLastRunSecs": chat::codex_hook::newest_hook_heartbeat("muse"),
                   "chatMuse": store.muse_chats(),
                   "stampUserWords": store.stamp_user_words(),
                   "chatPlugins": install::view(),
                   "chatClaudeHookDelivery": store.claude_hook_delivery(),
                   "chatClaudeHookDeliveryMode": store.claude_hook_delivery_mode(),
                   "chatTerminalChats": store.terminal_chats(),
                   "chatSocketFallback": store.socket_fallback(),
                   "chatClaudeProfiles": store.claude_profiles(),
                   "chatCodexWakeIdle": store.codex_wake_idle(),
                   "chatClaudeWakeIdle": store.claude_wake_idle(),
                   "chatMuseWakeIdle": store.muse_wake_idle(),
                   "chatCodexAppDelivery": store.codex_app_delivery(),
                   "chatCodexHiddenDelivery": store.codex_hidden_delivery(),
                   "claudeConfigFolders": claude_folders.iter().map(|(dir, label)| json!({
                       "sessions": dir.to_string_lossy(), "profile": label})).collect::<Vec<_>>(),
                   "claudeUnreadable": scan.unreadable.iter().map(|(pid, why)| json!({
                       "pid": pid, "reason": why})).collect::<Vec<_>>()})
        );
        return Ok(());
    }
    let hub_line = if hub && cfg!(target_os = "linux") {
        "running (the chat service, `@cli@ chat bridge-serve`: registered with Claude, posts to a chat's live messaging socket; there is no @product@ app on Linux)"
    } else if hub && lookup.error.is_some() {
        "local Chat running; device-link lookup unavailable"
    } else if hub {
        "running"
    } else if cfg!(target_os = "linux") {
        "not running (the chat service, the `@cli@-chat` unit running `@cli@ chat bridge-serve`, is not running; there is no @product@ app on Linux. Until it runs, a busy Claude chat takes messages through its plugin hook, an idle one holds them for `@cli@ chat inbox`)"
    } else {
        "not running (open @product@)"
    };
    println!("{}: {}", brand().product, t(hub_line));
    if !store.chat_enabled() {
        println!(
            "{}",
            t("Chat off: sends are refused (turn it on in @product@).")
        );
    }
    println!("{here} chats on {}", me.alias);
    if let Some(note) = lookup.note() {
        println!("{note}");
    }
    println!(
        "Claude chat folders read: {}",
        claude_folders
            .iter()
            .map(|(dir, label)| match label {
                Some(label) => format!("{} ({label})", dir.display()),
                None => dir.display().to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (pid, why) in &scan.unreadable {
        println!(
            "{}",
            t("Warning: a running Claude (pid @pid@) has a chat @product@ cannot list: @why@. It is not shown in peers and cannot be messaged.")
                .replace("@pid@", &pid.to_string())
                .replace("@why@", &why.to_string())
        );
    }
    println!("{}", codex_hook_line());
    println!("{}", claude_hook_line());
    println!("{}", muse_hook_line());
    println!("{}", plugins_line());
    println!("{}", install::claude_hook_delivery_line(&store));
    println!(
        "List terminal chats active in the last 6 hours (chatTerminalChats): {}",
        if store.terminal_chats() { "on" } else { "off" }
    );
    println!(
        "Deliver to a Claude chat's live socket when its key file is unusable (chatSocketFallback): {}",
        if store.socket_fallback() { "on" } else { "off" }
    );
    println!(
        "List and reach Claude chats in other Claude profiles, ~/ClaudeProfiles (chatClaudeProfiles): {}",
        if store.claude_profiles() { "on" } else { "off" }
    );
    println!(
        "List, read and reach Muse chats (chatMuse): {}",
        if store.muse_chats() { "on" } else { "off" }
    );
    println!(
        "Stamp my last three typed turns into messages (stampUserWords): {}",
        if store.stamp_user_words() {
            "on"
        } else {
            "off"
        }
    );
    println!(
        "Start a turn in an idle Codex chat when a message arrives (chatCodexWakeIdle): {}",
        if store.codex_wake_idle() { "on" } else { "off" }
    );
    println!(
        "Start a turn in an idle Claude chat (Claude Desktop) when a message arrives (chatClaudeWakeIdle): {}",
        if store.claude_wake_idle() {
            "on"
        } else {
            "off"
        }
    );
    println!(
        "Start a turn in an idle Muse chat when a message arrives (chatMuseWakeIdle; Muse has no supported wake entry yet, so it only holds): {}",
        if store.muse_wake_idle() { "on" } else { "off" }
    );
    println!(
        "Hand messages to a Codex chat the Codex app has loaded (chatCodexAppDelivery): {}",
        if store.codex_app_delivery() {
            "on"
        } else {
            "off"
        }
    );
    println!(
        "Hand a Codex chat messages as hidden context, waking it with one line (chatCodexHiddenDelivery): {}",
        if store.codex_hidden_delivery() {
            "on"
        } else {
            "off"
        }
    );
    println!(
        "Show receipts of your sent messages in chat hooks (chatReceiptNotices): {}",
        if store.receipt_notices() { "on" } else { "off" }
    );
    if remotes.is_empty() {
        println!(
            "{}",
            t(
                "No linked computers (pair one with `@cli@ device`, or link one with `@cli@ chat link <device> <ssh-host>`)."
            )
        );
    }
    for r in &remotes {
        match &r.listing {
            Ok(l) => println!(
                "{} chats on {} ({})",
                l.chats.len(),
                r.link.device,
                link_label(&r.link)
            ),
            // Unknown, never "0 chats": an unanswered link (a slow one included, #582) says so.
            Err(e) => println!(
                "{} ({}): chats unknown, no answer ({e})",
                r.link.device,
                link_label(&r.link)
            ),
        }
    }
    let now = chat::envelope::now_ms();
    for state in link_states(&store) {
        if let Some(error) = &state.last_error {
            let seen = state
                .age_ms(now)
                .map_or("never reached".to_string(), |age| {
                    format!("last reached {} ago", since_text(age))
                });
            println!("{}: {seen}; last error: {error}", state.device);
        }
    }
    Ok(())
}

fn inbox(p: &Parsed, machine: bool) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let session = match p.value("--session") {
        Some(id) => id,
        None => caller(&store, p.value("--from").as_deref())?.id,
    };
    if let Some(id) = p.value("--refuse") {
        return refuse_held(
            &store,
            &session,
            id.trim(),
            p.value("--reason").as_deref(),
            machine,
        );
    }
    let all = p.flag("--all");
    let acked = p.flag("--ack");
    let unread_entries = store
        .take_unread_ack(&session, None, false)
        .map_err(|e| e.to_string())?;
    // Only messages unread before this call are released by `--ack` (with `--all` read ones are not).
    let unread: Vec<String> = unread_entries.iter().map(|e| e.env.id.clone()).collect();
    let entries = if all {
        store.read_inbox(&session)
    } else {
        unread_entries
    };
    let newest = entries.iter().map(|e| e.seq).max();
    if let (true, Some(seq)) = (acked, newest) {
        store.ack_inbox(&session, seq).map_err(|e| e.to_string())?;
    }
    let refused = store.refused_inbox(&session);
    let shown: Vec<_> = entries
        .into_iter()
        .filter(|e| !refused.contains(&e.env.id))
        .collect();
    if acked {
        for entry in shown.iter().filter(|e| unread.contains(&e.env.id)) {
            receipts::released(&store, &session, &entry.env);
        }
    }
    let after = if all {
        0
    } else {
        store.receipt_read_seq(&session)
    };
    let updates = store.receipts_after(&session, after);
    if let (true, Some(seq)) = (acked, updates.iter().map(|u| u.seq).max()) {
        store
            .ack_receipts(&session, seq)
            .map_err(|e| e.to_string())?;
    }
    let evicted = store.inbox_evicted(&session);
    if machine {
        let rows: Vec<Value> = shown
            .iter()
            .map(|e| {
                json!({"id": e.env.id, "seq": e.seq, "from": e.env.from.name,
                       "body": e.env.body})
            })
            .collect();
        let receipt_rows: Vec<Value> = updates.iter().map(receipt_row).collect();
        println!(
            "{}",
            json!({"messages": rows, "receipts": receipt_rows, "evicted": evicted,
                   "acknowledged": acked && newest.is_some()})
        );
        return Ok(());
    }
    if shown.is_empty() && updates.is_empty() {
        println!("No unread messages.");
    }
    for entry in &shown {
        println!("{}\n{}\n", entry.env.from.name, entry.env.body);
    }
    for update in &updates {
        println!("{}", receipts::describe(update));
    }
    if evicted > 0 {
        println!("{evicted} older messages were dropped to keep the inbox small.");
    }
    if (!shown.is_empty() || !updates.is_empty()) && !acked {
        println!("Shown again next time; add --ack to mark these read.");
    }
    Ok(())
}

/// `inbox --refuse ID`: refuse one held message. It stops being shown, and its sender gets a
/// `refused` receipt with the reason.
fn refuse_held(
    store: &Store,
    session: &str,
    id: &str,
    reason: Option<&str>,
    machine: bool,
) -> Result<(), String> {
    let entry = store
        .read_inbox(session)
        .into_iter()
        .find(|e| e.env.id == id)
        .ok_or_else(|| format!("no held message {id} in this chat's inbox"))?;
    store.refuse_inbox(session, id).map_err(|e| e.to_string())?;
    let why = reason
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("the chat's owner refused it in the chat inbox");
    receipts::refused_by_owner(store, session, &entry.env, why);
    if machine {
        println!(
            "{}",
            json!({"msgId": id, "state": "refused", "detail": why})
        );
    } else {
        println!("Refused {id}; its sender gets a receipt saying so.");
    }
    Ok(())
}

/// `receipts`: what happened to messages this chat sent (unread ones, after `--since N`, or all).
fn receipts_cmd(p: &Parsed, machine: bool) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let session = match p.value("--session") {
        Some(id) => id,
        None => caller(&store, p.value("--from").as_deref())?.id,
    };
    let after = match p.value("--since") {
        Some(text) => text
            .trim()
            .parse::<u64>()
            .map_err(|_| "--since takes a receipt number".to_string())?,
        None if p.flag("--all") => 0,
        None => store.receipt_read_seq(&session),
    };
    let updates = store.receipts_after(&session, after);
    if p.flag("--ack")
        && let Some(seq) = updates.iter().map(|u| u.seq).max()
    {
        store
            .ack_receipts(&session, seq)
            .map_err(|e| e.to_string())?;
    }
    if machine {
        let rows: Vec<Value> = updates.iter().map(receipt_row).collect();
        println!("{}", Value::Array(rows));
        return Ok(());
    }
    if updates.is_empty() {
        println!("No receipts.");
    }
    for update in &updates {
        println!("[{}] {}", update.seq, receipts::describe(update));
    }
    Ok(())
}

/// `receipt <message-id>`: the receipt state of one message and its one-line reason, from this
/// computer's delivery records (messages sent here) and stored receipt updates.
fn receipt_cmd(id: &str, machine: bool) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct Rec {
        id: String,
        #[serde(default)]
        state: String,
        #[serde(default)]
        detail: String,
    }
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let id = id.trim();
    let mut state = String::new();
    let mut detail = String::new();
    for (_, records) in store.list_records::<Vec<Rec>>("delivered") {
        if let Some(r) = records.iter().find(|r| r.id == id && !r.state.is_empty()) {
            state = r.state.clone();
            detail = r.detail.clone();
            break;
        }
    }
    let mut updates: Vec<receipts::Update> = Vec::new();
    if let Ok(dir) = std::fs::read_dir(store.root().join("receipts")) {
        for path in dir.flatten().map(|e| e.path()) {
            if path.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                updates.extend(
                    text.lines()
                        .filter_map(|l| serde_json::from_str::<receipts::Update>(l).ok())
                        .filter(|u| u.msg_id == id),
                );
            }
        }
    }
    updates.sort_by_key(|u| u.ts);
    // The sending computer has no delivery record (that is the receiver's); what it learned is
    // the receipt updates the receiver posted back (a late `delivered`, `seen`, ...). The
    // newest one is the message's state here (#620): `unknown` only when nothing came back.
    if state.is_empty()
        && let Some(last) = updates.last()
    {
        state = last.state.clone();
        detail = format!("reported by {}: {}", last.by, last.detail);
    }
    if state.is_empty() {
        state = "unknown".into();
        detail = "no delivery record for this message on this computer; ask on the computer it was sent to".into();
    }
    if machine {
        let rows: Vec<Value> = updates.iter().map(receipt_row).collect();
        println!(
            "{}",
            json!({"msgId": id, "state": state, "detail": detail, "updates": rows})
        );
        return Ok(());
    }
    println!("receipt: {id} {state} ({detail})");
    for update in &updates {
        println!("  later: {}", receipts::describe(update));
    }
    Ok(())
}

/// A receipt update as JSON, as `receipts --json` and `inbox --json` list it.
fn receipt_row(update: &receipts::Update) -> Value {
    json!({"seq": update.seq, "msgId": update.msg_id, "state": update.state,
           "detail": update.detail, "by": update.by, "ts": update.ts})
}

/// A linked computer's receipt update, arriving over ssh (`@cli@ chat receipt-post`): stored
/// for the sending chat here. Prints one JSON line.
fn receipt_post(encoded: &str) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let bytes = links::unb64(encoded.trim()).ok_or("not base64")?;
    let update: receipts::SignedUpdate =
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    match receipts::apply_remote(&store, update) {
        Ok(stored) => println!("{}", json!({"ok": true, "stored": stored})),
        Err(refusal) => println!(
            "{}",
            json!({"ok": false, "code": refusal.code, "error": refusal.error})
        ),
    }
    Ok(())
}

/// A linked computer's `send`, arriving over ssh: deliver here and print the
/// receipt as one JSON line.
fn post(encoded: &str) -> Result<(), String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let mut env = links::decode_envelope(encoded)?;
    // Only the device link's engine may say an envelope came over the mesh (`mesh::serve`).
    env.mesh.clear();
    let receipt = chat::receive(&store, &env);
    println!(
        "{}",
        json!({"msg_id": receipt.msg_id, "status": receipt.state.as_str(),
               "detail": receipt.detail})
    );
    Ok(())
}

/// "42s", "5min", "3h" or "2d" since `ms` (milliseconds since the epoch).
fn age_text(ms: u64) -> String {
    since_text(chat::envelope::now_ms().saturating_sub(ms))
}

/// `age_text` for an age already in milliseconds.
fn since_text(age_ms: u64) -> String {
    let secs = age_ms / 1000;
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}min", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}
