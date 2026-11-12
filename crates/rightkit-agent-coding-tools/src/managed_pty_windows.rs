//! Windows ConPTY transport for the host app-managed PTY execution.
//!
//! Mechanism/argv adaptation: gnoviawan/termul@bbedc41424b46511cfce62880c590203fc291a65,
//! src-tauri/src/pty/windows.rs. MIT notice: ../licenses/termul-MIT.txt.
//! No renderer, terminal grid, session store or alternate process authority is imported.
//!
//! This is the narrow native transport layer only. It owns raw ConPTY I/O,
//! resize, process wait/cancellation, and process-tree containment. It does not
//! parse terminal escape sequences or own rendering/grid state.

use super::{spawn_reader_bridge, spawn_writer_bridge, PtyAsyncReader, PtyAsyncWriter};
use rightkit_sandbox::execution::{
    CommandLaunchSpec, EffectExecutionError, ExecutionChannel, ManagedChild, PtySize,
    SandboxProcessProof,
};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::mem::{size_of, MaybeUninit};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::FromRawHandle;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::{AsyncRead, AsyncWrite};
use winapi::ctypes::c_void;
use winapi::shared::minwindef::DWORD;
use winapi::um::handleapi::{CloseHandle, SetHandleInformation, INVALID_HANDLE_VALUE};
use winapi::um::processthreadsapi::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
};
use winapi::um::winbase::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    HANDLE_FLAG_INHERIT, STARTF_USESTDHANDLES, STARTUPINFOEXW, WAIT_OBJECT_0,
};
use winapi::um::wincon::COORD;
use winapi::um::wincontypes::HPCON;

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: DWORD = 0x0002_0016;

pub(super) fn spawn(
    spec: &CommandLaunchSpec,
    size: PtySize,
    program: &str,
    argv: &[String],
    proof: SandboxProcessProof,
    sandbox_tier: Option<crate::sandbox::SandboxTier>,
) -> Result<Box<dyn ManagedChild>, EffectExecutionError> {
    let size = validate_size(size)?;
    let image = crate::sandbox::resolve_program(program).map_err(|error| {
        tracing::warn!("[sandbox] PTY launch refused: cannot resolve program '{program}': {error}");
        spawn_failed(error)
    })?;
    // `Some(tier)` means a confined launch was decided: every failure below
    // refuses the launch (never a silent unconfined fallback).
    let confined = sandbox_tier.is_some();
    let profile = match sandbox_tier {
        Some(tier) => Some(
            crate::sandbox::AppContainerProfile::for_workspace(std::path::Path::new(
                &spec.context.workspace_root,
            ))
            .map_err(|error| {
                eprintln!(
                    "[sandbox] confined PTY launch refused: AppContainer profile/workspace preparation failed (workspace={}): {error}",
                    spec.context.workspace_root
                );
                EffectExecutionError::SandboxMissing
            })?
            // The token carries `internetClient` exactly when the tier allows
            // network, mirroring the piped launch path.
            .with_network(!tier.denies_network()),
        ),
        None => None,
    };
    let (child_cwd, mut environment) = match &profile {
        Some(profile) => {
            // The same validated alias translation as piped confined
            // launches: a failed preparation must not launch anywhere else.
            let child_cwd = profile
                .confined_child_cwd(std::path::Path::new(&spec.cwd))
                .map_err(|error| {
                    eprintln!(
                        "[sandbox] confined PTY launch refused: working directory {} rejected: {error}",
                        spec.cwd
                    );
                    EffectExecutionError::SandboxMissing
                })?;
            let environment = profile
                .prepare_launch(&image, &child_cwd, &spec.env)
                .map_err(|error| {
                    eprintln!(
                        "[sandbox] confined PTY launch refused: launch preparation failed (image={}): {error}",
                        image.display()
                    );
                    EffectExecutionError::SandboxMissing
                })?;
            (child_cwd, environment)
        }
        None => (
            std::path::PathBuf::from(&spec.cwd),
            environment_block(&spec.env),
        ),
    };
    // The capability SIDs and the container SID outlive the attribute list.
    let capability_set = match &profile {
        Some(profile) => Some(profile.capability_set().map_err(|error| {
            tracing::warn!("[sandbox] confined PTY launch refused: capability set failed: {error}");
            EffectExecutionError::SandboxMissing
        })?),
        None => None,
    };
    let capabilities = capability_set
        .as_ref()
        .map(|capability_set| capability_set.security_capabilities());
    // ACL leases must outlive the child: dropping the profile at the end of
    // this function would revoke the workspace/image grants under a running
    // PTY child. The keep-alive is owned by the managed child below.
    let lease_keepalive = profile.as_ref().map(|profile| profile.lease_keepalive());

    let (input_read, input_write) = create_pipe().map_err(spawn_failed)?;
    let (output_read, output_write) = create_pipe().map_err(spawn_failed)?;
    set_not_inheritable(&input_write).map_err(spawn_failed)?;
    set_not_inheritable(&output_read).map_err(spawn_failed)?;

    let mut hpcon: HPCON = std::ptr::null_mut();
    let created = unsafe {
        winapi::um::consoleapi::CreatePseudoConsole(
            to_coord(size),
            input_read.raw(),
            output_write.raw(),
            0,
            &mut hpcon,
        )
    };
    if created != 0 {
        return Err(EffectExecutionError::SpawnFailed);
    }
    let pseudo_console = PseudoConsole::new(hpcon);

    let mut attributes =
        ProcThreadAttributes::for_pseudo_console(pseudo_console.raw(), capabilities.as_ref())
            .map_err(spawn_failed)?;

    let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
    startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
    startup.lpAttributeList = attributes.ptr();

    let command_line = build_windows_command_line(program, argv);
    let mut command_wide = to_wide(&command_line);
    let cwd_wide = to_wide(child_cwd.as_os_str());
    let image_wide = to_wide(&image);

    let mut process_info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let created = unsafe {
        CreateProcessW(
            image_wide.as_ptr(),
            command_wide.as_mut_ptr(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
            environment.as_mut_ptr() as *mut c_void,
            cwd_wide.as_ptr(),
            &mut startup.StartupInfo,
            &mut process_info,
        )
    };
    if created == 0 {
        eprintln!(
            "[sandbox] PTY CreateProcessW failed (confined={confined}, image={}): {}",
            image.display(),
            io::Error::last_os_error()
        );
        return Err(if confined {
            EffectExecutionError::SandboxMissing
        } else {
            EffectExecutionError::SpawnFailed
        });
    }

    let process = RawHandle::new(process_info.hProcess);
    let thread = RawHandle::new(process_info.hThread);
    let pid = process_info.dwProcessId;

    // The process is created suspended so the host app's existing process-tree
    // authority can own it before any descendant can escape the Job Object.
    let job = match crate::sandbox::shell_job().and_then(|job| {
        job.assign_process_id(pid)?;
        Ok(job)
    }) {
        Ok(job) => job,
        Err(error) => {
            eprintln!(
                "[sandbox] PTY child pid {pid} could not be assigned to its Job Object before start; terminating it: {error}"
            );
            unsafe {
                let _ = TerminateProcess(process.raw(), 1);
            }
            return Err(EffectExecutionError::SpawnFailed);
        }
    };

    let resumed = unsafe { ResumeThread(thread.raw()) };
    if resumed == u32::MAX {
        eprintln!(
            "[sandbox] PTY child pid {pid} failed to resume: {}",
            io::Error::last_os_error()
        );
        unsafe {
            let _ = TerminateProcess(process.raw(), 1);
        }
        return Err(EffectExecutionError::SpawnFailed);
    }
    drop(thread);

    // ConPTY owns the child-facing ends after CreatePseudoConsole. Retaining our
    // duplicates would prevent input EOF/output completion from propagating.
    drop(input_read);
    drop(output_write);

    // Drop ownership precedes thread allocation, so a bridge failure cannot
    // leave the child, job or ConPTY alive without a managed owner.
    let mut managed = ManagedConPtyChild {
        pseudo_console: Some(pseudo_console),
        process,
        job,
        pid,
        proof,
        size,
        input: None,
        input_shutdown: Arc::new(AtomicBool::new(false)),
        output: None,
        output_drain: Arc::new(AtomicBool::new(false)),
        exit_code: None,
        close_task: None,
        _leases: lease_keepalive,
    };
    let (output, drain) =
        spawn_reader_bridge(Box::new(output_read.into_file())).map_err(spawn_failed)?;
    managed.output = Some(output);
    managed.output_drain = drain;
    let (input, stop) =
        spawn_writer_bridge(Box::new(input_write.into_file())).map_err(spawn_failed)?;
    managed.input = Some(input);
    managed.input_shutdown = stop;
    Ok(Box::new(managed))
}

struct ManagedConPtyChild {
    pseudo_console: Option<PseudoConsole>,
    process: RawHandle,
    job: crate::sandbox::WindowsJob,
    pid: u32,
    proof: SandboxProcessProof,
    size: PtySize,
    input: Option<PtyAsyncWriter>,
    input_shutdown: Arc<AtomicBool>,
    output: Option<PtyAsyncReader>,
    output_drain: Arc<AtomicBool>,
    exit_code: Option<i32>,
    close_task: Option<tokio::task::JoinHandle<()>>,
    /// The confined profile's ACL leases. Declared last so they are released
    /// only after `Drop` has terminated the job and closed the pseudo console.
    _leases: Option<Arc<dyn std::any::Any + Send + Sync>>,
}

#[async_trait::async_trait]
impl ManagedChild for ManagedConPtyChild {
    fn pid(&self) -> Option<u32> {
        Some(self.pid)
    }

    fn sandbox_proof(&self) -> Option<SandboxProcessProof> {
        Some(self.proof.clone())
    }

    fn execution_channel(&self) -> ExecutionChannel {
        ExecutionChannel::Pty(self.size)
    }

    fn start_kill(&mut self) -> bool {
        self.job.terminate().is_ok()
    }

    fn take_stdin(&mut self) -> Option<Pin<Box<dyn AsyncWrite + Send>>> {
        self.input
            .take()
            .map(|writer| Box::pin(writer) as Pin<Box<dyn AsyncWrite + Send>>)
    }

    fn take_stdout(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        self.output
            .take()
            .map(|reader| Box::pin(reader) as Pin<Box<dyn AsyncRead + Send>>)
    }

    fn take_stderr(&mut self) -> Option<Pin<Box<dyn AsyncRead + Send>>> {
        None
    }

    fn resize_pty(&mut self, size: PtySize) -> Result<(), EffectExecutionError> {
        let size = validate_size(size)?;
        let Some(console) = self.pseudo_console.as_ref() else {
            return Err(EffectExecutionError::Internal);
        };
        let resized =
            unsafe { winapi::um::consoleapi::ResizePseudoConsole(console.raw(), to_coord(size)) };
        if resized != 0 {
            return Err(EffectExecutionError::Internal);
        }
        self.size = size;
        Ok(())
    }

    async fn wait(&mut self) -> Result<i32, EffectExecutionError> {
        if let Some(code) = self.exit_code {
            self.finish_console_close().await?;
            return Ok(code);
        }
        // Poll the owned handle. No raw borrowed handle escapes into a
        // detached wait task if this future is cancelled or the child drops.
        loop {
            let waited =
                unsafe { winapi::um::synchapi::WaitForSingleObject(self.process.raw(), 0) };
            if waited == WAIT_OBJECT_0 {
                break;
            }
            if waited != 258 {
                return Err(EffectExecutionError::Internal);
            } // WAIT_TIMEOUT
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let mut code = 0_u32;
        if unsafe { GetExitCodeProcess(self.process.raw(), &mut code) } == 0 {
            return Err(EffectExecutionError::Internal);
        }
        let code = code as i32;
        self.exit_code = Some(code);
        // Closing the pseudo console after process exit releases ConPTY's
        // output-side pipe so raw readers can observe terminal EOF.
        self.input_shutdown.store(true, Ordering::Release);
        let _ = self.job.terminate();
        if let Some(console) = self.pseudo_console.take() {
            // Older Windows can block on terminal close while output drains.
            // Keep that wait off the async executor, with owned handle lifetime.
            self.close_task = Some(tokio::task::spawn_blocking(move || drop(console)));
        }
        self.finish_console_close().await?;
        Ok(code)
    }

    async fn cancel(&mut self) -> Result<(), EffectExecutionError> {
        self.output_drain.store(true, Ordering::Release);
        self.input_shutdown.store(true, Ordering::Release);
        self.input.take();
        self.job
            .terminate()
            .map_err(|_| EffectExecutionError::KillFailed)
    }
}

impl ManagedConPtyChild {
    async fn finish_console_close(&mut self) -> Result<(), EffectExecutionError> {
        // Retain the close operation across cancellation of a wait future.
        // A resize request cannot detach cleanup or settle a lease early.
        if let Some(task) = self.close_task.as_mut() {
            task.await.map_err(|_| EffectExecutionError::Internal)?;
        }
        self.close_task.take();
        Ok(())
    }
}

impl Drop for ManagedConPtyChild {
    fn drop(&mut self) {
        self.output_drain.store(true, Ordering::Release);
        self.input_shutdown.store(true, Ordering::Release);
        self.input.take();
        // The job owns descendants even when the initial child has exited.
        let _ = self.job.terminate();
        // ClosePseudoConsole is deliberately last: process-tree teardown first,
        // then terminal transport teardown, matching the pinned Termul mechanism.
        self.pseudo_console.take();
    }
}

fn validate_size(size: PtySize) -> Result<PtySize, EffectExecutionError> {
    let valid = |value: u16| value > 0 && value <= i16::MAX as u16;
    if !valid(size.rows) || !valid(size.cols) {
        return Err(EffectExecutionError::InvalidRequest {
            field: "execution_channel".to_string(),
            reason: "ConPTY rows and columns must be between 1 and 32767".to_string(),
        });
    }
    Ok(size)
}

fn to_coord(size: PtySize) -> COORD {
    COORD {
        X: size.cols as i16,
        Y: size.rows as i16,
    }
}

fn create_pipe() -> io::Result<(RawHandle, RawHandle)> {
    let mut read = std::ptr::null_mut();
    let mut write = std::ptr::null_mut();
    let mut security = winapi::um::minwinbase::SECURITY_ATTRIBUTES {
        nLength: size_of::<winapi::um::minwinbase::SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 1,
    };
    let ok =
        unsafe { winapi::um::namedpipeapi::CreatePipe(&mut read, &mut write, &mut security, 0) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok((RawHandle::new(read), RawHandle::new(write)))
    }
}

fn set_not_inheritable(handle: &RawHandle) -> io::Result<()> {
    let ok = unsafe { SetHandleInformation(handle.raw(), HANDLE_FLAG_INHERIT, 0) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn build_windows_command_line(program: &str, args: &[String]) -> String {
    let mut command = String::new();
    append_windows_arg(&mut command, program);
    for arg in args {
        command.push(' ');
        append_windows_arg(&mut command, arg);
    }
    command
}

fn append_windows_arg(command: &mut String, arg: &str) {
    let quote = arg.is_empty()
        || arg
            .chars()
            .any(|c| matches!(c, ' ' | '\t' | '\n' | '\x0b' | '"'));
    if !quote {
        command.push_str(arg);
        return;
    }

    command.push('"');
    let mut chars = arg.chars().peekable();
    loop {
        let mut backslashes = 0_usize;
        while matches!(chars.peek(), Some('\\')) {
            chars.next();
            backslashes += 1;
        }
        match chars.next() {
            None => {
                for _ in 0..backslashes * 2 {
                    command.push('\\');
                }
                break;
            }
            Some('"') => {
                for _ in 0..backslashes * 2 + 1 {
                    command.push('\\');
                }
                command.push('"');
            }
            Some(c) => {
                for _ in 0..backslashes {
                    command.push('\\');
                }
                command.push(c);
            }
        }
    }
    command.push('"');
}

fn environment_block(overrides: &[(String, String)]) -> Vec<u16> {
    // Windows environment names are case-insensitive. Preserve the preferred
    // casing while sorting by a normalized key for CreateProcessW.
    let mut entries: BTreeMap<String, (OsString, OsString)> = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        entries.insert(key.to_string_lossy().to_ascii_lowercase(), (key, value));
    }
    for (key, value) in overrides {
        entries.insert(
            key.to_ascii_lowercase(),
            (OsString::from(key), OsString::from(value)),
        );
    }

    let mut block = Vec::new();
    for (_, (key, value)) in entries {
        block.extend(key.encode_wide());
        block.push('=' as u16);
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

fn to_wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn spawn_failed(_: io::Error) -> EffectExecutionError {
    EffectExecutionError::SpawnFailed
}

struct ProcThreadAttributes {
    ptr: LPPROC_THREAD_ATTRIBUTE_LIST,
    _storage: Vec<MaybeUninit<usize>>,
}

impl ProcThreadAttributes {
    fn for_pseudo_console(
        hpcon: HPCON,
        capabilities: Option<&windows::Win32::Security::SECURITY_CAPABILITIES>,
    ) -> io::Result<Self> {
        let count = if capabilities.is_some() { 2 } else { 1 };
        let mut bytes = 0_usize;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), count, 0, &mut bytes);
        }
        let words = bytes.div_ceil(size_of::<usize>()).max(1);
        let mut storage = vec![MaybeUninit::<usize>::uninit(); words];
        let ptr = storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if unsafe { InitializeProcThreadAttributeList(ptr, count, 0, &mut bytes) } == 0 {
            return Err(io::Error::last_os_error());
        }

        let updated = unsafe {
            UpdateProcThreadAttribute(
                ptr,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                hpcon as *mut c_void,
                size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if updated == 0 {
            unsafe {
                DeleteProcThreadAttributeList(ptr);
            }
            return Err(io::Error::last_os_error());
        }

        if let Some(capabilities) = capabilities {
            // The sandbox and PTY are attributes of the SAME suspended child.
            let updated = unsafe {
                UpdateProcThreadAttribute(
                    ptr,
                    0,
                    windows::Win32::System::Threading::PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES
                        as usize,
                    (capabilities as *const windows::Win32::Security::SECURITY_CAPABILITIES)
                        .cast_mut()
                        .cast(),
                    size_of::<windows::Win32::Security::SECURITY_CAPABILITIES>(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if updated == 0 {
                let error = io::Error::last_os_error();
                unsafe {
                    DeleteProcThreadAttributeList(ptr);
                }
                return Err(error);
            }
        }
        Ok(Self {
            ptr,
            _storage: storage,
        })
    }

    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.ptr
    }
}

impl Drop for ProcThreadAttributes {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe {
                DeleteProcThreadAttributeList(self.ptr);
            }
            self.ptr = std::ptr::null_mut();
        }
    }
}

struct PseudoConsole(HPCON);

unsafe impl Send for PseudoConsole {}

impl PseudoConsole {
    fn new(handle: HPCON) -> Self {
        Self(handle)
    }

    fn raw(&self) -> HPCON {
        self.0
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                winapi::um::consoleapi::ClosePseudoConsole(self.0);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

struct RawHandle(*mut c_void);

unsafe impl Send for RawHandle {}

impl RawHandle {
    fn new(handle: *mut c_void) -> Self {
        Self(handle)
    }

    fn raw(&self) -> *mut c_void {
        self.0
    }

    fn into_file(mut self) -> File {
        let handle = self.0;
        self.0 = std::ptr::null_mut();
        unsafe { File::from_raw_handle(handle.cast()) }
    }
}

impl Drop for RawHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}
