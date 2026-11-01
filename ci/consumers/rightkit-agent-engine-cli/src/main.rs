// rightkit-agent-engine-cli 0.1.0 consumed by exact registry pin: the schedule subcommand parses. No I/O.
use clap::Parser;
use rightkit_agent_engine_cli::schedule_spec::ScheduleCommand;

fn check(ok: bool, what: &str) {
    if ok {
        println!("ok: {what}");
    } else {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
}

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: ScheduleCommand,
}

fn main() {
    let parsed = Cli::try_parse_from(["x", "create", "payload.json"]);
    check(
        matches!(parsed.map(|c| c.cmd), Ok(ScheduleCommand::Create { .. })),
        "schedule create parses",
    );
    check(
        Cli::try_parse_from(["x", "bogus"]).is_err(),
        "unknown subcommand rejected",
    );
}
