//! Windows-only pinned enumeration of ambiguous temporary-looking names; never deletes them.

use crate::atomic::{generated_temp_pid, SweepReport};
use crate::error::{FsError, Result};
use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::{offset_of, size_of};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr::{null, null_mut};

type Handle = *mut c_void;

const FILE_LIST_DIRECTORY: u32 = 0x0001;
const READ_ATTRIBUTES: u32 = 0x0080;
const SYNCHRONIZE: u32 = 0x0010_0000;
const SHARE_READ_WRITE_DELETE: u32 = 7;
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_TAG_INFO: i32 = 9;
const FILE_ID_EXTD_DIRECTORY_INFO: i32 = 19;
const FILE_ID_EXTD_DIRECTORY_RESTART_INFO: i32 = 20;
const ERROR_NO_MORE_FILES: i32 = 18;

#[repr(C)]
struct AttributeTagInfo {
    attributes: u32,
    reparse_tag: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ExtendedDirectoryInfo {
    next_entry_offset: u32,
    file_index: u32,
    creation_time: i64,
    last_access_time: i64,
    last_write_time: i64,
    change_time: i64,
    end_of_file: i64,
    allocation_size: i64,
    attributes: u32,
    file_name_length: u32,
    ea_size: u32,
    reparse_tag: u32,
    file_id: [u8; 16],
    file_name: [u16; 1],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *const c_void,
        disposition: u32,
        flags: u32,
        template: Handle,
    ) -> Handle;
    fn GetFileInformationByHandleEx(file: Handle, class: i32, info: *mut c_void, size: u32) -> i32;
}

pub(crate) struct Candidate {
    pub(crate) name: String,
}

fn open_directory(path: &Path) -> Result<File> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if wide[..wide.len() - 1].contains(&0) {
        return Err(FsError::BadName(path.display().to_string()));
    }
    // This is an ordinary directory pin: it follows the supplied directory path as
    // `read_dir` does and does not inspect or change owner/DACL policy.
    // SAFETY: NUL-terminated path and no retained security/template pointers.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_LIST_DIRECTORY | READ_ATTRIBUTES | SYNCHRONIZE,
            SHARE_READ_WRITE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    if raw.is_null() || raw as isize == -1 {
        return Err(FsError::io(path, io::Error::last_os_error()));
    }
    // SAFETY: CreateFileW returned a fresh owned handle.
    let file = unsafe { File::from_raw_handle(raw) };
    if attributes(&file).map_err(|error| FsError::io(path, error))? & FILE_ATTRIBUTE_DIRECTORY == 0
    {
        return Err(FsError::NotADirectory(path.to_path_buf()));
    }
    Ok(file)
}

fn attributes(file: &File) -> io::Result<u32> {
    let mut info = AttributeTagInfo {
        attributes: 0,
        reparse_tag: 0,
    };
    // SAFETY: live handle and correctly sized output structure.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FILE_ATTRIBUTE_TAG_INFO,
            (&mut info as *mut AttributeTagInfo).cast(),
            size_of::<AttributeTagInfo>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(info.attributes)
}

fn for_each_candidate(directory: &File, mut visit: impl FnMut(Candidate)) -> Result<()> {
    // Read names through the pinned directory handle. Candidate identity never authorizes
    // deletion: every temporary-looking name remains ambiguous.
    let mut buffer = vec![0u64; 8192];
    let capacity = buffer.len() * size_of::<u64>();
    let mut class = FILE_ID_EXTD_DIRECTORY_RESTART_INFO;
    loop {
        buffer.fill(0);
        // SAFETY: live directory handle and aligned writable buffer of `capacity` bytes.
        if unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                capacity as u32,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                return Ok(());
            }
            return Err(FsError::io(".", error));
        }
        class = FILE_ID_EXTD_DIRECTORY_INFO;
        let mut offset = 0usize;
        loop {
            if offset > capacity - size_of::<ExtendedDirectoryInfo>() {
                return Err(FsError::Unavailable(
                    "malformed extended directory information header",
                ));
            }
            // SAFETY: bounded header in initialized storage.
            let info = unsafe {
                std::ptr::read_unaligned(
                    buffer
                        .as_ptr()
                        .cast::<u8>()
                        .add(offset)
                        .cast::<ExtendedDirectoryInfo>(),
                )
            };
            let name_start = offset + offset_of!(ExtendedDirectoryInfo, file_name);
            let name_len = info.file_name_length as usize;
            if name_len == 0 || !name_len.is_multiple_of(2) || name_len > capacity - name_start {
                return Err(FsError::Unavailable(
                    "malformed extended directory information name",
                ));
            }
            // SAFETY: bounded, even-sized UTF-16 slice.
            let name_wide = unsafe {
                std::slice::from_raw_parts(
                    buffer.as_ptr().cast::<u8>().add(name_start).cast::<u16>(),
                    name_len / 2,
                )
            };
            let name = String::from_utf16_lossy(name_wide);
            if name != "." && name != ".." {
                visit(Candidate { name });
            }
            let next = info.next_entry_offset as usize;
            if next == 0 {
                break;
            }
            if !next.is_multiple_of(8)
                || next < offset_of!(ExtendedDirectoryInfo, file_name) + name_len
                || next > capacity - offset
            {
                return Err(FsError::Unavailable(
                    "malformed extended directory information offset",
                ));
            }
            offset += next;
        }
    }
}

pub(crate) fn sweep(directory: &File) -> Result<usize> {
    sweep_report(directory).map(|report| report.removed)
}

fn sweep_report(directory: &File) -> Result<SweepReport> {
    let mut report = SweepReport {
        retained_counted: true,
        ..SweepReport::default()
    };
    for_each_candidate(directory, |candidate| {
        let Some(pid) = generated_temp_pid(&candidate.name) else {
            return;
        };
        if pid == std::process::id() {
            report.retained_live += 1;
            return;
        }
        match crate::process_liveness(pid) {
            crate::ProcessLiveness::Alive => report.retained_live += 1,
            crate::ProcessLiveness::Unknown => report.retained_unknown += 1,
            // Even a proven-dead PID supplies no ownership proof for this name.
            crate::ProcessLiveness::Dead => report.retained_other += 1,
        }
    })?;
    Ok(report)
}

pub(crate) fn sweep_path_report(path: &Path) -> Result<SweepReport> {
    let directory = match open_directory(path) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => {
            return Ok(SweepReport {
                retained_counted: true,
                ..SweepReport::default()
            })
        }
        Err(error) => return Err(error),
    };
    sweep_report(&directory)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_sweep_preserves_dead_pid_names_and_foreign_replacements() {
        let dir = tempfile::tempdir().unwrap();
        let name = dir.path().join(".x.rktmp-4294967294-1-0");
        let old = dir.path().join("old");
        std::fs::write(&name, b"imported record").unwrap();
        let directory = open_directory(dir.path()).unwrap();
        assert_eq!(sweep(&directory).unwrap(), 0);
        std::fs::rename(&name, &old).unwrap();
        std::fs::write(&name, b"replacement record").unwrap();
        let report = sweep_report(&directory).unwrap();
        assert!(report.retained_counted);
        assert_eq!(report.removed, 0);
        assert_eq!(
            report.retained_live + report.retained_unknown + report.retained_other,
            1
        );
        assert_eq!(std::fs::read(name).unwrap(), b"replacement record");
        assert_eq!(std::fs::read(old).unwrap(), b"imported record");
    }
}
