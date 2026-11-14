//! The non-disableable destructive guard from the host app's safety policy. Unlike the permission engine — which
//! a permissive mode (Bypass) can override — this guard hard-blocks a small set
//! of catastrophic, irreversible, whole-system commands *regardless of mode or
//! trust*. It is intentionally conservative: it targets unambiguous
//! system-destroyers, not ordinary risky dev commands. Where it cannot tell
//! code from data it treats the text as code: refusing a harmless command is an
//! annoyance, allowing a root wipe is a vulnerability.

/// If `command` is catastrophic, return a human-readable reason; else `None`.
pub fn catastrophic_reason(command: &str) -> Option<&'static str> {
    reason(command, 0)
}

/// `catastrophic_reason` for code nested `depth` files deep (text written to a file and run).
fn reason(command: &str, depth: usize) -> Option<&'static str> {
    let norm = command.to_lowercase();
    let collapsed = norm.split_whitespace().collect::<Vec<_>>().join(" ");

    // Fork bomb (matched on the raw text: its parentheses and braces are the signature).
    if collapsed.contains(":(){") || collapsed.contains(":|:&") || collapsed.contains(": | : &") {
        return Some("fork bomb");
    }

    // Every piece of code the command runs: the command itself, then each substitution body,
    // `sh -c` / `su -c` / `eval` argument, here-document or here-string fed to a shell, text piped
    // into a shell, and quoted argument that a program might run. Each piece is checked clause by
    // clause. `lenient` marks a quoted argument of an ordinary program (a PR title, a `python -c`
    // script): its first clause is exempt from the power-state check, as the old guard's was.
    let mut code: Code = vec![(norm, false)];
    let mut pieces = 0;
    while let Some((text, lenient)) = code.pop() {
        pieces += 1;
        if pieces > MAX_PIECES {
            return Some("command too complex for the destructive guard to inspect");
        }
        let clauses = split_clauses(&text, &mut code);
        for (k, clause) in clauses.iter().enumerate() {
            let power = !(lenient && k == 0);
            if let Some(reason) = check_clause(clause, &clauses[..k], power, &mut code) {
                return Some(reason);
            }
        }
        if let Some(reason) = written_then_run(&clauses, depth) {
            return Some(reason);
        }
    }
    None
}

/// Queued code: (text, lenient). See `catastrophic_reason`.
type Code = Vec<(String, bool)>;

/// A bound on queued pieces; a command that exceeds it is refused rather than half-checked.
const MAX_PIECES: usize = 10_000;

/// A bound on files run from files (`echo '...' > a; sh a` inside the text written to another
/// file); a deeper write-then-run is refused rather than half-checked.
const MAX_DEPTH: usize = 4;

/// One shell word with its quoting removed. `bare` is true when the word starts unquoted, which
/// tells a redirection `>` or a `VAR=value` assignment apart from the same text in quotes.
struct Word {
    text: String,
    bare: bool,
}

/// One simple command: its words (redirection operators and their targets included), the bodies
/// of its here-documents, and whether a pipe feeds it from the clause before.
struct Clause {
    words: Vec<Word>,
    stdin: Vec<String>,
    piped_in: bool,
}

/// Programs that run the commands on their stdin: the shells, and the schedulers `at`, `batch` and
/// `crontab` (`echo '...' | at now`).
const STDIN_RUNNERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "fish", "csh", "tcsh", "at", "batch",
    "crontab",
];

/// Programs that run the commands in a file named by a later word (`sh x.sh`, `source x.sh`,
/// `. x.sh`, `at -f x.sh now`, `crontab x.sh`) or fed to them (`sh < x.sh`, `cat x.sh | sh`).
const FILE_RUNNERS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "mksh", "ash", "fish", "csh", "tcsh", "at", "batch",
    "crontab", "source", ".",
];

/// Programs that run the command named by a later word (`sudo rm`, `timeout 5 rm`, `su -c ...`).
const WRAPPERS: &[&str] = &[
    "sudo",
    "doas",
    "env",
    "exec",
    "nohup",
    "command",
    "builtin",
    "time",
    "nice",
    "ionice",
    "eval",
    "xargs",
    "busybox",
    "timeout",
    "setsid",
    "flock",
    "strace",
    "ltrace",
    "watch",
    "systemd-run",
    "unshare",
    "chroot",
    "script",
    "su",
    "runuser",
    "stdbuf",
    "taskset",
    "chrt",
    "nsenter",
    "caffeinate",
    "firejail",
    "sg",
    "pkexec",
    "proot",
    "fakeroot",
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "mksh",
    "ash",
    "fish",
    "csh",
    "tcsh",
];

/// Shell reserved words that may precede a command in the same clause (`then rm -rf /`).
const KEYWORDS: &[&str] = &[
    "!", "{", "if", "then", "elif", "else", "while", "until", "do",
];

/// Programs that only print or search their quoted arguments: quoted text there is data
/// (`git commit -m "..."`, `grep "rm -rf /" docs`, `echo "done & reboot"`).
const DATA: &[&str] = &["echo", "printf", "grep", "egrep", "fgrep", "rg", "git"];

/// Block devices, Linux and macOS.
const DISKS: &[&str] = &[
    "/dev/sd",
    "/dev/nvme",
    "/dev/hd",
    "/dev/vd",
    "/dev/xvd",
    "/dev/mmcblk",
    "/dev/disk",
    "/dev/rdisk",
    "/dev/md",
    "/dev/dm-",
    "/dev/mapper/",
];

const POWER_VERBS: &[&str] = &[
    "poweroff",
    "reboot",
    "halt",
    "kexec",
    "soft-reboot",
    "poweroff.target",
    "reboot.target",
    "halt.target",
    "kexec.target",
];

const DISKUTIL_ERASE: &[&str] = &[
    "erasedisk",
    "erasevolume",
    "reformat",
    "zerodisk",
    "randomdisk",
    "secureerase",
    "partitiondisk",
    "deletecontainer",
    "deletevolume",
    "deletepartition",
];

/// `find` tests that narrow which files a `-delete` reaches.
const NAME_TESTS: &[&str] = &[
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-regex",
    "-iregex",
    "-wholename",
    "-iwholename",
];

fn is_disk(target: &str) -> bool {
    DISKS.iter().any(|disk| target.starts_with(disk))
}

/// `mkfs`, `mkfs.ext4`, `mke2fs`, `mkswap`, `wipefs`, macOS `newfs_apfs`, ...
fn is_fs_wipe(name: &str) -> bool {
    name == "mkfs"
        || name.starts_with("mkfs.")
        || name.starts_with("mkfs_")
        || name == "newfs"
        || name.starts_with("newfs_")
        || matches!(
            name,
            "wipefs" | "mke2fs" | "mkswap" | "mkdosfs" | "mkntfs" | "mkexfatfs" | "mkudffs"
        )
}

/// The checks for one clause. `earlier` holds the clauses before it in the same piece of code
/// (what a pipe or `xargs` may be fed from); `power` enables the power-state check. Code this
/// clause hands to a shell or another program is queued on `code`.
fn check_clause(
    clause: &Clause,
    earlier: &[Clause],
    power: bool,
    code: &mut Code,
) -> Option<&'static str> {
    let words = &clause.words;
    let ops = operands(words);
    let (at, data, behind) = shape(&ops);
    // A dangerous word counts anywhere in the clause; quoted text is exempt only as an argument of
    // a printing/searching program.
    let live: Vec<usize> = (0..ops.len()).filter(|&i| ops[i].bare || !data).collect();

    // Filesystem creation / raw-disk writes.
    if live.iter().any(|&i| is_fs_wipe(basename(&ops[i].text))) {
        return Some("filesystem wipe (mkfs/wipefs)");
    }
    let redirect_to_disk = words
        .windows(2)
        .any(|w| is_redirect(&w[0]) && w[0].text.contains('>') && is_disk(&w[1].text));
    let dd_to_disk = live
        .iter()
        .any(|&i| ops[i].text.strip_prefix("of=").is_some_and(is_disk));
    if redirect_to_disk || dd_to_disk || runs_with(&ops, &live, "tee", is_disk) {
        return Some("raw write to a block device");
    }
    if runs_with(&ops, &live, "shred", |t| t.starts_with("/dev/")) {
        return Some("shredding a device");
    }
    if live.iter().any(|&i| is_named(ops[i], &["blkdiscard"])) {
        return Some("discarding a block device");
    }
    if runs_with(&ops, &live, "sgdisk", zaps)
        || runs_with(&ops, &live, "parted", |t| t == "mklabel")
        || runs_with(&ops, &live, "sfdisk", |t| t == "--delete")
    {
        return Some("partition table wipe");
    }
    if runs_with(&ops, &live, "diskutil", |t| DISKUTIL_ERASE.contains(&t)) {
        return Some("disk erase (diskutil)");
    }

    // System power state, at the command word or behind a wrapper (`sudo -u root reboot`).
    let cands = candidates(&ops, at, behind && !data);
    if power {
        for &i in &cands {
            let next = ops.get(i + 1).map(|w| w.text.as_str());
            let changes_power = match basename(&ops[i].text) {
                "shutdown" | "reboot" | "poweroff" | "halt" => true,
                "init" | "telinit" => matches!(next, Some("0" | "6")),
                "systemctl" | "loginctl" => after(&ops, i).any(|t| POWER_VERBS.contains(&t)),
                _ => false,
            };
            if changes_power {
                return Some("system power-state change");
            }
        }
    }

    // Catastrophic recursive delete of root/home: `rm` at any word, with the words after it.
    let rm_root = live.iter().any(|&i| {
        is_named(ops[i], &["rm"]) && is_catastrophic_rm(&after(&ops, i).collect::<Vec<_>>())
    });
    if rm_root {
        return Some("recursive force-delete of a system root");
    }
    for &i in &live {
        if !is_named(ops[i], &["find"]) {
            continue;
        }
        let rest: Vec<&str> = after(&ops, i).collect();
        let at_root = rest
            .iter()
            .take_while(|t| !t.starts_with('-') && !matches!(**t, "(" | "!"))
            .any(|t| !t.is_empty() && names_system_root(t));
        let deletes = rest
            .iter()
            .any(|t| *t == "-delete" || matches!(basename(t), "rm" | "shred" | "unlink"));
        let narrowed = rest.windows(2).any(|w| {
            let pattern = w[1].trim_matches(|ch: char| matches!(ch, '*' | '.' | '/'));
            NAME_TESTS.contains(&w[0]) && !pattern.is_empty()
        });
        if at_root && deletes && !narrowed {
            return Some("recursive force-delete of a system root");
        }
    }
    // `echo / | xargs rm -rf`: the targets come from the clauses before (or this clause's stdin).
    for &i in &live {
        if !is_named(ops[i], &["xargs"]) {
            continue;
        }
        let Some(r) = (i + 1..ops.len()).find(|&r| is_named(ops[r], &["rm"])) else {
            continue;
        };
        if !rm_is_recursive(&after(&ops, r).collect::<Vec<_>>()) {
            continue;
        }
        let fed: Vec<String> = earlier
            .iter()
            .flat_map(fed_text)
            .chain(stdin_of(clause))
            .collect();
        if fed
            .iter()
            .flat_map(|t| t.split_whitespace())
            .any(names_system_root)
        {
            return Some("recursive force-delete of a system root");
        }
    }
    for &i in &live {
        if !is_named(ops[i], &["chmod", "chown", "chgrp"]) {
            continue;
        }
        let args: Vec<&str> = after(&ops, i).collect();
        // `chmod -R` / `chown -R` use a capital R: reuse the rm rule, which accepts both cases.
        let recursive = rm_is_recursive(&args);
        let at_root = args
            .iter()
            .any(|t| !t.starts_with('-') && !t.is_empty() && names_system_root(t));
        if recursive && at_root {
            return Some("recursive permission change on a system root");
        }
    }

    // Code this clause runs or hands on.
    if !data {
        // A quoted argument that reads as a command line (`sh -c '...'`, `su -c '...'`,
        // `env -S '...'`, `python -c '...'`, `ssh host '...'`) is checked as code.
        for w in &ops {
            if w.text
                .contains(|ch: char| ch.is_whitespace() || ";&|()<>`".contains(ch))
            {
                code.push((w.text.clone(), !behind));
            }
        }
    } else if ops.get(at).is_some_and(|w| basename(&w.text) == "git") {
        // `git -c alias.x='!rm -rf /'` and `-c core.pager=...` run their value in a shell.
        for w in ops.windows(2).filter(|w| w[0].bare && w[0].text == "-c") {
            let value = w[1]
                .text
                .split_once('=')
                .map_or(w[1].text.as_str(), |(_, v)| v);
            code.push((value.trim_start_matches('!').to_string(), false));
        }
        // `git config core.pager '...'` and `git config alias.x '!...'` store a command that a
        // later git call runs. Every argument after `config` is checked as code: strictly (power
        // state included) for a `!` alias or a key whose value git runs, otherwise like any quoted
        // argument (`git config user.name "Halt Reboot"` stays allowed; `rm -rf /` never does).
        if let Some(s) = git_subcommand(&ops, at).filter(|&s| ops[s].text == "config") {
            let args: Vec<&str> = ops[s + 1..]
                .iter()
                .filter(|w| !(w.bare && w.text.starts_with('-')))
                .map(|w| w.text.as_str())
                .collect();
            let runs = args.iter().any(|t| git_key_runs(t));
            for t in args {
                code.push((
                    t.trim_start_matches('!').to_string(),
                    !(runs || t.starts_with('!')),
                ));
            }
        }
    }
    for &i in &cands {
        if is_named(ops[i], &["eval"]) {
            code.push((after(&ops, i).collect::<Vec<_>>().join(" "), false));
        }
    }
    // A shell (or `at`, `batch`, `crontab`) reading its stdin runs a here-document, a here-string,
    // or whatever is piped in.
    if cands.iter().any(|&i| is_named(ops[i], STDIN_RUNNERS)) {
        let crontab = cands.iter().any(|&i| is_named(ops[i], &["crontab"]));
        let mut fed = stdin_of(clause);
        if clause.piped_in {
            for e in earlier {
                let pieces = fed_text(e);
                fed.push(pieces.join(" "));
                fed.extend(pieces);
            }
        }
        for piece in fed {
            let mut texts = vec![piece.clone()];
            if piece.contains("\\n") {
                texts.push(piece.replace("\\n", "\n"));
            }
            for text in texts {
                if crontab {
                    // `0 0 * * * reboot`: the command follows five schedule fields (or `@daily`).
                    for line in text.lines() {
                        let schedule = if line.trim_start().starts_with('@') {
                            1
                        } else {
                            5
                        };
                        let command = line.split_whitespace().skip(schedule);
                        code.push((command.collect::<Vec<_>>().join(" "), false));
                    }
                }
                code.push((text, false));
            }
        }
    }
    None
}

/// The index of the command word, whether it is a printing/searching program (its quoted
/// arguments are data), and whether the clause starts with a wrapper (`sudo`, `timeout 5`, ...).
fn shape(ops: &[&Word]) -> (usize, bool, bool) {
    let at = command_start(ops);
    let first = ops
        .iter()
        .position(|w| !(is_assignment(w) || KEYWORDS.contains(&w.text.as_str())));
    let behind = first.is_some_and(|f| WRAPPERS.contains(&basename(&ops[f].text)));
    let data = ops
        .get(at)
        .is_some_and(|w| DATA.contains(&basename(&w.text)));
    (at, data, behind)
}

/// The index of git's subcommand: the first word after `git` that is neither an option nor the
/// value of `-c`, `-C`, `--git-dir`, `--work-tree`, `--namespace` or `--config-env`.
fn git_subcommand(ops: &[&Word], at: usize) -> Option<usize> {
    let mut j = at + 1;
    while let Some(w) = ops.get(j) {
        match w.text.as_str() {
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env" => j += 2,
            t if t.starts_with('-') => j += 1,
            _ => return Some(j),
        }
    }
    None
}

/// A `git config` key whose value git runs as a command (lowercase, as the guard sees it).
fn git_key_runs(key: &str) -> bool {
    matches!(
        key,
        "core.pager"
            | "core.editor"
            | "core.sshcommand"
            | "core.fsmonitor"
            | "core.askpass"
            | "core.gitproxy"
            | "diff.external"
            | "credential.helper"
            | "gpg.program"
            | "sequence.editor"
    ) || key.starts_with("pager.")
        || key.starts_with("filter.")
        || (key.starts_with("credential.") && key.ends_with(".helper"))
        || (key.starts_with("gpg.") && key.ends_with(".program"))
        || (key.starts_with("merge.") && key.ends_with(".driver"))
        || (key.starts_with("diff.") && (key.ends_with(".textconv") || key.ends_with(".command")))
}

/// `echo 'rm -rf /' > x.sh; sh x.sh`: text a clause writes to a file that a later clause of the
/// same code runs is code. The writer is not modelled program by program (`tee`, `dd of=`, `cp`,
/// `install`, `sponge`, `curl -o`, `sed -i`, ... is an open list): a clause may write every file it
/// names (see `writes`), and it writes catastrophic text when the text it has (see `text_of`) would
/// be refused, or when it names a file an earlier clause filled with such text (`cp a.sh b.sh`,
/// `cat a.sh > b.sh`, `dd if=a.sh of=b.sh`). Refuses when such a file is run by a later clause. A
/// file written but not run (`cat <<'EOF' > notes.md`) stays data.
fn written_then_run(clauses: &[Clause], depth: usize) -> Option<&'static str> {
    let named: Vec<Vec<&str>> = clauses.iter().map(names).collect();
    let mut tainted: Vec<&str> = Vec::new();
    for (k, clause) in clauses.iter().enumerate() {
        let files = writes(clause);
        // Only a file a later clause names can be run or copied on; skip the text check otherwise.
        let named_later = |f: &&str| named[k + 1..].iter().flatten().any(|n| same_file(n, f));
        if !files.iter().any(named_later) {
            continue;
        }
        let from_file = pipeline(clauses, k).any(|p| {
            named[p]
                .iter()
                .any(|n| tainted.iter().any(|t| same_file(n, t)))
        });
        if !from_file && !refused_text(&text_of(clauses, k), depth) {
            continue;
        }
        let run = |f: &&str| (k + 1..clauses.len()).any(|j| runs_file(clauses, j, k, f));
        if files.iter().any(run) {
            return Some("writes a catastrophic command to a file and runs it");
        }
        tainted.extend(files);
    }
    None
}

/// Programs that only print or search: they never write a file their arguments name, so their
/// arguments (`echo 'rm -rf /'`) are text, not files written.
const PRINTERS: &[&str] = &["echo", "printf", "grep", "egrep", "fgrep", "rg"];

/// The files a clause may write: every output redirection target (`> f`, `>> f`, `&> f`), and,
/// unless the program only prints, every word after the command word (`tee f`, `cp /dev/stdin f`,
/// `install -m755 /dev/stdin f`, `sponge f`, `sed -i ... f`, `curl -o f`), the value of a
/// `key=value` word (`dd of=f`, `--output=f`) and the value attached to a short flag (`-of`).
fn writes(clause: &Clause) -> Vec<&str> {
    let ops = operands(&clause.words);
    let (at, _, _) = shape(&ops);
    let mut files: Vec<&str> = clause
        .words
        .windows(2)
        .filter(|w| is_redirect(&w[0]) && w[0].text.contains('>'))
        .map(|w| w[1].text.as_str())
        .collect();
    if !ops.get(at).is_some_and(|w| is_named(w, PRINTERS)) {
        files.extend(ops.iter().skip(at + 1).copied().flat_map(file_names));
    }
    // `2>&1` names a file descriptor, not a file.
    files.retain(|f| {
        !f.chars().all(|ch| ch.is_ascii_digit()) && !matches!(basename(f), "" | "." | "..")
    });
    files
}

/// Every file a clause names, read or written: each word that is not a redirection operator, and
/// the values inside its words (`if=a.sh`, `-oa.sh`).
fn names(clause: &Clause) -> Vec<&str> {
    clause
        .words
        .iter()
        .filter(|w| !is_redirect(w))
        .flat_map(file_names)
        .collect()
}

/// The file names one word may carry: a flag carries the value after `=` (`--output=f`) or attached
/// to a short flag (`-of`); any other word is a name itself and carries the value after `=`
/// (`of=f`, quoted or not).
fn file_names(w: &Word) -> Vec<&str> {
    let t = w.text.as_str();
    let value = t.split_once('=').map(|(_, v)| v);
    if w.bare && t.starts_with("--") {
        value.into_iter().collect()
    } else if w.bare && t.starts_with('-') {
        let attached = t.char_indices().nth(2).map(|(i, _)| &t[i..]);
        attached.into_iter().chain(value).collect()
    } else {
        std::iter::once(t).chain(value).collect()
    }
}

/// Clause `k` and the clauses whose pipes feed it (`echo ... | dd of=x.sh`).
fn pipeline(clauses: &[Clause], k: usize) -> impl Iterator<Item = usize> {
    let mut p = k;
    while p > 0 && clauses[p].piped_in {
        p -= 1;
    }
    p..=k
}

/// The text clause `k` may write: its here-documents and here-strings, a printing program's
/// arguments, any other program's quoted arguments, and what its pipe feeds it.
fn text_of(clauses: &[Clause], k: usize) -> Vec<String> {
    let clause = &clauses[k];
    let ops = operands(&clause.words);
    let (at, data, _) = shape(&ops);
    let mut text = stdin_of(clause);
    text.extend(
        ops.iter()
            .skip(at + 1)
            .filter(|w| data || !w.bare)
            .map(|w| w.text.clone()),
    );
    for p in pipeline(clauses, k).filter(|&p| p != k) {
        text.extend(fed_text(&clauses[p]));
    }
    text
}

/// Whether clause `j` runs `file`, which clause `writer` (before it) wrote: as its program
/// (`./x.sh`, `exec x.sh`, `timeout 5 /tmp/x.sh`), or through a file runner that names or is fed
/// it (`sh x.sh`, `source x.sh`, `. x.sh`, `bash < x.sh`, `cat x.sh | sh`). Files match by their
/// last component.
fn runs_file(clauses: &[Clause], j: usize, writer: usize, file: &str) -> bool {
    let clause = &clauses[j];
    let ops = operands(&clause.words);
    let (at, data, behind) = shape(&ops);
    let cands = candidates(&ops, at, behind && !data);
    // Behind a wrapper the program follows the wrapper's options and numeric arguments
    // (`timeout 5 x.sh`, `nice -n 5 x.sh`, `sudo timeout 2s x.sh`), and may be a bare name found
    // on PATH (issue 457 item 2). A wrapper with a non-numeric positional argument
    // (`flock lockfile x.sh`, `su user x.sh`) is still a listed limit.
    let wrapped = if behind && !data {
        wrapped_program(&ops)
    } else {
        None
    };
    let program = |i: &&usize| **i == at || Some(**i) == wrapped || ops[**i].text.contains('/');
    if cands
        .iter()
        .filter(program)
        .any(|&i| same_file(&ops[i].text, file))
    {
        return true;
    }
    if !cands.iter().any(|&i| is_named(ops[i], FILE_RUNNERS)) {
        return false;
    }
    let mut p = j;
    loop {
        if reads(&clauses[p], file) {
            return true;
        }
        if !clauses[p].piped_in || p - 1 <= writer {
            return false;
        }
        p -= 1;
    }
}

/// The index of the program a wrapper-led clause runs: the first word after the leading wrappers
/// that is not an option, an assignment or a number or duration (`5`, `2s`, `0.5`, `-n`).
fn wrapped_program(ops: &[&Word]) -> Option<usize> {
    let numeric = |t: &str| {
        let digits = t.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
    };
    (0..ops.len()).find(|&i| {
        let text = ops[i].text.as_str();
        !(is_assignment(ops[i])
            || KEYWORDS.contains(&text)
            || WRAPPERS.contains(&basename(text))
            || text.starts_with('-')
            || numeric(text))
    })
}

/// Whether a clause names `file` other than as the target of an output redirection.
fn reads(clause: &Clause, file: &str) -> bool {
    let mut target = false;
    clause.words.iter().any(|w| {
        let written = target;
        target = is_redirect(w) && w.text.contains('>');
        !written && same_file(&w.text, file)
    })
}

fn same_file(word: &str, file: &str) -> bool {
    let name = basename(file);
    !name.is_empty() && basename(word) == name
}

/// Whether text written to a file that is then run would be refused as code: each piece, all of
/// them joined, and each with printf's `\n` read as a newline.
fn refused_text(text: &[String], depth: usize) -> bool {
    if text.iter().all(|t| t.trim().is_empty()) {
        return false;
    }
    if depth >= MAX_DEPTH {
        return true;
    }
    let joined = text.join(" ");
    std::iter::once(&joined).chain(text).any(|t| {
        reason(t, depth + 1).is_some()
            || (t.contains("\\n") && reason(&t.replace("\\n", "\n"), depth + 1).is_some())
    })
}

/// Whether a live word runs `name` with some later word satisfying `pred`.
fn runs_with(ops: &[&Word], live: &[usize], name: &str, pred: impl Fn(&str) -> bool) -> bool {
    live.iter()
        .any(|&i| is_named(ops[i], &[name]) && after(ops, i).any(&pred))
}

fn is_named(w: &Word, names: &[&str]) -> bool {
    names.contains(&basename(&w.text))
}

/// The words after `ops[i]`.
fn after<'a, 'b>(ops: &'b [&'a Word], i: usize) -> impl Iterator<Item = &'a str> + 'b {
    ops[i + 1..].iter().copied().map(|w| w.text.as_str())
}

/// An `sgdisk` option that destroys the partition table (`-Z`, `--zap-all`, `-o`, `--clear`).
fn zaps(t: &str) -> bool {
    matches!(t, "--zap-all" | "--zap" | "--clear")
        || (t.starts_with('-') && !t.starts_with("--") && t.contains(['z', 'o']))
}

/// The words of a clause that are not redirections: drops each operator and its target.
fn operands(words: &[Word]) -> Vec<&Word> {
    let mut out = Vec::new();
    let mut target = false;
    for w in words {
        if target {
            target = false;
        } else if is_redirect(w) {
            target = true;
        } else {
            out.push(w);
        }
    }
    out
}

/// A redirection operator the tokeniser emitted (`>`, `>>`, `2>&`, `&>`, `<`, `<<`, `<<<`, ...).
fn is_redirect(w: &Word) -> bool {
    w.bare
        && !w.text.is_empty()
        && w.text.chars().all(|ch| "<>&|".contains(ch))
        && w.text.contains(['<', '>'])
}

fn is_assignment(w: &Word) -> bool {
    w.bare && w.text.contains('=')
}

/// The here-documents and here-strings a clause reads as stdin.
fn stdin_of(clause: &Clause) -> Vec<String> {
    let strings = clause
        .words
        .windows(2)
        .filter(|w| w[0].bare && w[0].text == "<<<");
    strings
        .map(|w| w[1].text.clone())
        .chain(clause.stdin.iter().cloned())
        .collect()
}

/// What a clause may write into a pipe, as far as the guard can tell: its arguments and its stdin.
fn fed_text(clause: &Clause) -> Vec<String> {
    let ops = operands(&clause.words);
    let at = command_start(&ops);
    ops.iter()
        .skip(at + 1)
        .map(|w| w.text.clone())
        .chain(stdin_of(clause))
        .collect()
}

/// The words that may name the program a clause runs: the wrappers and the command word, every
/// later word when the clause starts with a wrapper (whose own option arguments are not parsed,
/// up to a printing/searching program), and the word after `find -exec`.
fn candidates(ops: &[&Word], at: usize, behind: bool) -> Vec<usize> {
    let mut out: Vec<usize> = (0..ops.len().min(at + 1)).collect();
    if behind {
        out.extend((at + 1..ops.len()).take_while(|&i| !DATA.contains(&basename(&ops[i].text))));
    }
    let exec = |i: &usize| {
        matches!(
            ops[*i - 1].text.as_str(),
            "-exec" | "-execdir" | "-ok" | "-okdir"
        )
    };
    out.extend((1..ops.len()).filter(exec));
    out
}

/// Every simple command `text` runs, as clauses split on unquoted `; & | ( )` and newlines.
/// Substitution bodies (`$(...)`, backticks, and those in unquoted here-documents) are queued on
/// `code`. `>&`, `<&`, `&>` and `>|` are redirections, not separators; `|&` is a pipe.
fn split_clauses(text: &str, code: &mut Code) -> Vec<Clause> {
    let c: Vec<char> = text.chars().collect();
    let mut b = Builder::default();
    let mut heredocs: Vec<(String, bool, usize)> = Vec::new(); // (delimiter, expands, clause index)
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => {
                i += 1;
                if let Some(&ch) = c.get(i).filter(|&&ch| ch != '\n') {
                    b.open(false).push(ch);
                }
            }
            '\'' => {
                let end = find(&c, i + 1, |ch| ch == '\'');
                b.open(false).extend(&c[i + 1..end]);
                i = end;
            }
            '"' => {
                let s = b.open(false);
                i += 1;
                while i < c.len() && c[i] != '"' {
                    match c[i] {
                        '\\' if matches!(c.get(i + 1), Some('"' | '\\' | '$' | '`')) => {
                            i += 1;
                            s.push(c[i]);
                        }
                        '$' if c.get(i + 1) == Some(&'(') => i = substitution(&c, i + 2, code),
                        '`' => i = backtick(&c, i + 1, code),
                        ch => s.push(ch),
                    }
                    i += 1;
                }
            }
            '$' if c.get(i + 1) == Some(&'(') => i = substitution(&c, i + 2, code),
            '`' => i = backtick(&c, i + 1, code),
            '#' if b.word.is_none() => i = find(&c, i, |ch| ch == '\n') - 1,
            ' ' | '\t' | '\r' => b.end_word(),
            '\n' => {
                b.end_clause();
                // A pending here-document's body runs to a line that is just its delimiter.
                for (delimiter, expands, at) in heredocs.drain(..) {
                    let mut body = String::new();
                    while i + 1 < c.len() {
                        let start = i + 1;
                        i = find(&c, start, |ch| ch == '\n');
                        let line: String = c[start..i].iter().collect();
                        if line.trim() == delimiter {
                            break;
                        }
                        if expands {
                            scan_substitutions(&c[start..i], code);
                        }
                        body.push_str(&line);
                        body.push('\n');
                    }
                    if let Some(clause) = b.clauses.get_mut(at) {
                        clause.stdin.push(body);
                    }
                }
            }
            '|' => {
                b.end_clause();
                match c.get(i + 1) {
                    Some('|') => i += 1, // `||`
                    Some('&') => {
                        i += 1; // `|&` pipes stdout and stderr
                        b.piped = true;
                    }
                    _ => b.piped = true,
                }
            }
            '&' if c.get(i + 1) == Some(&'>') => i = redirection(&c, i, &mut b),
            ';' | '&' | '(' | ')' => b.end_clause(),
            '<' if c.get(i + 1) == Some(&'<') && c.get(i + 2) == Some(&'<') => {
                b.op("<<<".to_string());
                i += 2;
            }
            '<' if c.get(i + 1) == Some(&'<') => {
                b.op("<<".to_string());
                i += 2;
                if c.get(i) == Some(&'-') {
                    i += 1;
                }
                while matches!(c.get(i), Some(' ' | '\t')) {
                    i += 1;
                }
                let end = find(&c, i, |ch| ch.is_whitespace() || ";&|()<>".contains(ch));
                let raw: String = c[i..end].iter().collect();
                let quoted = raw.contains(['\'', '"', '\\']);
                let delimiter = raw.replace(['\'', '"', '\\'], "");
                b.words.push(Word {
                    text: delimiter.clone(),
                    bare: false,
                });
                heredocs.push((delimiter, !quoted, b.clauses.len()));
                i = end - 1;
            }
            '<' | '>' => i = redirection(&c, i, &mut b),
            ch => b.open(true).push(ch),
        }
        i += 1;
    }
    b.end_clause();
    b.clauses
}

/// The clause being built.
#[derive(Default)]
struct Builder {
    clauses: Vec<Clause>,
    words: Vec<Word>,
    word: Option<Word>,
    /// The next clause pushed is fed by a pipe.
    piped: bool,
}

impl Builder {
    /// The word being built, started (as `bare` or quoted) if there is none yet.
    fn open(&mut self, bare: bool) -> &mut String {
        &mut self
            .word
            .get_or_insert_with(|| Word {
                text: String::new(),
                bare,
            })
            .text
    }

    fn end_word(&mut self) {
        self.words.extend(self.word.take());
    }

    fn end_clause(&mut self) {
        self.end_word();
        if !self.words.is_empty() {
            let words = std::mem::take(&mut self.words);
            self.clauses.push(Clause {
                words,
                stdin: Vec::new(),
                piped_in: self.piped,
            });
            self.piped = false;
        }
    }

    /// Ends the current word and adds the operator `text` as a bare word.
    fn op(&mut self, text: String) {
        self.end_word();
        self.words.push(Word { text, bare: true });
    }
}

/// Reads the redirection operator at `start` (`>`, `>>`, `>|`, `>&`, `<`, `<&`, `<>`, `&>`, `&>>`);
/// an unquoted all-digit word right before it is its file descriptor, not an argument. Returns
/// the index of the operator's last char.
fn redirection(c: &[char], start: usize, b: &mut Builder) -> usize {
    let digits = |w: &Word| w.bare && w.text.chars().all(|ch| ch.is_ascii_digit());
    let fd = b.word.as_ref().is_some_and(digits);
    if fd {
        b.word = None;
    }
    let mut i = start;
    if c[i] == '&' {
        i += 1;
    }
    i += 1; // the `<` or `>`
    while i < c.len() && i - start < 3 && matches!(c[i], '>' | '&' | '|') {
        i += 1;
    }
    b.op(c[start..i].iter().collect());
    i - 1
}

/// The index of the first char at or after `from` matching `pred`, or `c.len()`.
fn find(c: &[char], from: usize, pred: impl Fn(char) -> bool) -> usize {
    let from = from.min(c.len());
    c[from..]
        .iter()
        .position(|&ch| pred(ch))
        .map_or(c.len(), |p| from + p)
}

/// Queues the body of a `$(` substitution that starts at `start`; returns the index of its `)`.
fn substitution(c: &[char], start: usize, code: &mut Code) -> usize {
    let mut depth = 1;
    let mut i = start;
    while i < c.len() {
        match c[i] {
            '\\' => i += 1,
            '\'' => i = find(c, i + 1, |ch| ch == '\''),
            '"' => {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    i += if c[i] == '\\' { 2 } else { 1 };
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let end = i.min(c.len());
    code.push((c[start.min(end)..end].iter().collect(), false));
    end
}

/// Queues the body of a backtick substitution that starts at `start`; returns its closing index.
fn backtick(c: &[char], start: usize, code: &mut Code) -> usize {
    let mut i = start;
    while i < c.len() && c[i] != '`' {
        i += if c[i] == '\\' { 2 } else { 1 };
    }
    let end = i.min(c.len());
    code.push((c[start.min(end)..end].iter().collect(), false));
    end
}

/// Queues the substitutions in an unquoted-delimiter here-document body (the rest is data).
fn scan_substitutions(c: &[char], code: &mut Code) {
    let mut i = 0;
    while i < c.len() {
        match c[i] {
            '\\' => i += 1,
            '$' if c.get(i + 1) == Some(&'(') => i = substitution(c, i + 2, code),
            '`' => i = backtick(c, i + 1, code),
            _ => {}
        }
        i += 1;
    }
}

/// The last path component, so `/bin/rm` and `/sbin/reboot` match like `rm` and `reboot`.
fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// The index of the command word: skips wrappers (`sudo`, `env`, `sh -c`, ...), reserved words
/// (`then`, `!`, `{`), flags, and `VAR=value` assignments.
fn command_start(ops: &[&Word]) -> usize {
    ops.iter()
        .position(|w| {
            !(w.text.starts_with('-')
                || is_assignment(w)
                || WRAPPERS.contains(&basename(&w.text))
                || KEYWORDS.contains(&w.text.as_str()))
        })
        .unwrap_or(ops.len())
}

/// `/`, home, or a top-level system directory, with or without a trailing `/` or `/*`.
fn names_system_root(token: &str) -> bool {
    let t = token.trim_end_matches('/');
    let t = t.strip_suffix("/*").unwrap_or(t).trim_end_matches('/');
    matches!(
        t,
        "" | "~"
            | "$home"
            | "${home}"
            | "/bin"
            | "/boot"
            | "/dev"
            | "/etc"
            | "/home"
            | "/lib"
            | "/lib64"
            | "/opt"
            | "/proc"
            | "/root"
            | "/sbin"
            | "/srv"
            | "/sys"
            | "/usr"
            | "/var"
            | "/users"
            | "/system"
            | "/library"
            | "/applications"
    )
}

/// `-f` only silences prompts; a recursive delete of a root is catastrophic without it.
fn rm_is_recursive(args: &[&str]) -> bool {
    args.iter().any(|t| {
        *t == "--recursive"
            || (t.starts_with('-') && !t.starts_with("--") && t.contains(['r', 'R']))
    })
}

/// The arguments of an `rm`: recursive, and aimed at a system root.
fn is_catastrophic_rm(args: &[&str]) -> bool {
    if !rm_is_recursive(args) {
        return false;
    }
    if args.contains(&"--no-preserve-root") {
        return true;
    }
    args.iter()
        .filter(|t| !t.starts_with('-') && !t.is_empty())
        .any(|t| names_system_root(t))
}
