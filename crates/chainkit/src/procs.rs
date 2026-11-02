//! Whether the game, EMF or Steam runs: dllchain changes nothing while one does.

use crate::game::Game;

/// The lowercase exe names of every running process.
#[cfg(windows)]
pub fn running_exes() -> Vec<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    let mut out = Vec::new();
    // SAFETY: a snapshot handle is closed below; the entry struct is sized as the API asks.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut e) != 0;
        while ok {
            let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push(String::from_utf16_lossy(&e.szExeFile[..n]).to_lowercase());
            ok = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
    }
    out
}

#[cfg(not(windows))]
pub fn running_exes() -> Vec<String> {
    Vec::new()
}

/// Why dllchain must wait, when one of the game's blocking programs is in `running`.
pub fn blocking(game: &Game, running: &[String]) -> Option<String> {
    let found: Vec<&str> =
        game.running.iter().copied().filter(|n| running.iter().any(|r| r.eq_ignore_ascii_case(n))).collect();
    if found.is_empty() {
        return None;
    }
    Some(format!(
        "{} is running. Close it first (for Steam: Steam menu, Exit), then try again: files the game or Steam has open must not change.",
        found.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::ENSHROUDED;

    #[test]
    fn names_what_blocks() {
        assert_eq!(blocking(&ENSHROUDED, &["explorer.exe".into()]), None);
        let why = blocking(&ENSHROUDED, &["Steam.exe".into(), "enshrouded.exe".into()]).unwrap();
        assert!(why.starts_with("enshrouded.exe, steam.exe is running"), "{why}");
    }
}
