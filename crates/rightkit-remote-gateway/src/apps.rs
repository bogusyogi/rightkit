//! `apps.launch`: a remote caller opens an app by its id. Tier 2, its own switch
//! (`remoteLaunch`, off), needed in addition to `remoteTools`.
//!
//! * Any app opens except the ones on the `remoteBlockedApps` blocklist (empty by default) and the
//!   hard floor in `settings::hard_floor` (the host app itself, System Settings, security prompts).
//!   the owner, 2026-10-17: "the host app should be able to do anything ... we should have a block list,
//!   the opposite of an allow list". The id is a bundle id on macOS; on Windows an
//!   AppUserModelID, the full path of an `.exe`, or a bare exe name such as `spotify.exe`, which
//!   The host app resolves itself through the App Paths registry keys (`reg.exe query`, fixed arguments)
//!   and then a Start-menu shortcut of the same name, or refuses with `app_not_found`. What a bare
//!   name resolved to is checked against the hard floor and the blocklist again. No search, no
//!   arguments, no URL. A Windows `.lnk` is read, not run blind: its target is checked against the
//!   floor and the blocklist, and a shortcut that carries arguments, or whose target cannot be read, is
//!   refused (`app_blocked`).
//! * The local agent and the owner (`Origin::Local`) are never asked, per app or otherwise. The
//!   ChatGPT gateway (`Origin::Gateway`, a remote door) keeps its stricter rule: the first launch
//!   of each app asks the user once on this computer; the answer is a standing grant for that app
//!   on that door (family `launch:<app>`, ended after 30 days without use or by an engine
//!   restart). Turning the switch off or blocking the app ends the permission at once, because
//!   every call re-checks both.
//! * The program run is fixed: `/usr/bin/open -b <bundle id>` on macOS, `explorer.exe
//!   shell:AppsFolder\<id>` or the exe on Windows, started with no shell and no caller text beyond
//!   the id. The id is checked against a narrow character set first.
//!
//! Design note: this does not go through `rightkit-automation`'s `Engine::launch`, which starts an
//! app hidden and without activation. The Gate in `screen.rs` keeps denying `Launch` for
//! `screen.act`, so `screen.act` still cannot open anything.

use super::approvals;
use super::settings::{Settings, hard_floor};
use super::{Origin, Refusal, Result};
use serde_json::{Value, json};

/// The longest app id accepted. A grant family is `launch:` plus the id and is capped at 160.
const APP_MAX: usize = 140;

fn invalid(why: impl Into<String>) -> Refusal {
    Refusal::new("invalid_request", why)
}

/// `apps.launch`. `args`: `app` (an app id). For the gateway the first call for an app answers
/// `approval_required` and the call is repeated after the user approved it in the host app; the local
/// door opens the app at once.
pub fn launch(settings: &Settings, origin: Origin, args: &Value) -> Result<Value> {
    // Nothing types, launches or takes focus while the user holds the screen (spot takeover, #355).
    super::screen::user_in_control()?;
    settings.require_launch()?;
    let app = args["app"]
        .as_str()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            invalid("app is needed: the app's bundle id, AppUserModelID, exe name or exe path")
        })?;
    if app.len() > APP_MAX || app.chars().any(char::is_control) {
        return Err(invalid("app is an app id of at most 140 characters"));
    }
    let blocked = || {
        Refusal::new(
            "app_blocked",
            "that app is blocked: it is on the list of apps the app may not control (remoteBlockedApps) or the app never opens it",
        )
    };
    if hard_floor(app) || settings.app_blocked(app) {
        return Err(blocked());
    }
    // A bare exe name becomes the path the host app found for it, which is checked again: the name a
    // caller gives is not what runs.
    let target = imp::resolve(app)?;
    if hard_floor(&target) || settings.app_blocked(&target) {
        return Err(blocked());
    }
    if origin == Origin::Gateway {
        let family = format!("launch:{}", app.to_lowercase());
        if !approvals::session_active_held(origin, &family)? {
            return Err(approvals::request_session_held(
                origin,
                &family,
                &format!(
                    "allow {} to open the app {app}, now and whenever it asks, until you stop using it for 30 days",
                    origin.as_str()
                ),
            ));
        }
    }
    // A Windows shortcut runs something else: its target is checked against the same floor and
    // blocklist, and a shortcut that carries arguments (or whose target cannot be read) is refused.
    if let Some(inner) = imp::shortcut_target(&target)?
        && (hard_floor(&inner) || settings.app_blocked(&inner))
    {
        return Err(blocked());
    }
    imp::open(&target)?;
    Ok(json!({"launched": true, "app": app}))
}

#[cfg(not(any(target_os = "macos", windows)))]
mod imp {
    use super::*;

    pub fn resolve(id: &str) -> Result<String> {
        Ok(id.to_string())
    }

    pub fn shortcut_target(_: &str) -> Result<Option<String>> {
        Ok(None)
    }

    pub fn open(_: &str) -> Result<()> {
        Err(Refusal::new(
            "unsupported_platform",
            "apps.launch runs on macOS and Windows only",
        ))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::*;

    /// A reverse-DNS bundle id: letters, digits, `.`, `-`, starting with a letter or digit, so it
    /// can never read as an option to `open`.
    fn bundle_id(id: &str) -> bool {
        id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
            && id.contains('.')
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
    }

    /// A bundle id is run as it is.
    pub fn resolve(id: &str) -> Result<String> {
        Ok(id.to_string())
    }

    /// No shortcuts on a Mac.
    pub fn shortcut_target(_: &str) -> Result<Option<String>> {
        Ok(None)
    }

    pub fn open(id: &str) -> Result<()> {
        if !bundle_id(id) {
            return Err(invalid("app is not a bundle id such as com.spotify.client"));
        }
        // `launch_app` reports a launcher that exited with a failure as `ErrorKind::Other`
        // (CP/src/launch.rs `hand_off`); anything else is a launcher that did not start.
        rightkit_child_process::launch_app(id).map_err(|e| {
            if e.kind() == std::io::ErrorKind::Other {
                Refusal::new("app_not_found", "no installed app has that bundle id")
            } else {
                Refusal::new("spawn_failed", "open could not be started")
            }
        })
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use rightkit_child_process::{OwnedCommand, RunOptions};
    use std::path::Path;
    use std::process::Stdio;
    use std::time::Duration;

    /// An AppUserModelID: letters, digits and `. _ ! - { } \` and spaces, nothing a shell or the
    /// Shell's `shell:` syntax reads as something else.
    fn aumid(id: &str) -> bool {
        !id.is_empty()
            && !id.starts_with('-')
            && id.chars().all(|c| {
                c.is_ascii_alphanumeric()
                    || matches!(c, '.' | '_' | '!' | '-' | '{' | '}' | '\\' | ' ')
            })
    }

    /// A bare exe name: letters, digits, `.`, `_`, `-`, ` ` and `+`, ending in `.exe`, with no
    /// path separator, so it can only name a registry key or a shortcut, never a place.
    fn bare_exe_name(id: &str) -> bool {
        let lower = id.to_ascii_lowercase();
        lower.ends_with(".exe")
            && lower.len() > 4
            && !id.starts_with(['.', '-', ' '])
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ' | '+'))
    }

    /// The text of `%NAME%` references replaced from the environment; an unknown name is left.
    fn expand_env(text: &str) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find('%') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            match after.find('%') {
                Some(end) => match std::env::var(&after[..end]) {
                    Ok(value) if !after[..end].is_empty() => {
                        out.push_str(&value);
                        rest = &after[end + 1..];
                    }
                    _ => {
                        out.push('%');
                        rest = after;
                    }
                },
                None => {
                    out.push('%');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// The default value of `HKCU` or `HKLM` `...\App Paths\<name>` for a bare exe name, through
    /// `reg.exe` with fixed arguments (the name passed `bare_exe_name`). The program it names must
    /// be an existing absolute `.exe`.
    fn app_paths(name: &str) -> Option<String> {
        const KEYS: [&str; 3] = [
            r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths",
            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths",
            r"HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths",
        ];
        let system = std::env::var_os("SystemRoot")?;
        let reg = Path::new(&system).join("System32").join("reg.exe");
        for key in KEYS {
            let options = RunOptions::new(Duration::from_secs(5))
                .stdout_cap(usize::MAX)
                .stderr_cap(0);
            let done = rightkit_child_process::run(
                &reg,
                [
                    "query".to_string(),
                    format!(r"{key}\{name}"),
                    "/ve".to_string(),
                ],
                &options,
            )
            .ok()
            .filter(|done| !done.timed_out)?;
            if !done.status.success() {
                continue;
            }
            let text = String::from_utf8_lossy(&done.stdout.bytes);
            for line in text.lines() {
                let Some((_, value)) = line
                    .split_once("REG_SZ")
                    .or_else(|| line.split_once("REG_EXPAND_SZ"))
                else {
                    continue;
                };
                let path = expand_env(value.trim().trim_matches('"'));
                let candidate = Path::new(&path);
                if path.to_ascii_lowercase().ends_with(".exe")
                    && candidate.is_absolute()
                    && candidate.is_file()
                {
                    return Some(path);
                }
            }
        }
        None
    }

    /// The Start-menu folders of this user and of everyone.
    fn start_menu_roots() -> Vec<std::path::PathBuf> {
        let rest = Path::new(r"Microsoft\Windows\Start Menu\Programs");
        ["APPDATA", "ProgramData"]
            .iter()
            .filter_map(std::env::var_os)
            .map(|base| Path::new(&base).join(rest))
            .collect()
    }

    /// A Start-menu shortcut named like the exe (`Spotify.lnk` for `spotify.exe`), searched three
    /// folders deep.
    fn start_menu_shortcut(name: &str) -> Option<String> {
        let wanted = format!("{}.lnk", &name[..name.len() - 4]).to_lowercase();
        fn walk(dir: &Path, wanted: &str, depth: usize) -> Option<std::path::PathBuf> {
            for entry in std::fs::read_dir(dir).ok()?.flatten() {
                let path = entry.path();
                let kind = entry.file_type().ok()?;
                if kind.is_file() && entry.file_name().to_string_lossy().to_lowercase() == wanted {
                    return Some(path);
                }
                if kind.is_dir()
                    && depth < 3
                    && let Some(found) = walk(&path, wanted, depth + 1)
                {
                    return Some(found);
                }
            }
            None
        }
        start_menu_roots()
            .iter()
            .find_map(|root| walk(root, &wanted, 0))
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// What to run for `id`: a bare exe name becomes the full path App Paths gives it, or the
    /// Start-menu shortcut of that name; anything else stays as it is. A bare name nothing
    /// resolves is `app_not_found`.
    pub fn resolve(id: &str) -> Result<String> {
        if !bare_exe_name(id) {
            return Ok(id.to_string());
        }
        app_paths(id)
            .or_else(|| start_menu_shortcut(id))
            .ok_or_else(|| {
                Refusal::new(
                    "app_not_found",
                    "The app found no installed program with that exe name (no App Paths entry, no Start-menu shortcut); give its AppUserModelID or the full path of its exe",
                )
            })
    }

    /// Reads the target of a `.lnk` (MS-SHLLINK) without running it: the `LocalBasePath` and
    /// `CommonPathSuffix` of its LinkInfo, and whether its command-line arguments are non-empty.
    /// `None` when the file is not a readable shortcut.
    fn parse_lnk(bytes: &[u8]) -> Option<(Option<String>, bool)> {
        let u16_at = |at: usize| -> Option<usize> {
            Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?) as usize)
        };
        let u32_at = |at: usize| -> Option<usize> {
            Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize)
        };
        if u32_at(0)? != 0x4C {
            return None;
        }
        let flags = u32_at(0x14)?;
        let mut at = 0x4C;
        if flags & 0x1 != 0 {
            at += 2 + u16_at(at)?;
        }
        let mut target = None;
        if flags & 0x2 != 0 {
            let base = at;
            let size = u32_at(base)?;
            let header = u32_at(base + 4)?;
            let info_flags = u32_at(base + 8)?;
            let text = |offset: usize, wide: bool| -> Option<String> {
                let start = base.checked_add(offset)?;
                let rest = bytes.get(start..base.checked_add(size)?)?;
                if wide {
                    let units: Vec<u16> = rest
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| u16::from_le_bytes(*c))
                        .take_while(|u| *u != 0)
                        .collect();
                    Some(String::from_utf16_lossy(&units))
                } else {
                    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
                    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
                }
            };
            if info_flags & 0x1 != 0 {
                let (prefix, suffix) = if header >= 0x24 && u32_at(base + 28)? != 0 {
                    (
                        text(u32_at(base + 28)?, true)?,
                        text(u32_at(base + 32)?, true).unwrap_or_default(),
                    )
                } else {
                    (
                        text(u32_at(base + 16)?, false)?,
                        text(u32_at(base + 24)?, false).unwrap_or_default(),
                    )
                };
                target = Some(format!("{prefix}{suffix}"));
            }
            at = base.checked_add(size)?;
        }
        // StringData: name, relative path, working dir, arguments, icon, each a u16 count and text.
        let unicode = flags & 0x80 != 0;
        let mut has_args = false;
        for (bit, is_args) in [
            (0x4, false),
            (0x8, false),
            (0x10, false),
            (0x20, true),
            (0x40, false),
        ] {
            if flags & bit == 0 {
                continue;
            }
            let count = u16_at(at)?;
            if is_args && count > 0 {
                has_args = true;
            }
            at += 2 + count * if unicode { 2 } else { 1 };
        }
        Some((target, has_args))
    }

    /// For a `.lnk` entry: its target path, after refusing a shortcut that carries arguments or whose
    /// target cannot be read (a store-app shortcut: use its AppUserModelID). `None` for anything else.
    pub fn shortcut_target(id: &str) -> Result<Option<String>> {
        if !id.to_ascii_lowercase().ends_with(".lnk") {
            return Ok(None);
        }
        let refused = |why: &str| Refusal::new("app_blocked", why.to_string());
        let bytes = std::fs::read(id).map_err(|_| {
            Refusal::new(
                "app_not_found",
                "the shortcut could not be read".to_string(),
            )
        })?;
        if bytes.len() > 1024 * 1024 {
            return Err(refused("that shortcut is not a plain shortcut"));
        }
        match parse_lnk(&bytes) {
            Some((_, true)) => Err(refused(
                "a shortcut that carries arguments is not opened; open the program itself",
            )),
            Some((Some(info_target), false)) if !info_target.is_empty() => {
                // The LinkInfo path above is only one of the places a shortcut names its target;
                // Windows follows its LinkTargetIDList (and may use the environment-variable
                // block). The target is therefore also read the way Windows reads it
                // (IShellLinkW.GetPath), and a shortcut whose two readings differ is refused, so a
                // harmless LinkInfo cannot hide a blocked program.
                let Some((shell_target, shell_args)) = shell_link(id) else {
                    return Err(refused(
                        "The app could not read that shortcut the way Windows does, so it does not open it; use the app's AppUserModelID or exe path",
                    ));
                };
                if !shell_args.trim().is_empty() {
                    return Err(refused(
                        "a shortcut that carries arguments is not opened; open the program itself",
                    ));
                }
                if same_path(&shell_target, &info_target) {
                    Ok(Some(shell_target))
                } else {
                    Err(refused(
                        "that shortcut names two different programs, so it is not opened; open the program itself",
                    ))
                }
            }
            _ => Err(refused(
                "The app could not read where that shortcut leads, so it does not open it; use the app's AppUserModelID or exe path",
            )),
        }
    }

    /// Two paths compared as Windows would: environment references expanded, `/` as `\`, no case.
    fn same_path(a: &str, b: &str) -> bool {
        let norm = |s: &str| expand_env(s.trim()).replace('/', "\\").to_lowercase();
        let (a, b) = (norm(a), norm(b));
        !a.is_empty() && a == b
    }

    /// The target and the arguments Windows itself reads from a shortcut: `IShellLinkW` over the
    /// file, loaded read-only, never resolved and never run. COM runs on a thread of its own so the
    /// caller's COM state is left alone. `None` when the shortcut cannot be read this way.
    fn shell_link(path: &str) -> Option<(String, String)> {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
            CoUninitialize, IPersistFile, STGM_READ,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::{IUnknown, Interface, PCWSTR};

        fn text(buffer: &[u16]) -> String {
            let end = buffer.iter().position(|u| *u == 0).unwrap_or(buffer.len());
            String::from_utf16_lossy(&buffer[..end])
        }
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        std::thread::spawn(move || {
            let initialised = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
            let read = || -> windows::core::Result<(String, String)> {
                unsafe {
                    let link: IShellLinkW =
                        CoCreateInstance(&ShellLink, None::<&IUnknown>, CLSCTX_INPROC_SERVER)?;
                    let file: IPersistFile = link.cast()?;
                    file.Load(PCWSTR(wide.as_ptr()), STGM_READ)?;
                    let mut target = [0u16; 2048];
                    link.GetPath(&mut target, std::ptr::null_mut(), 0)?;
                    let mut args = [0u16; 2048];
                    link.GetArguments(&mut args)?;
                    Ok((text(&target), text(&args)))
                }
            };
            let read = read().ok().filter(|(target, _)| !target.is_empty());
            if initialised {
                unsafe { CoUninitialize() };
            }
            read
        })
        .join()
        .ok()
        .flatten()
    }

    /// True when `path` is a `.lnk` inside one of the Start-menu folders.
    fn in_start_menu(path: &Path) -> bool {
        let Ok(path) = path.canonicalize() else {
            return false;
        };
        start_menu_roots()
            .iter()
            .any(|root| root.canonicalize().is_ok_and(|root| path.starts_with(root)))
    }

    pub fn open(id: &str) -> Result<()> {
        // Detached launches through `OwnedCommand`: `launch_app` refuses the AppUserModelID shapes
        // `aumid` above accepts (braces, backslash, spaces; CP/src/launch.rs `valid_app_user_model_id`),
        // and `reveal::open` refuses `.lnk` and `.exe`.
        let mut command;
        if id.to_ascii_lowercase().ends_with(".lnk") {
            let path = Path::new(id);
            if !path.is_absolute() || !path.is_file() || !in_start_menu(path) {
                return Err(Refusal::new(
                    "app_not_found",
                    "a shortcut is opened only from the Start menu",
                ));
            }
            command = OwnedCommand::new("explorer.exe");
            command.command_mut().arg(path);
        } else if id.to_ascii_lowercase().ends_with(".exe") {
            let path = Path::new(id);
            if !path.is_absolute() || !path.is_file() {
                return Err(Refusal::new(
                    "app_not_found",
                    "an exe entry is an installed program's exe name (such as spotify.exe) or its full path; use its AppUserModelID otherwise",
                ));
            }
            command = OwnedCommand::new(path);
        } else {
            if !aumid(id) {
                return Err(invalid(
                    "app is not an AppUserModelID or the full path of an exe",
                ));
            }
            command = OwnedCommand::new("explorer.exe");
            command.command_mut().arg(format!("shell:AppsFolder\\{id}"));
        }
        command
            .command_mut()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .spawn_uncontained_detached()
            .map(|_| ())
            .map_err(|_| Refusal::new("spawn_failed", "the app could not be started"))
    }
}
