use std::path::{Component, Path, PathBuf};

pub fn normalize_lossy(path: impl AsRef<str>) -> PathBuf {
    let path = path.as_ref().replace('\\', "/");
    let source = Path::new(&path);
    let mut out = PathBuf::new();
    for component in source.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
