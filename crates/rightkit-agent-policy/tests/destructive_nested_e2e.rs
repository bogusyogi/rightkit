//! End to end through the public guard: a catastrophic command written to a file and run later
//! is refused when the write sits inside a subshell or brace group, inside a shell's `-c` code
//! string, or when the run is a bare name behind a wrapper with a non-numeric argument
//! (CodeRight #457 items 1, 2 and 4). Ordinary commands of the same shapes stay allowed.

use rightkit_agent_policy::catastrophic_reason;

const REFUSED: &[&str] = &[
    // Item 1: the write is inside a group whose output is redirected.
    "( echo 'rm -rf /*' ) > x.sh && sh x.sh",
    "{ echo 'rm -rf /*'; } > x.sh && sh x.sh",
    "( echo hi; echo 'rm -rf /*' ) >> x.sh; bash x.sh",
    // Item 2: a bare name behind a wrapper whose argument is not a number.
    "echo 'rm -rf /*' > /usr/local/bin/x.sh && chmod +x /usr/local/bin/x.sh && flock /tmp/lock x.sh",
    "echo 'rm -rf /*' > x.sh && su builder x.sh",
    // Item 4: the write is inside a shell's code string.
    "sh -c \"echo 'rm -rf /*' > x.sh\" && sh x.sh",
    "bash -lc \"printf 'rm -rf /*' > x.sh\"; bash x.sh",
    // Unchanged: the flat form was already refused.
    "echo 'rm -rf /*' > x.sh && sh x.sh",
];

const ALLOWED: &[&str] = &[
    "( echo hello ) > notes.txt && cat notes.txt",
    "{ echo one; echo two; } > list.txt && wc -l list.txt",
    "sh -c \"echo building > build.log\" && cat build.log",
    "flock /tmp/lock make test",
    "timeout 5 cargo test",
    "echo 'deploy done' > status.txt && su builder ./publish.sh",
    "( cd src && ls ) > files.txt",
];

#[test]
fn nested_writes_and_wrapped_runs_of_a_catastrophic_command_are_refused() {
    let missed: Vec<&str> = REFUSED
        .iter()
        .copied()
        .filter(|command| catastrophic_reason(command).is_none())
        .collect();
    assert!(
        missed.is_empty(),
        "allowed, but should be refused: {missed:#?}"
    );
}

#[test]
fn ordinary_commands_of_the_same_shapes_stay_allowed() {
    let refused: Vec<(&str, &str)> = ALLOWED
        .iter()
        .filter_map(|command| catastrophic_reason(command).map(|reason| (*command, reason)))
        .collect();
    assert!(
        refused.is_empty(),
        "refused, but should be allowed: {refused:#?}"
    );
}
