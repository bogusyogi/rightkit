#[cfg(windows)]
static ORIGINAL_VT_INPUT: std::sync::Mutex<Vec<VirtualTerminalInput>> =
    std::sync::Mutex::new(Vec::new());

#[cfg(windows)]
fn current_input_mode() -> Option<(windows_sys::Win32::Foundation::HANDLE, u32)> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::GetConsoleMode;
    use windows_sys::Win32::System::Console::GetStdHandle;
    use windows_sys::Win32::System::Console::STD_INPUT_HANDLE;

    let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }

    let mut mode = 0;
    if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
        return None;
    }

    Some((handle, mode))
}

#[cfg(windows)]
pub(super) fn set_input_record_mode() -> std::io::Result<()> {
    use windows_sys::Win32::System::Console::SetConsoleMode;

    let Some((handle, mode)) = current_input_mode() else {
        return Ok(());
    };
    let requested_mode = input_record_mode(mode);
    if requested_mode != mode && unsafe { SetConsoleMode(handle, requested_mode) } == 0 {
        return Err(std::io::Error::last_os_error());
    }

    let original = if mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0 {
        VirtualTerminalInput::Enabled
    } else {
        VirtualTerminalInput::Disabled
    };
    ORIGINAL_VT_INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(original);
    Ok(())
}

#[cfg(windows)]
pub(super) fn ensure_input_record_mode() -> std::io::Result<()> {
    use windows_sys::Win32::System::Console::SetConsoleMode;

    let Some((handle, mode)) = current_input_mode() else {
        return Ok(());
    };
    let requested_mode = input_record_mode(mode);
    if requested_mode != mode && unsafe { SetConsoleMode(handle, requested_mode) } == 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(windows)]
pub(super) fn restore_input_mode() -> std::io::Result<()> {
    use windows_sys::Win32::System::Console::SetConsoleMode;

    let mut original_modes = ORIGINAL_VT_INPUT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(original) = original_modes.last().copied() else {
        return Ok(());
    };
    let Some((handle, mode)) = current_input_mode() else {
        original_modes.pop();
        return Ok(());
    };
    let requested_mode = restored_input_mode(mode, original);
    if requested_mode != mode && unsafe { SetConsoleMode(handle, requested_mode) } == 0 {
        return Err(std::io::Error::last_os_error());
    }

    original_modes.pop();
    Ok(())
}

/// Text of one visible screen row (0-based from the window top), with
/// trailing blanks trimmed. `None` when the console cannot be read.
#[cfg(windows)]
pub(super) fn read_visible_row(row: u16, columns: u16) -> Option<String> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, COORD, GetConsoleScreenBufferInfo, GetStdHandle,
        ReadConsoleOutputCharacterW, STD_OUTPUT_HANDLE,
    };

    let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return None;
    }
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } == 0 {
        return None;
    }
    let width = columns.min(u16::try_from(info.dwSize.X).unwrap_or(0));
    if width == 0 {
        return None;
    }
    let y = info.srWindow.Top.checked_add(i16::try_from(row).ok()?)?;
    let mut buffer = vec![0u16; usize::from(width)];
    let mut read = 0u32;
    let ok = unsafe {
        ReadConsoleOutputCharacterW(
            handle,
            buffer.as_mut_ptr(),
            u32::from(width),
            COORD { X: 0, Y: y },
            &mut read,
        )
    };
    if ok == 0 {
        return None;
    }
    buffer.truncate(read as usize);
    Some(String::from_utf16_lossy(&buffer).trim_end().to_string())
}
