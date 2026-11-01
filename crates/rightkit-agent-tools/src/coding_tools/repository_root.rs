//! Session-repository identity checks shared by Membrane-bound tools.

use std::path::{Path, PathBuf};

/// True when `session_root` is the caller's repository root, or a linked git
/// worktree of it (a session worktree under `<state-dir>/worktrees`). Both
/// inputs must already be canonical.
pub fn is_same_repository(session_root: &Path, caller_root: &Path) -> bool {
    let same = |left: &Path, right: &Path| {
        if cfg!(windows) {
            left.to_string_lossy()
                .eq_ignore_ascii_case(&right.to_string_lossy())
        } else {
            left == right
        }
    };
    same(session_root, caller_root)
        || linked_worktree_source(session_root)
            .and_then(|source| source.canonicalize().ok())
            .is_some_and(|source| same(&source, caller_root))
}

/// `<worktree>/.git` is a file `gitdir: <repo>/.git/worktrees/<name>`.
pub fn linked_worktree_source(workspace: &Path) -> Option<PathBuf> {
    let marker = std::fs::read_to_string(workspace.join(".git")).ok()?;
    let gitdir = PathBuf::from(marker.trim().strip_prefix("gitdir:")?.trim());
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    worktrees.parent()?.parent().map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_of_caller_root_is_the_same_repository() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let worktree = temp.path().join("wt");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        let repo = repo.canonicalize().unwrap();
        let worktree = worktree.canonicalize().unwrap();
        assert!(is_same_repository(&repo, &repo));
        assert!(is_same_repository(&worktree, &repo));
        assert!(!is_same_repository(&repo, &worktree));
    }
}
