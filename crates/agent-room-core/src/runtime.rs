use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInfo {
    pub room_id: String,
    pub pid: u32,
    pub port: u16,
    pub base_url: String,
    pub database: PathBuf,
    pub pairing_code: String,
    pub human_bearer: String,
}

pub fn write_runtime(run_dir: &Path, info: &RuntimeInfo) -> io::Result<PathBuf> {
    fs::create_dir_all(run_dir)?;
    let path = run_dir.join("runtime.json");
    write_owner_file(
        &path,
        &serde_json::to_vec_pretty(info).map_err(io::Error::other)?,
    )?;
    Ok(path)
}

pub fn write_owner_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    if !bytes.ends_with(b"\n") {
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    restrict_to_owner(path)?;
    Ok(())
}

pub fn load_runtime(run_dir: &Path) -> io::Result<RuntimeInfo> {
    let bytes = fs::read(run_dir.join("runtime.json"))?;
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(windows)]
fn restrict_to_owner(path: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let username = std::env::var("USERNAME")
        .map_err(|_| io::Error::other("USERNAME is required to secure runtime.json"))?;
    let grant = format!("{username}:(F)");
    let status = std::process::Command::new("icacls.exe")
        .arg(path)
        .args(["/inheritance:r", "/grant:r", &grant])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("icacls failed"))
    }
}

#[cfg(unix)]
pub fn is_owner_restricted(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    Ok(fs::metadata(path)?.permissions().mode() & 0o077 == 0)
}

#[cfg(windows)]
pub fn is_owner_restricted(path: &Path) -> io::Result<bool> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let output = std::process::Command::new("icacls.exe")
        .arg(path)
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(std::process::Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("icacls inspection failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(!text.contains("(I)"))
}
