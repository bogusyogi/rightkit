// rightkit-video-project 0.1.0 consumed by exact registry pin, outside the workspace.
//
// Proves the project lifecycle a host relies on: a project is created in a directory, opened for
// writing, holds an empty timeline at the requested frame rate, saves its timeline and metadata,
// and opens again read-only; creating it twice in the same directory is idempotent.
// Does not prove: ingest and rendering (they need an editing FFmpeg), bundles, source integrity.
// State: a per-process directory under the OS temp directory, removed at the end.
use rightkit_timeline::Fps;
use rightkit_video_project::{Access, Project};

fn check(ok: bool, what: &str) {
    if !ok {
        eprintln!("FAIL: {what}");
        std::process::exit(1);
    }
    println!("ok: {what}");
}

fn main() {
    let root =
        std::env::temp_dir().join(format!("rk-video-project-consumer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp dir");
    let dir = root.join("proj");
    let fps = Fps { num: 30, den: 1 };
    check(
        Project::init(&dir, Some("demo"), fps).is_ok(),
        "a project is created",
    );
    check(
        Project::init(&dir, Some("demo"), fps).is_ok(),
        "creating it again in the same place is idempotent",
    );
    let project = Project::open(&dir, Access::Write).expect("open for writing");
    check(project.is_writable(), "it opens for writing");
    check(
        project.timeline().fps == fps && project.timeline().total_frames() == 0,
        "it holds an empty timeline at the requested rate",
    );
    check(
        project.save_timeline().is_ok() && project.save_meta().is_ok(),
        "it saves its timeline and metadata",
    );
    drop(project);
    let read = Project::open(&dir, Access::Read);
    check(
        matches!(&read, Ok(p) if !p.is_writable()),
        "it opens again read-only",
    );
    drop(read);
    let _ = std::fs::remove_dir_all(&root);
    println!("rightkit-video-project consumer: all checks passed");
}
