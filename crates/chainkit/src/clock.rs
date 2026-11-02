//! The local time, for record lines and folder names.

/// `2026-11-03 14:05:09`
pub fn now_text() -> String {
    let (y, mo, d, h, mi, s) = now();
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}

/// `20261103-140509`, for folder names.
pub fn now_stamp() -> String {
    let (y, mo, d, h, mi, s) = now();
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

#[cfg(windows)]
fn now() -> (u16, u16, u16, u16, u16, u16) {
    use windows_sys::Win32::System::SystemInformation::GetLocalTime;
    // SAFETY: GetLocalTime fills the struct it is given.
    let t = unsafe {
        let mut t = std::mem::zeroed();
        GetLocalTime(&mut t);
        t
    };
    (t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

#[cfg(not(windows))]
fn now() -> (u16, u16, u16, u16, u16, u16) {
    (1970, 1, 1, 0, 0, 0)
}
