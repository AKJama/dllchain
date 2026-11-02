//! Finding the game through Steam: `libraryfolders.vdf` names the libraries, `appmanifest_<id>.acf` the folder.

use crate::game::Game;
use crate::paths::RelPath;
use std::path::{Path, PathBuf};

/// Every `"path"` value in `libraryfolders.vdf`.
pub fn library_paths(vdf: &str) -> Vec<PathBuf> {
    vdf.lines()
        .filter_map(|l| {
            let q = quoted(l);
            (q.len() >= 2 && q[0].eq_ignore_ascii_case("path")).then(|| PathBuf::from(q[1].replace("\\\\", "\\")))
        })
        .collect()
}

/// The `"installdir"` value of an app manifest.
pub fn install_dir(acf: &str) -> Option<String> {
    acf.lines().find_map(|l| {
        let q = quoted(l);
        (q.len() >= 2 && q[0].eq_ignore_ascii_case("installdir")).then(|| q[1].clone())
    })
}

/// The quoted strings of a VDF line, with `\"` and `\\` kept as written.
fn quoted(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match (&mut cur, c) {
            (None, '"') => cur = Some(String::new()),
            (Some(s), '\\') => {
                s.push('\\');
                if let Some(n) = chars.next() {
                    s.push(n);
                }
            }
            (Some(_), '"') => out.push(cur.take().unwrap_or_default()),
            (Some(s), c) => s.push(c),
            (None, _) => {}
        }
    }
    out
}

/// Every folder under `steam_root`'s libraries that has the game's manifest and exe.
pub fn find_game(steam_root: &Path, game: &Game) -> Vec<PathBuf> {
    let mut libs = vec![steam_root.to_path_buf()];
    if let Ok(vdf) = std::fs::read_to_string(steam_root.join("steamapps").join("libraryfolders.vdf")) {
        for p in library_paths(&vdf) {
            if !libs.iter().any(|l| same_dir(l, &p)) {
                libs.push(p);
            }
        }
    }
    let mut found: Vec<PathBuf> = Vec::new();
    for lib in libs {
        let acf = lib.join("steamapps").join(format!("appmanifest_{}.acf", game.steam_app_id));
        let Ok(text) = std::fs::read_to_string(&acf) else {
            continue;
        };
        let Some(dir) = install_dir(&text) else {
            continue;
        };
        // one plain folder name: a manifest must not point anywhere else
        if RelPath::parse(&dir).map(|r| !r.is_top_level()).unwrap_or(true) {
            continue;
        }
        let at = lib.join("steamapps").join("common").join(dir);
        if is_game_folder(&at, game) && !found.iter().any(|f| same_dir(f, &at)) {
            found.push(at);
        }
    }
    found
}

pub fn is_game_folder(dir: &Path, game: &Game) -> bool {
    game.exes.iter().any(|e| dir.join(e).is_file())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches(['\\', '/']))
}

/// Steam's folder from the registry (`SteamPath`, else `InstallPath`).
#[cfg(windows)]
pub fn steam_root_from_registry() -> Option<PathBuf> {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    let tries = [
        (HKEY_CURRENT_USER, "Software\\Valve\\Steam", "SteamPath"),
        (HKEY_LOCAL_MACHINE, "SOFTWARE\\WOW6432Node\\Valve\\Steam", "InstallPath"),
    ];
    for (root, key, value) in tries {
        let key_w: Vec<u16> = key.encode_utf16().chain([0]).collect();
        let value_w: Vec<u16> = value.encode_utf16().chain([0]).collect();
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: the buffers outlive the call and `size` is their length in bytes.
        let rc = unsafe {
            RegGetValueW(
                root,
                key_w.as_ptr(),
                value_w.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if rc == 0 {
            let n = (size as usize / 2).saturating_sub(1);
            let s = String::from_utf16_lossy(&buf[..n]).replace('/', "\\");
            if !s.is_empty() {
                return Some(PathBuf::from(s));
            }
        }
    }
    None
}

#[cfg(not(windows))]
pub fn steam_root_from_registry() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::ENSHROUDED;

    #[test]
    fn reads_vdf_and_acf() {
        let vdf = "\"libraryfolders\"\n{\n\t\"0\"\n\t{\n\t\t\"path\"\t\t\"C:\\\\Program Files (x86)\\\\Steam\"\n\t}\n\t\"1\"\n\t{\n\t\t\"path\"\t\t\"D:\\\\SteamLibrary\"\n\t\t\"apps\" { \"1203620\" \"1\" }\n\t}\n}\n";
        assert_eq!(
            library_paths(vdf),
            vec![PathBuf::from("C:\\Program Files (x86)\\Steam"), PathBuf::from("D:\\SteamLibrary")]
        );
        assert_eq!(install_dir("\"AppState\"\n{\n\t\"installdir\"\t\t\"Enshrouded\"\n}"), Some("Enshrouded".into()));
    }

    #[test]
    fn finds_the_game_in_a_second_library() {
        let root = std::env::temp_dir().join(format!("chainkit-steam-{}", std::process::id()));
        let steam = root.join("Steam");
        let lib = root.join("Lib");
        std::fs::create_dir_all(steam.join("steamapps")).unwrap();
        std::fs::create_dir_all(lib.join("steamapps\\common\\Enshrouded")).unwrap();
        std::fs::create_dir_all(lib.join("steamapps\\common\\Bad")).unwrap();
        std::fs::write(lib.join("steamapps\\common\\Enshrouded\\enshrouded.exe"), b"").unwrap();
        let vdf = format!(
            "\"libraryfolders\"\n{{\n\"1\"\n{{\n\"path\" \"{}\"\n}}\n}}\n",
            lib.display().to_string().replace('\\', "\\\\")
        );
        std::fs::write(steam.join("steamapps\\libraryfolders.vdf"), vdf).unwrap();
        std::fs::write(
            lib.join("steamapps\\appmanifest_1203620.acf"),
            "\"AppState\"\n{\n\"installdir\" \"Enshrouded\"\n}\n",
        )
        .unwrap();
        assert_eq!(find_game(&steam, &ENSHROUDED), vec![lib.join("steamapps\\common\\Enshrouded")]);
        // a manifest pointing outside its library is ignored
        std::fs::write(lib.join("steamapps\\appmanifest_1203620.acf"), "\"installdir\" \"..\\\\..\\\\Bad\"\n").unwrap();
        assert!(find_game(&steam, &ENSHROUDED).is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
