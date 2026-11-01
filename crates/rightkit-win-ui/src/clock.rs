//! The machine's offset from UTC.

use windows::Win32::System::SystemInformation::GetLocalTime;

/// The machine's offset from UTC in seconds, from the difference between its clock and UTC, in
/// whole quarter hours.
pub fn local_offset_secs() -> i64 {
    // SAFETY: GetLocalTime has no preconditions.
    let now = unsafe { GetLocalTime() };
    let local = i64::from(now.wHour) * 3600 + i64::from(now.wMinute) * 60 + i64::from(now.wSecond);
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let utc = (seconds % 86_400) as i64;
    let mut delta = local - utc;
    if delta > 12 * 3600 {
        delta -= 86_400;
    } else if delta < -12 * 3600 {
        delta += 86_400;
    }
    // Whole quarter hours: the two clocks are read a moment apart.
    (delta as f64 / 900.0).round() as i64 * 900
}
