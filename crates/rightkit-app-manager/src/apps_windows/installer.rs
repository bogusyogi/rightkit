//! Installing a downloaded package on Windows: what it is, whether it is signed, a private
//! hash-bound copy to install from, the install and the undo, and the checks around them.
//!
//! This is the logic behind an "installer card": a host watches Downloads, asks `kind_of`
//! whether a finished file is an installer, `stage`s MSI and MSIX files, reads
//! `msi_facts` or `msix_facts`, and then `install_package`s, `remove_package`s or
//! `open_installed`s. The host owns the cards, the watcher and the timing.
//!
//! * `WinVerifyTrust` (`WINTRUST_ACTION_GENERIC_VERIFY_V2`, no UI, no revocation lookup) decides
//!   whether a signature is trusted (`is_trusted`); the signer's name is read from the
//!   certificate with PowerShell (`signer_name`).
//! * MSIX installs through `Add-AppxPackage`, MSI through `msiexec /i /qb /norestart`. Undo
//!   removes the package by its full name or product code.
//!
//! Staging (every MSI and MSIX): the download is copied once into an owner-only folder below the
//! host's state folder and hashed (`stage`). Every check and the install work on that one copy
//! and its hash. Immediately before the installer starts, the copy is opened with share mode
//! `FILE_SHARE_READ` only (no write, no delete) and hashed through that same handle
//! (`hold_staged`); the handle stays open until the installer process has ended, so the bytes
//! that were hashed are the bytes it reads and nobody can write to, replace or rename the file
//! meanwhile. If the copy cannot be made, held or matched, nothing installs. If `msiexec` or
//! `Add-AppxPackage` itself cannot open the file because of the share mode, the install fails
//! (MSI 1619 is reported as `InstallError::Changed`).
//! Threat boundary: code already running as the same Windows user is out of scope (it can run any
//! installer itself, or reopen the handle with other rights); staging defends against a download
//! swapped or still being written between the check and the install, not against a compromised
//! account. Staging folders are `<root>\<pid>-<nanos>`; `clean_stage_root` removes only
//! leftovers whose pid is not a running process (a failed check keeps the folder) and that are
//! plain, current-user-owned directories (never through a reparse point, never the root itself),
//! and logs any failure for a retry at the next start. Its plain-directory and owner checks are
//! not atomic against the removal: a same-user process could swap a directory for a junction
//! between check and delete, which is inside the same-user boundary above and not defended
//! against.
//!
//! Scripts run in Windows PowerShell with no window and receive values only as environment
//! variables, never inside the script text.

use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use rightkit_child_process::{OwnedCommand, read_bounded};

/// The most output kept from a PowerShell step (its answers are a few lines).
const OUTPUT_MAX: usize = 1024 * 1024;

type Logger = fn(&str, &[(&str, &str)]);

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Where this module reports what it could not do (event name and key/value fields). Only the
/// first call takes effect; returns `false` otherwise. Without one nothing is logged.
pub fn set_logger(logger: Logger) -> bool {
    LOGGER.set(logger).is_ok()
}

fn log(event: &str, fields: &[(&str, &str)]) {
    if let Some(logger) = LOGGER.get() {
        logger(event, fields);
    }
}

// ---- what was downloaded --------------------------------------------------------------------

/// What kind of installer a download is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackageKind {
    Msi,
    Msix,
    Exe,
}

impl PackageKind {
    /// `msi`, `msix` or `exe`, for logs.
    pub fn name(self) -> &'static str {
        match self {
            PackageKind::Msi => "msi",
            PackageKind::Msix => "msix",
            PackageKind::Exe => "exe",
        }
    }
}

/// The kind of installer a file name stands for: `.msi`; `.msix`, `.msixbundle`, `.appx`,
/// `.appxbundle`; a setup-looking `.exe` (its name has "setup" or "install" but not
/// "uninstall"). `None` for anything else.
pub fn kind_of(name: &str) -> Option<PackageKind> {
    let lower = name.to_ascii_lowercase();
    let (stem, extension) = lower.rsplit_once('.')?;
    match extension {
        "msi" => Some(PackageKind::Msi),
        "msix" | "msixbundle" | "appx" | "appxbundle" => Some(PackageKind::Msix),
        "exe" => {
            let looks_like_setup = stem.contains("setup") || stem.contains("install");
            (looks_like_setup && !stem.contains("uninstall")).then_some(PackageKind::Exe)
        }
        _ => None,
    }
}

/// "NotesPro-Setup-2.4.1" reads "NotesPro": words up to the first one that names the kind of
/// download, a platform or a version.
pub fn name_from_file(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let words: Vec<&str> = stem
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .take_while(|w| {
            let lower = w.to_ascii_lowercase();
            let numeric = lower
                .trim_start_matches('v')
                .starts_with(|c: char| c.is_ascii_digit());
            let platform = matches!(
                lower.as_str(),
                "setup"
                    | "install"
                    | "installer"
                    | "x64"
                    | "x86"
                    | "arm64"
                    | "win"
                    | "win32"
                    | "win64"
                    | "windows"
                    | "msi"
                    | "msix"
            );
            !numeric && !platform
        })
        .collect();
    if words.is_empty() {
        stem
    } else {
        words.join(" ")
    }
}

/// The first line of `text`, cut to `most` characters.
pub fn short(text: &str, most: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let line = line.trim();
    if line.chars().count() <= most {
        line.to_string()
    } else {
        let head: String = line.chars().take(most).collect();
        format!("{head}\u{2026}")
    }
}

// ---- watching a download settle -------------------------------------------------------------

/// What one look at a pending download found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Look {
    /// The file is gone or cannot be read.
    Gone,
    /// Its size differs from the last look (now this long).
    Grew(u64),
    /// The size is unchanged; `open` is true when nobody holds it open for any use.
    Same { len: u64, open: bool },
}

/// Looks at a download: its size against `known`, and whether it can be opened exclusively (share
/// mode 0, so a writer or a scanner holding it makes it "not open"). Call it with no lock held: a
/// file that is still being written, held by a scanner or on a stalled disk can make a metadata
/// call or an open wait for minutes (#351).
pub fn look(path: &Path, known: Option<u64>) -> Look {
    let Ok(meta) = std::fs::metadata(path) else {
        return Look::Gone;
    };
    let len = meta.len();
    if known != Some(len) {
        return Look::Grew(len);
    }
    let open = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(path)
        .is_ok();
    Look::Same { len, open }
}

// ---- staging --------------------------------------------------------------------------------

/// A private, owner-only copy of a download, bound to the SHA-256 of its bytes: the checks run on
/// this copy, it is hashed again immediately before the installer is started, and the installer
/// is started on it, so nothing placed at the download's path after the check can be what runs.
#[derive(Clone, Debug)]
pub struct Staged {
    pub dir: PathBuf,
    pub file: PathBuf,
    pub sha: String,
}

/// Copies `source` into a new owner-only folder below `root` (named `<pid>-<nanos>`) and hashes
/// the copy. Any failure (folder or file not provably owner-only, copy, hash) removes what was
/// made and returns `None`; the caller then asks instead of installing on its own.
pub fn stage(root: &Path, source: &Path) -> Option<Staged> {
    let name = source.file_name()?;
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    let dir = root.join(format!("{}-{unique}", std::process::id()));
    let built = (|| {
        rightkit_fs::private::ensure_private_dir(&dir).ok()?;
        let file = dir.join(name);
        let mut out = rightkit_fs::private::create_private_new(&file).ok()?;
        let mut input = std::fs::File::open(source).ok()?;
        std::io::copy(&mut input, &mut out).ok()?;
        out.sync_all().ok()?;
        drop(out);
        rightkit_fs::private::verify_owner_only(&dir).ok()?;
        rightkit_fs::private::verify_owner_only(&file).ok()?;
        let sha = rightkit_fs::digest::sha256_file(&file).ok()?;
        Some(Staged {
            dir: dir.clone(),
            file,
            sha,
        })
    })();
    if built.is_none()
        && let Err(error) = std::fs::remove_dir_all(&dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        // Left in place for the startup sweep (`clean_stage_root`), which retries it once
        // this process has ended.
        log(
            "installer_stage_cleanup_failed",
            &[
                ("what", "partial_stage"),
                ("reason", error.to_string().as_str()),
            ],
        );
    }
    built
}

/// Removes a staged copy (after the install, a cancel, or when its card is gone). A failure is
/// logged; the folder is retried at the next start (`clean_stage_root`).
pub fn discard_stage(staged: &Staged) {
    if let Err(error) = std::fs::remove_dir_all(&staged.dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log(
            "installer_stage_remove_failed",
            &[("reason", error.to_string().as_str())],
        );
    }
}

/// Opens the staged copy for reading with share mode `FILE_SHARE_READ` only (writers and
/// deleters are refused while it is open), without following a reparse point, and hashes it
/// through that same handle. Returns the handle, to be kept until the installer has ended, only
/// when the hash is the one the checks ran on.
pub fn hold_staged(staged: &Staged) -> Option<std::fs::File> {
    use std::io::{Seek, SeekFrom};
    const FILE_SHARE_READ: u32 = 1;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&staged.file)
        .ok()?;
    file.seek(SeekFrom::Start(0)).ok()?;
    let sha = rightkit_fs::digest::sha256_reader(&mut file).ok()?;
    (sha == staged.sha).then_some(file)
}

/// Startup cleanup of staging folders left by earlier runs. Only children of `root` named
/// `<pid>-<nanos>` that are plain directories (no symlink or reparse point), owned by the current
/// user, and not this process's own are removed. A root that is a reparse point or not
/// owner-only is left alone entirely. Every failure is logged and simply retried next start.
pub fn clean_stage_root(root: &Path) {
    use std::os::windows::fs::MetadataExt;
    const REPARSE: u32 = 0x400;
    let fail = |what: &str, why: String| {
        log(
            "installer_stage_cleanup_failed",
            &[("what", what), ("reason", why.as_str())],
        );
    };
    match std::fs::symlink_metadata(root) {
        Ok(meta)
            if meta.is_dir()
                && !meta.file_type().is_symlink()
                && meta.file_attributes() & REPARSE == 0 => {}
        Ok(_) => return fail("root", "not a plain directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => return fail("root", error.to_string()),
    }
    if let Err(error) = rightkit_fs::private::verify_owner_only(root) {
        return fail("root", error.to_string());
    }
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => return fail("root", error.to_string()),
    };
    let ours = std::process::id().to_string();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let generated = name.split_once('-').is_some_and(|(pid, nanos)| {
            !pid.is_empty()
                && !nanos.is_empty()
                && pid.bytes().all(|b| b.is_ascii_digit())
                && nanos.bytes().all(|b| b.is_ascii_digit())
                && pid != ours
        });
        if !generated {
            continue;
        }
        // Another process of the host may still own this stage: keep it unless its pid is
        // provably not running (a check that fails counts as running). A host that enforces one
        // instance per user has a live second instance only in the brief overlap while a newer
        // one takes over; this check covers that overlap, and pid reuse errs toward keeping.
        let owner_alive = name
            .split_once('-')
            .and_then(|(pid, _)| pid.parse::<u32>().ok())
            .is_none_or(sys::process_running);
        if owner_alive {
            continue;
        }
        let path = entry.path();
        let plain = std::fs::symlink_metadata(&path).is_ok_and(|meta| {
            meta.is_dir() && !meta.file_type().is_symlink() && meta.file_attributes() & REPARSE == 0
        });
        if !plain {
            fail("child", "not a plain directory".into());
            continue;
        }
        if let Err(error) = rightkit_fs::private::verify_owner_only(&path) {
            fail("child", error.to_string());
            continue;
        }
        if let Err(error) = std::fs::remove_dir_all(&path) {
            fail("child", error.to_string());
        }
    }
}

// ---- looking at a package -------------------------------------------------------------------

/// The file's Authenticode signature is trusted (`WinVerifyTrust`, generic verify, no UI, no
/// network revocation lookup).
pub fn is_trusted(path: &Path) -> bool {
    sys::verify_signature(path)
}

/// What a package says about itself and about the copy already installed.
#[derive(Clone, Debug, Default)]
pub struct PackageFacts {
    /// The package's own display name, when it has a usable one.
    pub name: Option<String>,
    pub version: String,
    /// The MSI product code or the MSIX package name.
    pub id: String,
    /// The version already installed, if any.
    pub installed: Option<String>,
    /// Where the installed copy lives, to see whether it is running.
    pub location: Option<String>,
    /// The new package's signer provably equals the installed copy's (MSI: the cached package's
    /// signer certificate; MSIX: the manifest publisher bound to the installed signature). False
    /// when unknown.
    pub same_signer: bool,
}

/// An MSI file's facts: product code, name, version, and the installed product (this very
/// product, else an older one sharing its upgrade code). `signed` is whether the package's own
/// signature is trusted (`is_trusted`); `None` when it is not a readable package.
pub fn msi_facts(work: &Path, signed: bool) -> Option<PackageFacts> {
    let meta = sys::msi_package(work)?;
    let mut facts = PackageFacts {
        name: (!meta.name.is_empty()).then_some(meta.name),
        version: meta.version,
        id: meta.product_code.clone(),
        ..PackageFacts::default()
    };
    // Installed already: this very product, else an older one sharing its upgrade code.
    let mut installed = meta.product_code;
    let mut version = sys::msi_product_info(&installed, "VersionString");
    if version.is_none()
        && let Some(related) = sys::msi_related_product(&meta.upgrade_code)
    {
        version = sys::msi_product_info(&related, "VersionString");
        installed = related;
    }
    if let Some(version) = version {
        facts.location = sys::msi_product_info(&installed, "InstallLocation");
        // Same signer = the same verified leaf certificate (SHA-256 of its raw bytes), for the new
        // package and for the package Windows cached for the installed copy, both with status
        // Valid. Display names are not identities. Rotation policy: a different certificate is
        // never replaced automatically, even from the same publisher; it asks.
        facts.same_signer = match (
            signed,
            signer_thumbprint(work),
            sys::msi_product_info(&installed, "LocalPackage")
                .and_then(|cached| signer_thumbprint(Path::new(&cached))),
        ) {
            (true, Some(new), Some(old)) => new == old,
            _ => false,
        };
        facts.installed = Some(version);
    }
    Some(facts)
}

/// An MSIX/AppX package's facts, read from its manifest and the installed package of the same
/// name. `None` when the script failed or its answer has an unexpected shape.
pub fn msix_facts(work: &Path) -> Option<PackageFacts> {
    const BODY: &str = "Add-Type -AssemblyName System.IO.Compression.FileSystem;\
        $z=[IO.Compression.ZipFile]::OpenRead($env:RK_PKG);\
        try{$e=$z.Entries|Where-Object{$_.FullName -eq 'AppxManifest.xml' -or \
        $_.FullName -eq 'AppxMetadata/AppxBundleManifest.xml'}|Select-Object -First 1;\
        $r=New-Object IO.StreamReader($e.Open());$x=[xml]$r.ReadToEnd();$r.Dispose()}\
        finally{$z.Dispose()};\
        $id=$x.DocumentElement.Identity;$dn='';if($x.Package){$dn=$x.Package.Properties.DisplayName};\
        $old=Get-AppxPackage -Name $id.Name|Select-Object -First 1;\
        $b='0';try{$n=[Security.Cryptography.X509Certificates.X500DistinguishedName];\
        $sg=Get-AuthenticodeSignature -LiteralPath $env:RK_PKG;\
        if($sg.Status -eq 'Valid' -and $sg.SignerCertificate -and $old -and \
        $old.InstallLocation -and $old.SignatureKind -and $old.SignatureKind.ToString() -ne 'None'){\
        $pub=(New-Object $n($id.Publisher)).Name;\
        if($pub -eq $sg.SignerCertificate.SubjectName.Name -and \
        $pub -eq (New-Object $n($old.Publisher)).Name){\
        Add-Type -AssemblyName System.Security;\
        $by=[IO.File]::ReadAllBytes((Join-Path $old.InstallLocation 'AppxSignature.p7x'));\
        if($by.Length -gt 4 -and $by[0] -eq 80 -and $by[1] -eq 75 -and $by[2] -eq 67 -and \
        $by[3] -eq 88){$cm=New-Object Security.Cryptography.Pkcs.SignedCms;\
        $cm.Decode([byte[]]$by[4..($by.Length-1)]);$cm.CheckSignature($true);\
        if($cm.SignerInfos.Count -eq 1 -and $cm.SignerInfos[0].Certificate){\
        $sh=[Security.Cryptography.SHA256]::Create();\
        $h1=[BitConverter]::ToString($sh.ComputeHash($sg.SignerCertificate.RawData));\
        $h2=[BitConverter]::ToString($sh.ComputeHash($cm.SignerInfos[0].Certificate.RawData));\
        if($h1 -eq $h2){$b='1'}}}}}}catch{};\
        [Console]::Out.Write(('{0}|{1}|{2}|{3}|{4}|{5}' -f $id.Name,$id.Version,$dn,$old.Version,\
        $old.InstallLocation,$b))";
    let path = work.to_string_lossy().into_owned();
    let out = powershell(BODY, &[("RK_PKG", path.as_str())]).filter(|o| o.code == 0)?;
    let parts: Vec<&str> = out.out.trim().split('|').collect();
    let [identity, version, display, old, location, bound] = parts.as_slice() else {
        return None;
    };
    let mut facts = PackageFacts {
        id: (*identity).to_string(),
        version: (*version).to_string(),
        // A display name that is a resource reference ("ms-resource:...") is not a name.
        name: (!display.is_empty() && !display.starts_with("ms-resource:"))
            .then(|| (*display).to_string()),
        ..PackageFacts::default()
    };
    if !old.is_empty() {
        facts.installed = Some((*old).to_string());
        facts.location = Some((*location).to_string()).filter(|l| !l.is_empty());
        // `bound`: the package's signature is Valid, its signer certificate subject equals the
        // manifest Identity.Publisher and the installed package's Publisher, and the SHA-256 of
        // its leaf certificate equals that of the installed package's own signing certificate
        // (read from AppxSignature.p7x in its install location, PKCX header stripped, CMS
        // signature checked). Unreadable installed identity, or a rotated certificate, is never
        // "same": it asks.
        facts.same_signer = *bound == "1";
    }
    Some(facts)
}

/// SHA-256 (upper-case hex) of the leaf signing certificate, only when the Authenticode
/// signature status is Valid; `None` for unsigned, invalid or unreadable.
fn signer_thumbprint(path: &Path) -> Option<String> {
    const BODY: &str = "$g=Get-AuthenticodeSignature -LiteralPath $env:RK_PKG;\
        if($g.Status -eq 'Valid' -and $g.SignerCertificate){$h=[Security.Cryptography.SHA256]::\
        Create().ComputeHash($g.SignerCertificate.RawData);\
        [Console]::Out.Write(([BitConverter]::ToString($h)).Replace('-',''))}";
    let path = path.to_string_lossy().into_owned();
    let out = powershell(BODY, &[("RK_PKG", path.as_str())])?;
    let hash = out.out.trim().to_string();
    (out.code == 0 && hash.len() == 64).then_some(hash)
}

/// The simple name of the file's signing certificate, at most 80 characters.
pub fn signer_name(path: &Path) -> Option<String> {
    const BODY: &str = "$s=(Get-AuthenticodeSignature -LiteralPath \
        $env:RK_PKG).SignerCertificate;if($s){[Console]::Out.Write($s.GetNameInfo(\
        [Security.Cryptography.X509Certificates.X509NameType]::SimpleName,$false))}";
    let path = path.to_string_lossy().into_owned();
    let out = powershell(BODY, &[("RK_PKG", path.as_str())])?;
    let name = short(&out.out, 80);
    (out.code == 0 && !name.is_empty()).then_some(name)
}

/// How many processes run from under `location`; with `quit`, asks them to close and waits up to
/// eight seconds first. `None` when the location or the check cannot be trusted.
pub fn running_count(location: &str, quit: bool) -> Option<u32> {
    running_processes(location, quit).map(|(count, _)| count)
}

/// Like [`running_count`], and also the distinct executables that were running before any were
/// asked to close, so they can be started again after an install. The location and each process
/// image path are made absolute (`GetFullPath`) and a process counts only when its path begins
/// with the location plus a path separator, so a sibling folder that merely shares the name
/// prefix never matches. A location that cannot be resolved, or is a drive root, yields `None`,
/// and so does any process whose path cannot be read or resolved unless it is provably unrelated
/// (see the script comment): unknown is never "not running".
pub fn running_processes(location: &str, quit: bool) -> Option<(u32, Vec<PathBuf>)> {
    // Fail closed: a failed process enumeration (Get-Process -ErrorAction Stop) is 'ERR', never an
    // empty list; a process whose image path cannot be read (Get-Process, then CIM) or
    // resolved makes the whole answer uncertain ('ERR'), never "not running". It is skipped only
    // on a positive test: PID 0 or 4, a process that has already exited, or a path-less kernel
    // pseudo-process whose parent is the System process (PID 4), which no user process can fake.
    const BODY: &str = "$l=$env:RK_LOC;$ok=$true;\
        try{if(-not [IO.Path]::IsPathRooted($l)){throw 'relative'};\
        $l=[IO.Path]::GetFullPath($l).TrimEnd('\\')+'\\'}catch{$ok=$false};\
        if((-not $ok) -or $l.Length -lt 4){[Console]::Out.Write('ERR')}else{\
        $c=@{};try{foreach($w in Get-CimInstance Win32_Process -ErrorAction Stop){\
        $c[[int]$w.ProcessId]=$w}}catch{};\
        $p=@();$x=@();$bad=$false;$all=@();\
        try{$all=@(Get-Process -ErrorAction Stop)}catch{$bad=$true};\
        foreach($q in $all){\
        if($q.Id -eq 0 -or $q.Id -eq 4){continue};\
        $f=$null;try{$f=$q.Path}catch{};\
        $w=$c[[int]$q.Id];if((-not $f) -and $w){$f=$w.ExecutablePath};\
        if(-not $f){$g=$false;try{$g=$q.HasExited}catch{};if($g){continue};\
        if($w -and $w.ParentProcessId -eq 4){continue};$bad=$true;continue};\
        try{$f=[IO.Path]::GetFullPath($f)}catch{$bad=$true;continue};\
        if($f.StartsWith($l,[StringComparison]::OrdinalIgnoreCase)){$p+=$q;$x+=$f}};\
        if($bad){[Console]::Out.Write('ERR')}else{\
        $x=@($x|Select-Object -Unique);\
        if($env:RK_MODE -eq 'quit'){foreach($q in $p){[void]$q.CloseMainWindow()};\
        for($i=0;$i -lt 16;$i++){Start-Sleep -Milliseconds 500;\
        $p=@($p|Where-Object{$e=$false;try{$e=$_.HasExited}catch{};-not $e});\
        if($p.Count -eq 0){break}}};\
        [Console]::Out.Write($p.Count);foreach($e in $x){[Console]::Out.Write(\"`n$e\")}}}";
    let mode = if quit { "quit" } else { "check" };
    let output =
        powershell(BODY, &[("RK_LOC", location), ("RK_MODE", mode)]).filter(|o| o.code == 0)?;
    let mut lines = output.out.lines();
    let count = lines.next()?.trim().parse().ok()?;
    let exes = lines
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect();
    Some((count, exes))
}

/// Starts each executable again, detached, so it outlives the host's own process.
pub fn relaunch_apps(exes: &[PathBuf]) {
    for exe in exes.iter().filter(|exe| exe.is_file()) {
        let mut command = OwnedCommand::new(exe);
        command
            .command_mut()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Detached (and not waited for): the app outlives this process.
        let _ = command.spawn_uncontained_detached();
    }
}

// ---- installing and undoing -----------------------------------------------------------------

/// Why an install did not happen.
#[derive(Debug)]
pub enum InstallError {
    /// The person cancelled the installer's own prompt.
    Cancelled,
    /// It failed; the text is for the person.
    Failed(String),
    /// The staged copy could not be read or no longer matches its hash, or the installer could
    /// not open it; nothing was started.
    Changed,
}

/// How long an install or removal script may run.
const POWERSHELL_INSTALL_WITHIN: Duration = Duration::from_secs(15 * 60);

/// Installs a staged MSI or MSIX package and returns what undoes it: the product code (MSI) or
/// the package full name (MSIX). Immediately before launch the staged copy is held open for
/// reading (no writer or deleter can get in) and hashed through that handle; the hold lasts until
/// this function returns, that is until the installer process has ended. No staged copy, no
/// install. `id` is the MSI product code or the MSIX package name from the package's facts.
/// A setup `.exe` is never run from here (`Failed`): it runs through its own window.
pub fn install_package(
    kind: PackageKind,
    staged: Option<&Staged>,
    id: &str,
) -> Result<String, InstallError> {
    let _hold = if kind == PackageKind::Exe {
        None
    } else {
        let Some(staged) = staged else {
            return Err(InstallError::Failed(
                "It could not be verified, so nothing was installed.".into(),
            ));
        };
        Some(hold_staged(staged).ok_or(InstallError::Changed)?)
    };
    match kind {
        PackageKind::Msi => {
            let work = staged.map(|s| s.file.clone()).unwrap_or_default();
            let mut msi = rightkit_child_process::command_std(system_file("System32\\msiexec.exe"));
            msi.arg("/i")
                .arg(&work)
                .args(["/qb", "/norestart"])
                .stdin(Stdio::null());
            let status =
                run_status(msi).map_err(|e| InstallError::Failed(short(&e.to_string(), 100)))?;
            match status.code() {
                Some(0 | 1641 | 3010) => {}
                Some(1602 | 1223) => return Err(InstallError::Cancelled),
                // The package could not be opened: possibly the share mode of the hold (to be
                // confirmed by a real install). Nothing ran; the download is asked about again.
                Some(1619) => return Err(InstallError::Changed),
                Some(code) => {
                    return Err(InstallError::Failed(format!(
                        "Windows Installer stopped with code {code}."
                    )));
                }
                None => {
                    return Err(InstallError::Failed(
                        "Windows Installer did not finish.".into(),
                    ));
                }
            }
            // Evidence, not the exit code alone: the product must now be registered.
            if sys::msi_product_info(id, "VersionString").is_none() {
                return Err(InstallError::Failed(
                    "It did not register with Windows.".into(),
                ));
            }
            Ok(id.to_string())
        }
        PackageKind::Msix => {
            const BODY: &str = "Add-AppxPackage -LiteralPath $env:RK_PKG;\
                $p=Get-AppxPackage -Name $env:RK_NAME|Sort-Object Version -Descending|\
                Select-Object -First 1;\
                if(-not $p){throw 'The package did not register.'};\
                [Console]::Out.Write($p.PackageFullName)";
            let work = staged.map(|s| s.file.clone()).unwrap_or_default();
            let path = work.to_string_lossy().into_owned();
            let env = [("RK_PKG", path.as_str()), ("RK_NAME", id)];
            let out = powershell_within(BODY, &env, POWERSHELL_INSTALL_WITHIN)
                .ok_or_else(|| InstallError::Failed("Windows PowerShell did not start.".into()))?;
            let full = out.out.trim().to_string();
            if out.code != 0 || full.is_empty() {
                let why = short(&out.err, 110);
                return Err(InstallError::Failed(if why.is_empty() {
                    "Windows could not install it.".into()
                } else {
                    why
                }));
            }
            Ok(full)
        }
        PackageKind::Exe => Err(InstallError::Failed(
            "A setup program is run by its own window.".into(),
        )),
    }
}

/// Removes what `install_package` installed (`target` is its return value). True when Windows
/// reports it removed.
pub fn remove_package(kind: PackageKind, target: &str) -> bool {
    match kind {
        PackageKind::Msi if sys::is_product_code(target) => {
            let mut msi = rightkit_child_process::command_std(system_file("System32\\msiexec.exe"));
            msi.arg("/x")
                .arg(target)
                .args(["/qb", "/norestart"])
                .stdin(Stdio::null());
            let status = run_status(msi);
            status.is_ok_and(|s| matches!(s.code(), Some(0 | 1641 | 3010)))
        }
        PackageKind::Msix => {
            const BODY: &str = "Remove-AppxPackage -Package $env:RK_FULL";
            let env = [("RK_FULL", target)];
            powershell_within(BODY, &env, POWERSHELL_INSTALL_WITHIN).is_some_and(|o| o.code == 0)
        }
        _ => false,
    }
}

/// Starts the program an install just made: an MSIX through its AppsFolder entry, an MSI through
/// its Start-menu shortcut named for the app, else the one executable in its install folder.
/// Nothing starts when none is found; true when something did.
pub fn open_installed(kind: PackageKind, target: &str, name: &str) -> bool {
    const MSIX_BODY: &str = "$p=Get-AppxPackage|Where-Object{$_.PackageFullName -eq $env:RK_FULL}|\
        Select-Object -First 1;\
        if(-not $p){exit 1};\
        $a=@((Get-AppxPackageManifest $p).Package.Applications.Application)[0];\
        if(-not $a){exit 1};\
        Start-Process ('shell:AppsFolder\\'+$p.PackageFamilyName+'!'+$a.Id)";
    const MSI_BODY: &str = "$roots=@([Environment]::GetFolderPath('Programs'),\
        [Environment]::GetFolderPath('CommonPrograms'))|Where-Object{$_ -and (Test-Path $_)};\
        $l=Get-ChildItem -LiteralPath $roots -Filter *.lnk -Recurse -ErrorAction SilentlyContinue|\
        Where-Object{$_.BaseName -like ('*'+$env:RK_NAME+'*')}|Select-Object -First 1;\
        if($l){Start-Process -FilePath $l.FullName;exit 0};\
        if($env:RK_LOC -and (Test-Path -LiteralPath $env:RK_LOC)){\
        $e=@(Get-ChildItem -LiteralPath $env:RK_LOC -Filter *.exe -File|\
        Where-Object{$_.Name -notmatch 'unins|uninstall|setup'});\
        if($e.Count -eq 1){Start-Process -FilePath $e[0].FullName;exit 0}};\
        exit 1";
    match kind {
        PackageKind::Msix => powershell(MSIX_BODY, &[("RK_FULL", target)]),
        PackageKind::Msi => {
            let location = sys::msi_product_info(target, "InstallLocation").unwrap_or_default();
            powershell(
                MSI_BODY,
                &[("RK_NAME", name), ("RK_LOC", location.as_str())],
            )
        }
        PackageKind::Exe => None,
    }
    .is_some_and(|o| o.code == 0)
}

/// Sends the downloaded file to the Recycle Bin (never deleted outright), then checks it is gone.
/// True when it is.
pub fn recycle_download(path: &Path) -> bool {
    const RECYCLE_BODY: &str = "Add-Type -AssemblyName Microsoft.VisualBasic;\
        [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($env:RK_PKG,\
        [Microsoft.VisualBasic.FileIO.UIOption]::OnlyErrorDialogs,\
        [Microsoft.VisualBasic.FileIO.RecycleOption]::SendToRecycleBin);\
        if(Test-Path -LiteralPath $env:RK_PKG){throw 'The download is still there.'}";
    let path = path.to_string_lossy().into_owned();
    powershell(RECYCLE_BODY, &[("RK_PKG", path.as_str())]).is_some_and(|o| o.code == 0)
}

// ---- processes ------------------------------------------------------------------------------

/// A file below `%SystemRoot%` (default `C:\Windows`).
pub fn system_file(relative: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from("C:\\Windows"));
    root.join(relative)
}

/// What a PowerShell step printed and exited with.
pub struct Output {
    pub code: i32,
    pub out: String,
    pub err: String,
}

const PS_HEAD: &str =
    "$ErrorActionPreference='Stop';[Console]::OutputEncoding=[Text.Encoding]::UTF8;try{";
const PS_TAIL: &str = "}catch{[Console]::Error.Write($_.Exception.Message);exit 1}";

/// Runs a fixed script in Windows PowerShell with no window. Values reach it only as environment
/// variables, never inside the script text.
pub fn powershell(body: &str, env: &[(&str, &str)]) -> Option<Output> {
    powershell_within(body, env, POWERSHELL_WITHIN)
}

/// How long a read or check script (signer, package details, running programs, recycle) may run.
/// A script that outlives it is ended with its tree and counts as failed: a read of a file that is
/// still downloading, locked by a scanner or on a stalled disk must not hold the thread that
/// asked (bogusyogi/pulse#351, #426).
const POWERSHELL_WITHIN: Duration = Duration::from_secs(90);

/// [`powershell`], ended after `within`: `None` when it did not finish in time or did not start.
pub fn powershell_within(body: &str, env: &[(&str, &str)], within: Duration) -> Option<Output> {
    let script = format!("{PS_HEAD}{body}{PS_TAIL}");
    let mut command = rightkit_child_process::command_std(system_file(
        "System32\\WindowsPowerShell\\v1.0\\powershell.exe",
    ));
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    // A child that ends with this process: no console window, and one that cannot join the host's
    // job still runs (the assignment is best effort). For a helper that starts nothing of the
    // user's.
    let mut owned = OwnedCommand::from_command(command);
    owned.job_assignment_mode(rightkit_child_process::JobAssignmentMode::BestEffort);
    let mut child = owned.spawn().ok()?;
    // Both pipes are read while the script runs, so a full pipe never holds it up.
    let out = capture(child.take_stdout());
    let err = capture(child.take_stderr());
    let code = match child.wait_then_kill(within) {
        Ok(rightkit_child_process::WaitOutcome::Exited(status)) => status.code().unwrap_or(-1),
        Ok(rightkit_child_process::WaitOutcome::Terminated(_)) => {
            log("installer_powershell_timeout", &[]);
            return None;
        }
        Err(_) => return None,
    };
    Some(Output {
        code,
        out: out.recv_timeout(PIPE_DRAIN).unwrap_or_default(),
        err: err.recv_timeout(PIPE_DRAIN).unwrap_or_default(),
    })
}

/// How long the pipes are read for after the script has exited. A program the script started can
/// keep a pipe's write end open for as long as it runs (an installer inheriting the handles), and
/// the read then never ends: the caller takes what was read and goes on, and the reader thread
/// ends with that program.
const PIPE_DRAIN: Duration = Duration::from_secs(5);

/// Reads a child's pipe to its end on a thread of its own, keeping at most `OUTPUT_MAX` bytes. The
/// text arrives on the channel when the pipe ends; a caller that stops waiting (`PIPE_DRAIN`)
/// leaves the thread to end by itself.
fn capture<R: Read + Send + 'static>(pipe: Option<R>) -> std::sync::mpsc::Receiver<String> {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let text = pipe
            .and_then(|pipe| read_bounded(pipe, OUTPUT_MAX).ok())
            .map(|captured| String::from_utf8_lossy(&captured.bytes).into_owned())
            .unwrap_or_default();
        let _ = send.send(text);
    });
    receive
}

/// Runs `command` to its end and returns its status. Detached, as an installer must outlive a
/// host that is ended while it runs.
fn run_status(command: Command) -> std::io::Result<ExitStatus> {
    OwnedCommand::from_command(command)
        .spawn_uncontained_detached()?
        .wait()
}

// ---- Win32 ----------------------------------------------------------------------------------

/// Raw declarations (kernel32, wintrust, msi) so no `windows` crate feature has to change; each
/// `unsafe` block says what it relies on.
mod sys {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    type Handle = *mut c_void;

    const WTD_UI_NONE: u32 = 2;
    const WTD_CHOICE_FILE: u32 = 1;
    const WTD_STATEACTION_VERIFY: u32 = 1;
    const WTD_STATEACTION_CLOSE: u32 = 2;
    const WTD_REVOCATION_CHECK_NONE: u32 = 0x10;
    const ERROR_SUCCESS: u32 = 0;
    const OPEN_PACKAGE_IGNORE_MACHINE_STATE: u32 = 1;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    /// WINTRUST_ACTION_GENERIC_VERIFY_V2.
    const GENERIC_VERIFY_V2: Guid = Guid {
        data1: 0x00AA_C56B,
        data2: 0xCD44,
        data3: 0x11D0,
        data4: [0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE],
    };

    #[repr(C)]
    struct WintrustFileInfo {
        size: u32,
        path: *const u16,
        file: Handle,
        known_subject: *const Guid,
    }

    #[repr(C)]
    struct WintrustData {
        size: u32,
        policy_data: *mut c_void,
        sip_data: *mut c_void,
        ui_choice: u32,
        revocation_checks: u32,
        union_choice: u32,
        file: *mut WintrustFileInfo,
        state_action: u32,
        state_data: Handle,
        url_reference: *const u16,
        prov_flags: u32,
        ui_context: u32,
        signature_settings: *mut c_void,
    }

    #[allow(non_snake_case)]
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn GetExitCodeProcess(process: Handle, code: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetLastError() -> u32;
    }

    /// Whether process `pid` is running. Fails closed: true unless the process provably does not
    /// exist (OpenProcess says ERROR_INVALID_PARAMETER) or has exited (exit code is not
    /// STILL_ACTIVE). Any other error (for example access denied) counts as running.
    pub fn process_running(pid: u32) -> bool {
        const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
        const ERROR_INVALID_PARAMETER: u32 = 87;
        const STILL_ACTIVE: u32 = 259;
        // SAFETY: OpenProcess returns null or a handle we close below; GetExitCodeProcess
        // writes one u32 through a valid pointer.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return GetLastError() != ERROR_INVALID_PARAMETER;
            }
            let mut code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok == 0 || code == STILL_ACTIVE
        }
    }

    #[allow(non_snake_case)]
    #[link(name = "wintrust")]
    unsafe extern "system" {
        fn WinVerifyTrust(window: Handle, action: *const Guid, data: *mut c_void) -> i32;
    }

    #[allow(non_snake_case)]
    #[link(name = "msi")]
    unsafe extern "system" {
        fn MsiOpenPackageExW(path: *const u16, options: u32, package: *mut u32) -> u32;
        fn MsiGetPropertyW(package: u32, name: *const u16, value: *mut u16, size: *mut u32) -> u32;
        fn MsiCloseHandle(handle: u32) -> u32;
        fn MsiGetProductInfoW(
            product: *const u16,
            property: *const u16,
            value: *mut u16,
            size: *mut u32,
        ) -> u32;
        fn MsiEnumRelatedProductsW(
            upgrade_code: *const u16,
            reserved: u32,
            index: u32,
            product: *mut u16,
        ) -> u32;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    // ---- Authenticode ----

    /// The file's signature is trusted (`WinVerifyTrust`, generic verify, no UI, no network
    /// revocation lookup).
    pub fn verify_signature(path: &Path) -> bool {
        let name = wide_path(path);
        let mut file = WintrustFileInfo {
            size: size_of::<WintrustFileInfo>() as u32,
            path: name.as_ptr(),
            file: std::ptr::null_mut(),
            known_subject: std::ptr::null(),
        };
        let mut data = WintrustData {
            size: size_of::<WintrustData>() as u32,
            policy_data: std::ptr::null_mut(),
            sip_data: std::ptr::null_mut(),
            ui_choice: WTD_UI_NONE,
            revocation_checks: 0,
            union_choice: WTD_CHOICE_FILE,
            file: &mut file,
            state_action: WTD_STATEACTION_VERIFY,
            state_data: std::ptr::null_mut(),
            url_reference: std::ptr::null(),
            prov_flags: WTD_REVOCATION_CHECK_NONE,
            ui_context: 0,
            signature_settings: std::ptr::null_mut(),
        };
        let invalid = usize::MAX as Handle;
        // SAFETY: `data` and `file` outlive both calls and have the documented layouts; the
        // second call releases the state the first one kept.
        let status = unsafe {
            WinVerifyTrust(
                invalid,
                &GENERIC_VERIFY_V2,
                (&raw mut data).cast::<c_void>(),
            )
        };
        data.state_action = WTD_STATEACTION_CLOSE;
        // SAFETY: see above.
        unsafe {
            WinVerifyTrust(
                invalid,
                &GENERIC_VERIFY_V2,
                (&raw mut data).cast::<c_void>(),
            );
        }
        status == 0
    }

    // ---- Windows Installer ----

    pub struct MsiPackage {
        pub product_code: String,
        pub upgrade_code: String,
        pub name: String,
        pub version: String,
    }

    fn from_buffer(buffer: &[u16], size: u32) -> Option<String> {
        let length = (size as usize).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..length]);
        (!text.is_empty()).then_some(text)
    }

    fn package_property(package: u32, name: &str) -> Option<String> {
        let key = wide(name);
        let mut buffer = vec![0u16; 512];
        let mut size = buffer.len() as u32;
        // SAFETY: NUL-terminated name; `size` is the buffer's length in characters.
        let result =
            unsafe { MsiGetPropertyW(package, key.as_ptr(), buffer.as_mut_ptr(), &mut size) };
        if result == ERROR_SUCCESS {
            from_buffer(&buffer, size)
        } else {
            None
        }
    }

    /// Reads the product code, upgrade code, name and version from an MSI file without
    /// installing it. `None` when it is not a readable package or has no product code.
    pub fn msi_package(path: &Path) -> Option<MsiPackage> {
        let name = wide_path(path);
        let mut package = 0u32;
        // SAFETY: NUL-terminated path; the handle is closed below.
        let result = unsafe {
            MsiOpenPackageExW(
                name.as_ptr(),
                OPEN_PACKAGE_IGNORE_MACHINE_STATE,
                &mut package,
            )
        };
        if result != ERROR_SUCCESS {
            return None;
        }
        let read = MsiPackage {
            product_code: package_property(package, "ProductCode").unwrap_or_default(),
            upgrade_code: package_property(package, "UpgradeCode").unwrap_or_default(),
            name: package_property(package, "ProductName").unwrap_or_default(),
            version: package_property(package, "ProductVersion").unwrap_or_default(),
        };
        // SAFETY: a handle from MsiOpenPackageExW, closed once.
        unsafe { MsiCloseHandle(package) };
        is_product_code(&read.product_code).then_some(read)
    }

    /// A property of an installed product ("VersionString", "InstallLocation"); `None` when the
    /// product is not installed or has no such value.
    pub fn msi_product_info(product_code: &str, property: &str) -> Option<String> {
        if !is_product_code(product_code) {
            return None;
        }
        let code = wide(product_code);
        let key = wide(property);
        let mut buffer = vec![0u16; 512];
        let mut size = buffer.len() as u32;
        // SAFETY: NUL-terminated strings; `size` is the buffer's length in characters.
        let result = unsafe {
            MsiGetProductInfoW(code.as_ptr(), key.as_ptr(), buffer.as_mut_ptr(), &mut size)
        };
        if result == ERROR_SUCCESS {
            from_buffer(&buffer, size)
        } else {
            None
        }
    }

    /// The first installed product that shares `upgrade_code`.
    pub fn msi_related_product(upgrade_code: &str) -> Option<String> {
        if !is_product_code(upgrade_code) {
            return None;
        }
        let code = wide(upgrade_code);
        let mut buffer = [0u16; 39];
        // SAFETY: NUL-terminated code; the product buffer holds the documented 39 characters.
        let result = unsafe { MsiEnumRelatedProductsW(code.as_ptr(), 0, 0, buffer.as_mut_ptr()) };
        if result == ERROR_SUCCESS {
            from_buffer(&buffer, 38)
        } else {
            None
        }
    }

    /// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}`: the only shape handed to msiexec or Windows
    /// Installer.
    pub fn is_product_code(text: &str) -> bool {
        let bytes = text.as_bytes();
        bytes.len() == 38
            && bytes[0] == b'{'
            && bytes[37] == b'}'
            && bytes[1..37].iter().enumerate().all(|(index, byte)| {
                if matches!(index, 8 | 13 | 18 | 23) {
                    *byte == b'-'
                } else {
                    byte.is_ascii_hexdigit()
                }
            })
    }
}
