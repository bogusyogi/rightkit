//! Gradle 9.2.1's uncontended cache lock opens wildcard UDP port 0.
//! JVM qualification admits UDP binding on all local addresses/ports.
//! This capability permits bind operations only. DenyNetwork is unchanged;
//! inbound, outbound, TCP & project writes remain denied, including descendants.
//! Native confinement controls must establish platform behavior before use is
//! counted as qualification; profile syntax alone proves no confinement.
use crate::{ExecutionReceipt, ExecutionState, SandboxAuthentication, SandboxGap};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// Private fields bind fresh owner-only cache storage, immutable byte closure &
/// fixed wrapper requests. A receipt string cannot construct this capability.
pub struct GradleBindOnlySandbox {
    project: PathBuf,
    cache: PathBuf,
    java: Option<PathBuf>,
    closure: BTreeMap<PathBuf, String>,
    #[cfg(target_os = "macos")]
    identity: (u64, u64),
    cleanup: bool,
}

const DIST: &str = "home/wrapper/dists/gradle-9.2.1-bin/2t0n5ozlw9xmuyvbp7dnzaxug/gradle-9.2.1";
/// Verbatim admission scope from workspace decisions.md, 2026-10-20 (#79).
pub const QUALIFICATION_ADMISSION: &str = r#"in src/gradle_sandbox.rs profile_text only, allow (network-bind (local udp "*:*")) for the JVM harness process tree; network-outbound and all inbound accept remain denied"#;
fn gap(detail: impl std::fmt::Display) -> SandboxGap {
    SandboxGap::IpcNamespaceInvalid(format!("gradle-bind-only: {detail}"))
}
#[cfg(target_os = "macos")]
fn hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

impl GradleBindOnlySandbox {
    pub fn path(&self) -> &Path {
        &self.cache
    }

    #[cfg(target_os = "macos")]
    pub fn create(parent: &Path, project: &Path) -> Result<Self, SandboxGap> {
        use std::{
            fs,
            os::unix::fs::{DirBuilderExt, MetadataExt},
        };
        let parent = super::sandbox::canonical_owned_path(parent)?;
        let project = super::sandbox::canonical_owned_path(project)?;
        let cache = parent.join("jvm-owned");
        if cache.starts_with(&project) || project.starts_with(&cache) {
            return Err(gap("cache/project overlap"));
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&cache)
            .map_err(gap)?;
        let meta = fs::symlink_metadata(&cache).map_err(gap)?;
        Ok(Self {
            project,
            cache,
            java: None,
            closure: BTreeMap::new(),
            identity: (meta.dev(), meta.ino()),
            cleanup: true,
        })
    }

    #[cfg(not(target_os = "macos"))]
    pub fn create(_parent: &Path, _project: &Path) -> Result<Self, SandboxGap> {
        Err(SandboxGap::UnsupportedPlatform)
    }

    /// Seal all JDK/compiler/distribution/project inputs after staging. Caller
    /// still owns official archive enrollment & complete closure enumeration.
    /// This owner verifies every supplied byte & admits only its fixed argv.
    pub fn seal(
        &mut self,
        java: &Path,
        closure: BTreeMap<PathBuf, String>,
    ) -> Result<(), SandboxGap> {
        if self.java.is_some() || closure.is_empty() {
            return Err(gap("already sealed or empty closure"));
        }
        if java.file_name().and_then(|s| s.to_str()) != Some("java") || !closure.contains_key(java)
        {
            return Err(gap("JDK identity absent"));
        }
        let wrapper = self.project.join("gradle/wrapper/gradle-wrapper.jar");
        if closure.get(&wrapper).map(String::as_str)
            != Some("sha256:423cb469ccc0ecc31f0e4e1c309976198ccb734cdcbb7029d4bda0f18f57e8d9")
        {
            return Err(gap("official wrapper identity differs"));
        }
        let agent = self
            .cache
            .join(DIST)
            .join("lib/agents/gradle-instrumentation-agent-9.2.1.jar");
        if !closure.contains_key(&agent) {
            return Err(gap("sealed instrumentation agent absent"));
        }
        self.java = Some(java.into());
        self.closure = closure;
        self.revalidate()
    }

    pub fn args(&self, task: &str) -> Result<Vec<String>, SandboxGap> {
        let target = match task {
            "wrapper" => None,
            "compile" => Some("legionCompile"),
            "lint" => Some("legionLint"),
            "test" => Some("legionTest"),
            "dependency" => Some("legionDependency"),
            _ => return Err(gap("unsupported task")),
        };
        let agent = self
            .cache
            .join(DIST)
            .join("lib/agents/gradle-instrumentation-agent-9.2.1.jar");
        let home = self.cache.join("home");
        let temp = self.cache.join("tmp");
        let daemon_args=format!("-Xmx512m -Xms256m -XX:MaxMetaspaceSize=384m -XX:+HeapDumpOnOutOfMemoryError -Dfile.encoding=UTF-8 -Duser.language=en -Duser.country=US -Duser.variant= -Djava.io.tmpdir={}",temp.display());
        let mut args = vec![
            "-Xmx512m".into(),
            "-Xms256m".into(),
            "-XX:MaxMetaspaceSize=384m".into(),
            "-XX:+HeapDumpOnOutOfMemoryError".into(),
            "-Dfile.encoding=UTF-8".into(),
            "-Duser.language=en".into(),
            "-Duser.country=US".into(),
            "-Duser.variant=".into(),
            format!("-Djava.io.tmpdir={}", temp.display()),
            format!("-javaagent:{}", agent.display()),
            format!("-Dgradle.user.home={}", home.display()),
            "-cp".into(),
            self.project
                .join("gradle/wrapper/gradle-wrapper.jar")
                .to_string_lossy()
                .into_owned(),
            "org.gradle.wrapper.GradleWrapperMain".into(),
            format!("-Dorg.gradle.jvmargs={daemon_args}"),
            "--offline".into(),
            "--no-daemon".into(),
            "--console=plain".into(),
            "--no-configuration-cache".into(),
            "--gradle-user-home".into(),
            home.to_string_lossy().into_owned(),
            "--project-cache-dir".into(),
            self.cache.join("project").to_string_lossy().into_owned(),
        ];
        if let Some(target) = target {
            args.extend(["--rerun-tasks".into(), target.into()]);
        } else {
            args.push("--version".into());
        }
        Ok(args)
    }

    #[cfg(target_os = "macos")]
    pub fn revalidate(&self) -> Result<(), SandboxGap> {
        use std::{fs, os::unix::fs::MetadataExt};
        super::sandbox::canonical_owned_path(&self.cache)?;
        let meta = fs::symlink_metadata(&self.cache).map_err(gap)?;
        if (meta.dev(), meta.ino()) != self.identity
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o7777 != 0o700
        {
            return Err(gap("owned cache replaced"));
        }
        if self.java.is_none() || self.closure.is_empty() {
            return Err(gap("unsealed request"));
        }
        for (path, expected) in &self.closure {
            if !path.is_absolute()
                || path.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::CurDir
                    )
                })
            {
                return Err(gap("noncanonical closure path"));
            }
            let meta = fs::symlink_metadata(path).map_err(gap)?;
            let actual = if let Some(link) = expected.strip_prefix("symlink:") {
                if !meta.file_type().is_symlink()
                    || fs::read_link(path).map_err(gap)?.to_string_lossy() != link
                {
                    return Err(gap("closure symlink changed"));
                }
                expected.clone()
            } else {
                if !meta.is_file() || meta.file_type().is_symlink() {
                    return Err(gap("closure file changed"));
                }
                hash(&fs::read(path).map_err(gap)?)
            };
            if &actual != expected {
                return Err(gap("immutable closure changed"));
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    pub fn revalidate(&self) -> Result<(), SandboxGap> {
        Err(SandboxGap::UnsupportedPlatform)
    }

    pub fn retain_if_process_tree_unconfirmed(&mut self, receipt: &ExecutionReceipt) -> bool {
        let failed = receipt.state == ExecutionState::KillFailed
            || receipt
                .gaps
                .iter()
                .any(|g| g == "platform_process_tree_unconfirmed")
            || receipt.process_tree.started
                && (!receipt.process_tree.terminated || !receipt.process_tree.reaped);
        if failed {
            self.cleanup = false;
        }
        failed
    }

    #[cfg(target_os = "macos")]
    pub fn authenticate(
        &self,
        task: &str,
        profile_dir: &Path,
    ) -> Result<SandboxAuthentication, SandboxGap> {
        self.revalidate()?;
        authenticate_inner(
            self.java.as_ref().ok_or_else(|| gap("unsealed"))?,
            &self.args(task)?,
            &self.project,
            profile_dir,
        )
    }
    #[cfg(not(target_os = "macos"))]
    pub fn authenticate(
        &self,
        _task: &str,
        _profile_dir: &Path,
    ) -> Result<SandboxAuthentication, SandboxGap> {
        Err(SandboxGap::UnsupportedPlatform)
    }
}

#[cfg(target_os = "macos")]
fn profile(project: &str) -> String {
    let project = project.replace('\\', "\\\\").replace('"', "\\\"");
    format!("(version 1)\n(allow default)\n(deny network*)\n(deny file-write* (subpath \"{project}\"))\n(allow file-write-data (require-all (subpath \"{project}\") (vnode-type DIRECTORY)))\n(allow network-bind (local udp \"*:*\"))\n")
}

#[cfg(target_os = "macos")]
fn authenticate_inner(
    executable: &Path,
    args: &[String],
    project: &Path,
    profile_dir: &Path,
) -> Result<SandboxAuthentication, SandboxGap> {
    use std::{fs, io::Write, os::unix::fs::OpenOptionsExt};
    if !Path::new("/usr/bin/sandbox-exec").is_file() {
        return Err(SandboxGap::ToolMissing);
    }
    let project = super::sandbox::canonical_owned_path(project)?;
    let project = project.to_str().ok_or_else(|| gap("non-UTF8 project"))?;
    let text = profile(project);
    let sha = hash(text.as_bytes());
    fs::create_dir_all(profile_dir).map_err(gap)?;
    super::sandbox::canonical_owned_path(profile_dir)?;
    let path = profile_dir.join(format!("audit-gradle-{}.sb", &sha[7..]));
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
    {
        Ok(mut file) => file.write_all(text.as_bytes()).map_err(gap)?,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if path.is_symlink() || fs::read(&path).map_err(gap)? != text.as_bytes() {
                return Err(gap("profile drift"));
            }
        }
        Err(e) => return Err(gap(e)),
    }
    let mut wrapped_args = vec![
        "-f".into(),
        path.to_string_lossy().into_owned(),
        "--".into(),
        executable.to_string_lossy().into_owned(),
    ];
    wrapped_args.extend_from_slice(args);
    Ok(SandboxAuthentication {
        id: format!("sandbox:macos:gradle-bind-only:{}", &sha[7..]),
        network: true,
        filesystem_scope: Some(project.into()),
        wrapped_executable: "/usr/bin/sandbox-exec".into(),
        wrapped_args,
    })
}

impl Drop for GradleBindOnlySandbox {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::MetadataExt;
            if self.cleanup
                && std::fs::symlink_metadata(&self.cache).is_ok_and(|m| {
                    !m.file_type().is_symlink()
                        && (m.dev(), m.ino()) == self.identity
                        && m.uid() == unsafe { libc::geteuid() }
                })
            {
                let _ = std::fs::remove_dir_all(&self.cache);
            }
        }
    }
}
