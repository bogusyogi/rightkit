//! Path helpers with Node semantics: lexical `path.resolve`, `realpathSync` with a
//! lexical fallback, and `/`-separated relative paths.

use std::path::{Component, Path, PathBuf};

/// Node `path.resolve`: join onto the current directory and collapse `.` and `..`
/// without touching the filesystem.
pub fn resolve(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `fs.realpathSync`, without the Windows verbatim prefix.
pub fn real(path: &Path) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(path).map(strip_verbatim)
}

/// `realpathSync(p)` or `path.resolve(p)` when it fails.
pub fn real_or_self(path: &Path) -> PathBuf {
    real(path).unwrap_or_else(|_| resolve(path))
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest.to_string());
        }
    }
    path
}

/// `path.relative(from, to)` joined with `/`. `None` when no common base exists.
pub fn relative_posix(from: &Path, to: &Path) -> Option<String> {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    if common == 0 && (!from.is_empty() || !to.is_empty()) && from.first() != to.first() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    for _ in common..from.len() {
        parts.push("..".into());
    }
    for component in &to[common..] {
        parts.push(component.as_os_str().to_string_lossy().into_owned());
    }
    Some(parts.join("/"))
}

/// Node `error.code` for an I/O error.
pub fn io_code(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind::*;
    match error.kind() {
        NotFound => "ENOENT",
        PermissionDenied => "EACCES",
        NotADirectory => "ENOTDIR",
        _ => "EIO",
    }
}

/// `path.posix.normalize`.
pub fn posix_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".into();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|last| *last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let mut joined = out.join("/");
    if joined.is_empty() && !absolute {
        joined = ".".into();
    }
    if trailing && !joined.is_empty() && joined != "." {
        joined.push('/');
    }
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// `path.posix.join`.
pub fn posix_join(parts: &[&str]) -> String {
    let joined = parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("/");
    posix_normalize(&joined)
}

/// `path.posix.dirname`.
pub fn posix_dirname(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rfind('/') {
        None => {
            if path.starts_with('/') {
                "/".into()
            } else {
                ".".into()
            }
        }
        Some(0) => "/".into(),
        Some(at) => trimmed[..at].trim_end_matches('/').to_string(),
    }
}

/// `path.posix.basename`.
pub fn posix_basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// `path.posix.relative` for normalized relative paths.
pub fn posix_relative(from: &str, to: &str) -> String {
    let split = |p: &str| -> Vec<String> {
        posix_normalize(p)
            .split('/')
            .filter(|s| !s.is_empty() && *s != ".")
            .map(String::from)
            .collect()
    };
    let (a, b) = (split(from), split(to));
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let mut parts: Vec<String> = vec!["..".to_string(); a.len() - common];
    parts.extend(b[common..].iter().cloned());
    parts.join("/")
}
