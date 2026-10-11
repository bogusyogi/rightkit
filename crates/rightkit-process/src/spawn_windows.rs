//! Stable Win32 spawn adapter. Opaque std Command settings are supplied explicitly.
use crate::process_windows::os_error;
use std::{
    ffi::{OsStr, OsString},
    fs::OpenOptions,
    io,
    os::windows::{
        ffi::OsStrExt,
        io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use windows::{
    core::{PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            GetHandleInformation, SetHandleInformation, HANDLE, HANDLE_FLAGS, HANDLE_FLAG_INHERIT,
            WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::Threading::{
            CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
            InitializeProcThreadAttributeList, ResumeThread, TerminateProcess,
            UpdateProcThreadAttribute, WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB,
            CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS,
            EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTF_USESTDHANDLES, STARTUPINFOEXW,
        },
    },
};

/// Stdio configuration for a stable native Windows spawn.
#[derive(Debug, Default)]
pub enum WindowsStdio {
    #[default]
    Inherit,
    Null,
    Piped,
    Handle(OwnedHandle),
}

/// Explicit settings that stable std Command cannot expose to a native adapter.
/// These stdio settings replace std Command's settings only on allowlisted spawns.
#[derive(Debug)]
pub struct WindowsSpawnConfig {
    pub stdin: WindowsStdio,
    pub stdout: WindowsStdio,
    pub stderr: WindowsStdio,
    /// Whether to copy parent environment before Command's explicit edits.
    pub inherit_environment: bool,
    /// Indices in Command::get_args() originally added through CommandExt::raw_arg.
    pub raw_arguments: Vec<usize>,
}
impl Default for WindowsSpawnConfig {
    fn default() -> Self {
        Self {
            stdin: WindowsStdio::Inherit,
            stdout: WindowsStdio::Inherit,
            stderr: WindowsStdio::Inherit,
            inherit_environment: true,
            raw_arguments: Vec::new(),
        }
    }
}

/// Child view shared by std & native Windows spawns; pipes retain std's types.
#[derive(Debug)]
pub struct WindowsChild {
    pub stdin: Option<ChildStdin>,
    pub stdout: Option<ChildStdout>,
    pub stderr: Option<ChildStderr>,
    handle: OwnedHandle,
    thread: Option<OwnedHandle>,
    pid: u32,
}

impl From<Child> for WindowsChild {
    fn from(mut child: Child) -> Self {
        Self {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            pid: child.id(),
            handle: child.into(),
            thread: None,
        }
    }
}

impl AsHandle for WindowsChild {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.handle.as_handle()
    }
}
impl AsRawHandle for WindowsChild {
    fn as_raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }
}

impl WindowsChild {
    pub fn id(&self) -> u32 {
        self.pid
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.wait_native(0)
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.stdin.take());
        self.wait_native(u32::MAX)?
            .ok_or_else(|| io::Error::other("infinite wait timed out"))
    }
    pub fn kill(&mut self) -> io::Result<()> {
        if self.try_wait()?.is_some() {
            return Ok(());
        }
        match unsafe { TerminateProcess(HANDLE(self.handle.as_raw_handle()), 1) } {
            Ok(()) => Ok(()),
            Err(error) => {
                let error = os_error(error);
                if self.try_wait()?.is_some() {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }
    fn wait_native(&self, timeout: u32) -> io::Result<Option<ExitStatus>> {
        let raw = HANDLE(self.handle.as_raw_handle());
        match unsafe { WaitForSingleObject(raw, timeout) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                unsafe { GetExitCodeProcess(raw, &mut code) }.map_err(os_error)?;
                Ok(Some(ExitStatus::from_raw(code)))
            }
            _ => Err(io::Error::last_os_error()),
        }
    }
    pub(crate) fn resume(&mut self, detached: bool) -> io::Result<()> {
        if let Some(thread) = self.thread.take() {
            if unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        } else if !detached {
            crate::process_windows::resume(self.pid)
        } else {
            Ok(())
        }
    }
}

// One lock for every process this crate creates on Windows.
//
// An allowlisted spawn raises the inherit flag on the caller's handles for the length of
// `CreateProcessW` and lowers it again afterwards. Rust's ordinary spawn passes
// `bInheritHandles = TRUE` with no handle list, so one running inside that window would hand
// those handles to its own child: a file or pipe the caller meant for one process only. Both
// spawn paths of this crate therefore take this lock, and it covers the raise, the creation
// and the restore. Callers must keep supplied handles valid and leave their flags alone until
// spawn returns.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// Held while this crate creates a process.
pub(crate) fn spawn_guard() -> std::sync::MutexGuard<'static, ()> {
    SPAWN_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// Run `spawn` while no RightKit spawn has handles raised for inheritance.
///
/// `OwnedCommand::inherit_handles_only` makes the listed handles inheritable for the moment
/// the child is created. A process created at that moment by other code (a plain
/// `std::process::Command::spawn`, a library) inherits them too, because Windows hands every
/// inheritable handle to a child created with handle inheritance on. RightKit's own spawns
/// are serialised against each other; code that spawns by other means in a process that also
/// uses `inherit_handles_only` wraps its spawn in this function to join that serialisation.
///
/// Do not create a RightKit process inside `spawn`: the lock is not re-entrant.
pub fn with_windows_spawn_lock<T>(spawn: impl FnOnce() -> T) -> T {
    let _spawning = spawn_guard();
    spawn()
}

/// Test seam: stop an allowlisted spawn at the point where its handles are inheritable.
#[cfg(test)]
mod pause {
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    /// (the handle the spawn to stop must carry, reached, released).
    static STATE: Mutex<(Option<usize>, bool, bool)> = Mutex::new((None, false, false));
    static CHANGED: Condvar = Condvar::new();

    pub(super) fn arm(handle: usize) {
        *STATE.lock().unwrap() = (Some(handle), false, false);
    }

    /// Called by `spawn` once the inherit flags are raised. Only the armed spawn stops.
    pub(super) fn reached(handles: &[usize]) {
        let mut state = STATE.lock().unwrap();
        if !state.0.is_some_and(|armed| handles.contains(&armed)) {
            return;
        }
        state.1 = true;
        CHANGED.notify_all();
        while !state.2 {
            state = CHANGED.wait(state).unwrap();
        }
        *state = (None, false, false);
    }

    pub(super) fn wait_reached() {
        let state = STATE.lock().unwrap();
        let (state, timeout) = CHANGED
            .wait_timeout_while(state, Duration::from_secs(30), |state| !state.1)
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "the armed spawn never raised its handles"
        );
        drop(state);
    }

    pub(super) fn release() {
        STATE.lock().unwrap().2 = true;
        CHANGED.notify_all();
    }
}

struct Inheritable(Vec<(HANDLE, u32)>);
impl Inheritable {
    fn new(handles: &[HANDLE]) -> io::Result<Self> {
        let mut guard = Self(Vec::with_capacity(handles.len()));
        for &handle in handles {
            let mut flags = 0;
            unsafe { GetHandleInformation(handle, &mut flags) }.map_err(os_error)?;
            if flags & HANDLE_FLAG_INHERIT.0 == 0 {
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) }
                    .map_err(os_error)?;
                guard.0.push((handle, flags));
            }
        }
        Ok(guard)
    }
    fn restore(&mut self) -> io::Result<()> {
        let mut error = None;
        for &(handle, flags) in self.0.iter().rev() {
            if let Err(failure) =
                unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(flags)) }
            {
                error.get_or_insert_with(|| os_error(failure));
            }
        }
        if let Some(error) = error {
            Err(error)
        } else {
            self.0.clear();
            Ok(())
        }
    }
}
impl Drop for Inheritable {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

struct Attributes {
    _storage: Vec<AttributeStorage>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
}
#[repr(align(16))]
#[derive(Clone)]
struct AttributeStorage {
    _bytes: [u8; 16],
}
impl Attributes {
    fn new(handles: &[HANDLE]) -> io::Result<Self> {
        let mut bytes = 0;
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut storage = vec![AttributeStorage { _bytes: [0; 16] }; bytes.div_ceil(16)];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut bytes) }
            .map_err(os_error)?;
        let attributes = Self {
            _storage: storage,
            list,
        };
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                std::mem::size_of_val(handles),
                None,
                None,
            )
        }
        .map_err(os_error)?;
        Ok(attributes)
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.list);
        }
    }
}

fn stdio(config: WindowsStdio, index: u32) -> io::Result<(OwnedHandle, Option<OwnedHandle>)> {
    match config {
        WindowsStdio::Piped => {
            let (parent, child) = child_pipe(index != 0)?;
            Ok((child, Some(parent)))
        }
        WindowsStdio::Handle(handle) => Ok((handle, None)),
        WindowsStdio::Inherit => {
            let raw = unsafe { GetStdHandle((-10i32 - index as i32) as u32) };
            if !raw.is_null() && raw as isize != -1 {
                // Duplicate parent's stdio; never mutate its flags or own its original.
                return Ok((
                    unsafe { BorrowedHandle::borrow_raw(raw) }.try_clone_to_owned()?,
                    None,
                ));
            }
            stdio(WindowsStdio::Null, index)
        }
        WindowsStdio::Null => {
            let file = OpenOptions::new()
                .read(index == 0)
                .write(index != 0)
                .open("NUL")?;
            Ok((file.into(), None))
        }
    }
}

fn child_pipe(parent_reads: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
    // ChildStdout/ChildStderr/ChildStdin use ReadFileEx/WriteFileEx, so their
    // parent ends must be overlapped; io::pipe/CreatePipe cannot supply those.
    static NEXT_PIPE: AtomicU64 = AtomicU64::new(0);
    let name = format!(
        "\\\\.\\pipe\\rightkit-process-{}-{}",
        std::process::id(),
        NEXT_PIPE.fetch_add(1, Ordering::Relaxed)
    );
    let name = wide(OsStr::new(&name))?;
    let access = if parent_reads { 1 } else { 2 }; // PIPE_ACCESS_INBOUND/OUTBOUND
    let parent = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            access | 0x4000_0000 | 0x0008_0000, // OVERLAPPED | FIRST_PIPE_INSTANCE
            8,
            1,
            65_536,
            65_536,
            0,
            std::ptr::null(),
        )
    }; // REJECT_REMOTE_CLIENTS
    if parent as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    let parent = unsafe { OwnedHandle::from_raw_handle(parent) };
    // Opening the client transitions this instance to connected. Only parent
    // end is overlapped; child end remains compatible with standard console I/O.
    let child = unsafe {
        CreateFileW(
            name.as_ptr(),
            if parent_reads {
                0x4000_0000
            } else {
                0x8000_0000
            }, // GENERIC_WRITE/READ
            0,
            std::ptr::null(),
            3,
            0x0010_0080,
            std::ptr::null_mut(),
        )
    };
    if child as isize == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((parent, unsafe { OwnedHandle::from_raw_handle(child) }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn spawn(
    command: &Command,
    mut config: WindowsSpawnConfig,
    handles: &[usize],
    detached: bool,
    tolerate_denied_breakaway: bool,
    hidden: bool,
    env_cleared: bool,
    cleanup_timeout: Option<Duration>,
) -> io::Result<(WindowsChild, bool)> {
    if env_cleared {
        config.inherit_environment = false;
    }
    let environment = environment(command, config.inherit_environment)?;
    let program = resolve_program(command, &environment)?;
    let (program_wide, mut line) = if program.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("bat") || extension.eq_ignore_ascii_case("cmd")
    }) {
        let mut buffer = vec![0; 32_768];
        let length =
            unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 || length >= buffer.len() {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(length);
        buffer.extend("\\cmd.exe\0".encode_utf16());
        (
            buffer,
            batch_command_line(&program, command, &config.raw_arguments)?,
        )
    } else {
        (
            wide(program.as_os_str())?,
            command_line(command, &config.raw_arguments)?,
        )
    };
    let cwd = command
        .get_current_dir()
        .map(|path| wide(path.as_os_str()))
        .transpose()?;
    let env_block = environment_block(&environment)?;
    let (stdin, parent_stdin) = stdio(config.stdin, 0)?;
    let (stdout, parent_stdout) = stdio(config.stdout, 1)?;
    let (stderr, parent_stderr) = stdio(config.stderr, 2)?;
    let mut handles: Vec<_> = handles
        .iter()
        .map(|handle| HANDLE(*handle as RawHandle))
        .collect();
    handles.extend([
        HANDLE(stdin.as_raw_handle()),
        HANDLE(stdout.as_raw_handle()),
        HANDLE(stderr.as_raw_handle()),
    ]);
    handles.sort_unstable_by_key(|handle| handle.0 as usize);
    handles.dedup();
    let _lock = spawn_guard();
    let mut inheritable = Inheritable::new(&handles)?;
    #[cfg(test)]
    pause::reached(
        &handles
            .iter()
            .map(|handle| handle.0 as usize)
            .collect::<Vec<_>>(),
    );
    let attributes = Attributes::new(&handles)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = HANDLE(stdin.as_raw_handle());
    startup.StartupInfo.hStdOutput = HANDLE(stdout.as_raw_handle());
    startup.StartupInfo.hStdError = HANDLE(stderr.as_raw_handle());
    startup.lpAttributeList = attributes.list;
    let mut flags = CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;
    if hidden {
        flags |= CREATE_NO_WINDOW;
    }
    if detached {
        flags |= CREATE_BREAKAWAY_FROM_JOB;
    }
    let mut info = PROCESS_INFORMATION::default();
    let spare_line = line.clone();
    let create = |flags, line: &mut Vec<u16>, info: &mut PROCESS_INFORMATION| {
        unsafe {
            CreateProcessW(
                PCWSTR(program_wide.as_ptr()),
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                true,
                flags,
                Some(env_block.as_ptr().cast()),
                PCWSTR(cwd.as_ref().map_or(std::ptr::null(), |cwd| cwd.as_ptr())),
                &startup.StartupInfo,
                info,
            )
        }
        .map_err(os_error)
    };
    let mut stayed_in_job = false;
    match create(flags, &mut line, &mut info) {
        Err(error) if detached && tolerate_denied_breakaway && error.raw_os_error() == Some(5) => {
            // The parent job denied breakaway: one retry, detached, inside the job.
            let mut retry = CREATE_SUSPENDED
                | CREATE_UNICODE_ENVIRONMENT
                | EXTENDED_STARTUPINFO_PRESENT
                | DETACHED_PROCESS;
            if hidden {
                retry |= CREATE_NO_WINDOW;
            }
            line = spare_line;
            info = PROCESS_INFORMATION::default();
            create(retry, &mut line, &mut info)?;
            stayed_in_job = true;
        }
        other => other?,
    }
    // Immediately own both handles, even if restoring caller flags fails.
    let child = WindowsChild {
        stdin: parent_stdin.map(ChildStdin::from),
        stdout: parent_stdout.map(ChildStdout::from),
        stderr: parent_stderr.map(ChildStderr::from),
        handle: unsafe { OwnedHandle::from_raw_handle(info.hProcess.0) },
        thread: Some(unsafe { OwnedHandle::from_raw_handle(info.hThread.0) }),
        pid: info.dwProcessId,
    };
    if let Err(error) = inheritable.restore() {
        crate::owned::cleanup_direct(Arc::new(Mutex::new(child)), cleanup_timeout);
        return Err(error);
    }
    Ok((child, stayed_in_job))
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "embedded NUL"));
    }
    value.push(0);
    Ok(value)
}

fn command_line(command: &Command, raw_arguments: &[usize]) -> io::Result<Vec<u16>> {
    let program = wide(command.get_program())?;
    if program.contains(&(b'"' as u16)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "program contains quotes",
        ));
    }
    let args: Vec<_> = command.get_args().collect();
    if raw_arguments.iter().any(|index| *index >= args.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "raw argument index out of bounds",
        ));
    }
    let mut line = vec![b'"' as u16];
    line.extend_from_slice(&program[..program.len() - 1]);
    line.push(b'"' as u16);
    for (index, arg) in args.into_iter().enumerate() {
        line.push(b' ' as u16);
        let mut arg = wide(arg)?;
        arg.pop();
        if raw_arguments.contains(&index) {
            line.extend(arg);
            continue;
        }
        append_argument(&mut line, &arg);
    }
    line.push(0);
    if line.len() > 32_767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command line too long",
        ));
    }
    Ok(line)
}

// Match std's regular-argument quoting: empty/space/tab quote, embedded quote
// gets 2n+1 backslashes, & quoted trailing backslashes get doubled.
fn append_argument(line: &mut Vec<u16>, arg: &[u16]) {
    let quote = arg.is_empty() || arg.iter().any(|ch| *ch == 32 || *ch == 9);
    if quote {
        line.push(34);
    }
    let mut slashes = 0;
    for &ch in arg {
        if ch == 92 {
            slashes += 1;
        } else {
            if ch == 34 {
                line.extend(std::iter::repeat_n(92, slashes + 1));
            }
            slashes = 0;
        }
        line.push(ch);
    }
    if quote {
        line.extend(std::iter::repeat_n(92, slashes));
        line.push(34);
    }
}

fn compare_keys(left: &OsStr, right: &OsStr) -> std::cmp::Ordering {
    let left: Vec<_> = left.encode_wide().collect();
    let right: Vec<_> = right.encode_wide().collect();
    match unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        )
    } {
        1 => std::cmp::Ordering::Less,
        3 => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    }
}

fn environment(command: &Command, inherit: bool) -> io::Result<Vec<(OsString, OsString)>> {
    let mut env: Vec<_> = if inherit {
        std::env::vars_os().collect()
    } else {
        Vec::new()
    };
    for (key, value) in command.get_envs() {
        let key_wide = wide(key)?;
        if key_wide.len() == 1 || key_wide[..key_wide.len() - 1].contains(&61) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid environment key",
            ));
        }
        env.retain(|(existing, _)| !compare_keys(existing, key).is_eq());
        if let Some(value) = value {
            wide(value)?;
            env.push((key.into(), value.into()));
        }
    }
    env.sort_by(|(left, _), (right, _)| compare_keys(left, right));
    Ok(env)
}

fn environment_block(env: &[(OsString, OsString)]) -> io::Result<Vec<u16>> {
    let mut block = Vec::new();
    for (key, value) in env {
        let mut key = wide(key)?;
        key.pop();
        block.extend(key);
        block.push(61);
        block.extend(wide(value)?);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

fn resolve_program(command: &Command, env: &[(OsString, OsString)]) -> io::Result<PathBuf> {
    let program = Path::new(command.get_program());
    let program_wide = wide(program.as_os_str())?;
    if program_wide.len() == 1
        || program_wide
            .get(program_wide.len() - 2)
            .is_some_and(|ch| *ch == 47 || *ch == 92)
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty program"));
    }
    let mut file = program.to_path_buf();
    let find = |path: PathBuf| -> Option<PathBuf> {
        if path.is_file() {
            Some(path)
        } else {
            None
        }
    };
    if file.components().count() > 1 || file.is_absolute() {
        if !file
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            let mut appended = file.as_os_str().to_owned();
            appended.push(".exe");
            if let Some(path) = find(PathBuf::from(appended)) {
                return std::path::absolute(path);
            }
        }
        return std::path::absolute(file);
    }
    if !program_wide.contains(&46) {
        file.set_extension("exe");
    }
    let child_path = env
        .iter()
        .find(|(key, _)| compare_keys(key, OsStr::new("PATH")).is_eq());
    let explicitly_set_path = command
        .get_envs()
        .any(|(key, value)| compare_keys(key, OsStr::new("PATH")).is_eq() && value.is_some());
    let mut directories = Vec::new();
    if explicitly_set_path {
        if let Some((_, path)) = child_path {
            directories
                .extend(std::env::split_paths(path).filter(|path| !path.as_os_str().is_empty()));
        }
    }
    if let Some(parent) = std::env::current_exe()?.parent() {
        directories.push(parent.into());
    }
    for system in [
        GetSystemDirectoryW as unsafe extern "system" fn(*mut u16, u32) -> u32,
        GetWindowsDirectoryW as unsafe extern "system" fn(*mut u16, u32) -> u32,
    ] {
        let mut buffer = vec![0; 32_768];
        let length = unsafe { system(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length >= buffer.len() {
            return Err(io::Error::from_raw_os_error(206));
        }
        use std::os::windows::ffi::OsStringExt;
        directories.push(OsString::from_wide(&buffer[..length]).into());
    }
    if let Some(path) = std::env::var_os("PATH") {
        directories
            .extend(std::env::split_paths(&path).filter(|path| !path.as_os_str().is_empty()));
    }
    for directory in directories {
        if let Some(path) = find(directory.join(&file)) {
            return std::path::absolute(path);
        }
    }
    Err(io::Error::from_raw_os_error(2))
}

fn batch_command_line(
    program: &Path,
    command: &Command,
    raw_arguments: &[usize],
) -> io::Result<Vec<u16>> {
    let mut script = wide(program.as_os_str())?;
    script.pop();
    // cmd.exe cannot use verbatim paths; normalize ordinary DOS/UNC prefixes.
    if script.len() < 260 && script.starts_with(&[92, 92, 63, 92]) {
        if script.get(4..8) == Some(&[85, 78, 67, 92]) {
            script.splice(..8, [92, 92]);
        } else if script.get(5) == Some(&58) {
            script.drain(..4);
        }
    }
    if script.contains(&34) || script.last() == Some(&92) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid batch path",
        ));
    }
    let args: Vec<_> = command.get_args().collect();
    if raw_arguments.iter().any(|index| *index >= args.len()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "raw argument index out of bounds",
        ));
    }
    let mut line: Vec<_> = "cmd.exe /e:ON /v:OFF /d /c \"\"".encode_utf16().collect();
    line.extend(script);
    line.push(34);
    for (index, value) in args.into_iter().enumerate() {
        line.push(32);
        let mut value = wide(value)?;
        value.pop();
        if raw_arguments.contains(&index) {
            line.extend(value);
            continue;
        }
        if value.iter().any(|ch| *ch == 10 || *ch == 13) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid batch argument",
            ));
        }
        let quote = value.is_empty()
            || value.last() == Some(&92)
            || char::decode_utf16(value.iter().copied())
                .filter_map(Result::ok)
                .any(|ch| {
                    ch.is_control()
                        || (ch.is_ascii()
                            && !ch.is_ascii_alphanumeric()
                            && !r"#$*+-./:?@\_".contains(ch))
                });
        if quote {
            line.push(34);
        }
        let mut slashes = 0;
        for ch in value {
            if ch == 92 {
                slashes += 1;
            } else {
                if ch == 34 {
                    line.extend(std::iter::repeat_n(92, slashes));
                    line.push(34);
                } else if ch == 37 {
                    line.extend("%%cd:~,".encode_utf16());
                }
                slashes = 0;
            }
            line.push(ch);
        }
        if quote {
            line.extend(std::iter::repeat_n(92, slashes));
            line.push(34);
        }
    }
    line.extend([34, 0]);
    if line.len() > 32_767 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command line too long",
        ));
    }
    Ok(line)
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetStdHandle(index: u32) -> RawHandle;
    fn CompareStringOrdinal(
        left: *const u16,
        left_len: i32,
        right: *const u16,
        right_len: i32,
        ignore_case: i32,
    ) -> i32;
    fn GetSystemDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn GetWindowsDirectoryW(buffer: *mut u16, size: u32) -> u32;
    fn CreateNamedPipeW(
        name: *const u16,
        open_mode: u32,
        pipe_mode: u32,
        instances: u32,
        out_buffer: u32,
        in_buffer: u32,
        timeout: u32,
        security: *const std::ffi::c_void,
    ) -> RawHandle;
    fn CreateFileW(
        name: *const u16,
        access: u32,
        sharing: u32,
        security: *const std::ffi::c_void,
        disposition: u32,
        attributes: u32,
        template: RawHandle,
    ) -> RawHandle;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::windows::{ffi::OsStringExt, process::CommandExt};

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn CommandLineToArgvW(line: *const u16, argc: *mut i32) -> *mut *mut u16;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }

    #[test]
    fn windows_parser_round_trips_regular_arguments() {
        let args = [
            OsString::from(""),
            OsString::from("space tab\tend"),
            OsString::from("embedded\"quote"),
            OsString::from("slash\\\"quote"),
            OsString::from(r"trailing space \\"),
            OsString::from("plain\\"),
            OsString::from("🦀"),
            OsString::from_wide(&[0xd800, 32, 0xdc00]),
        ];
        let mut command = Command::new("C:\\Program Files\\daemon.exe");
        command.args(&args);
        let line = command_line(&command, &[]).unwrap();
        let mut argc = 0;
        let argv = unsafe { CommandLineToArgvW(line.as_ptr(), &mut argc) };
        assert!(!argv.is_null());
        struct Arguments(*mut *mut u16);
        impl Drop for Arguments {
            fn drop(&mut self) {
                unsafe {
                    LocalFree(self.0.cast());
                }
            }
        }
        let _guard = Arguments(argv);
        assert_eq!(argc as usize, args.len() + 1);
        for (index, expected) in std::iter::once(command.get_program())
            .chain(args.iter().map(OsString::as_os_str))
            .enumerate()
        {
            let pointer = unsafe { *argv.add(index) };
            let mut length = 0;
            while unsafe { *pointer.add(length) } != 0 {
                length += 1;
            }
            let actual = unsafe { std::slice::from_raw_parts(pointer, length) };
            assert_eq!(actual, expected.encode_wide().collect::<Vec<_>>());
        }
    }

    #[test]
    fn raw_arguments_are_appended_verbatim() {
        // windowless: unit test builds the argument list only; never spawned
        let mut command = Command::new("cmd.exe");
        command.arg("/c").raw_arg("echo first & echo second");
        let line = command_line(&command, &[1]).unwrap();
        assert_eq!(
            String::from_utf16(&line[..line.len() - 1]).unwrap(),
            "\"cmd.exe\" /c echo first & echo second"
        );
    }

    fn is_inheritable(handle: usize) -> bool {
        let mut flags = 0;
        unsafe { GetHandleInformation(HANDLE(handle as *mut _), &mut flags) }.unwrap();
        flags & HANDLE_FLAG_INHERIT.0 != 0
    }

    /// Fourth read, finding 6. An allowlisted spawn is stopped at the moment its private
    /// handle is inheritable. An ordinary spawn started then must not create its process
    /// until the handle is private again, or its child would inherit the handle.
    #[test]
    fn an_ordinary_spawn_waits_until_an_allowlisted_spawn_has_restored_its_handles() {
        use std::os::windows::io::AsRawHandle;
        use std::sync::mpsc;
        use std::time::Duration;

        let path = std::env::temp_dir().join(format!(
            "rightkit-process-private-{}-{:?}.txt",
            std::process::id(),
            std::thread::current().id()
        ));
        let private = std::fs::File::create(&path).unwrap();
        let handle = private.as_raw_handle() as usize;
        assert!(!is_inheritable(handle), "a fresh handle is private");

        // Both children are hidden console programs that exit at once.
        let quick = || {
            let mut command = crate::OwnedCommand::new("cmd.exe");
            command.command_mut().args(["/d", "/c", "exit", "0"]);
            command
        };

        pause::arm(handle);
        let allowlisted = std::thread::spawn(move || {
            let mut command = quick();
            command.inherit_handles_only([handle as crate::RawHandleOrFd]);
            command.windows_spawn_config(WindowsSpawnConfig {
                stdin: WindowsStdio::Null,
                stdout: WindowsStdio::Null,
                stderr: WindowsStdio::Null,
                inherit_environment: true,
                raw_arguments: Vec::new(),
            });
            let mut child = command.spawn().expect("allowlisted spawn");
            child.wait_timeout(Duration::from_secs(30)).unwrap();
        });
        pause::wait_reached();
        // The window the finding describes: the private handle is inheritable right now.
        assert!(is_inheritable(handle));

        let (created, seen) = mpsc::channel();
        let ordinary = std::thread::spawn(move || {
            let mut child = quick().spawn().expect("ordinary spawn");
            created.send(is_inheritable(handle)).unwrap();
            child.wait_timeout(Duration::from_secs(30)).unwrap();
        });
        // While the window is open the ordinary spawn creates nothing.
        assert!(
            matches!(
                seen.recv_timeout(Duration::from_millis(500)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "an ordinary spawn ran while a private handle was inheritable"
        );

        pause::release();
        // It runs once the allowlisted spawn is done, and by then the handle is private again.
        let inheritable_at_creation = seen.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(!inheritable_at_creation);
        allowlisted.join().unwrap();
        ordinary.join().unwrap();
        assert!(!is_inheritable(handle));

        // Code outside this crate joins the same serialisation.
        assert_eq!(with_windows_spawn_lock(|| 7), 7);
        drop(private);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn embedded_nul_is_rejected_before_spawn() {
        // windowless: unit test builds the argument list only; never spawned
        let mut command = Command::new("cmd.exe");
        command.arg("bad\0argument");
        assert_eq!(
            command_line(&command, &[]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn batch_arguments_reject_line_breaks_and_escape_expansions() {
        // windowless: unit test builds the argument list only; never spawned
        let mut command = Command::new("script.cmd");
        command.arg("%PATH%");
        let line = batch_command_line(Path::new("C:\\script.cmd"), &command, &[]).unwrap();
        assert!(String::from_utf16(&line[..line.len() - 1])
            .unwrap()
            .contains("%%cd:~,%PATH%%cd:~,%"));
        command.arg("bad\nargument");
        assert_eq!(
            batch_command_line(Path::new("C:\\script.cmd"), &command, &[])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
