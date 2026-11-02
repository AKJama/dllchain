//! Reading the mod the player picked: a zip, or a DLL on its own. Nothing in it is run.

use crate::dllinfo::inspect;
use crate::game::Game;
use crate::hash::sha256_hex;
use crate::paths::{RelPath, slug};
use crate::{OWN_DIR, Result, fail};
use std::io::Read;
use std::path::Path;

pub const MAX_FILES: usize = 2000;
pub const MAX_TOTAL: u64 = 200 * 1024 * 1024;
pub const MAX_DLL: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ModSource {
    /// The file name the player picked.
    pub label: String,
    /// The SHA-256 of that file, to compare with the one the mod's author publishes.
    pub sha256: String,
    /// A word from the file name, for a DLL that does not say who it is.
    pub word_hint: String,
    /// The mod's `dinput8.dll`.
    pub dll: Vec<u8>,
    /// Its other files, relative to the game folder.
    pub files: Vec<(RelPath, Vec<u8>)>,
    /// Zip entries outside the folder that holds the DLL, which are not installed.
    pub ignored: Vec<String>,
}

pub fn read(path: &Path, game: &Game) -> Result<ModSource> {
    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let meta = std::fs::metadata(path).map_err(|e| crate::Error(format!("could not open {}: {e}", path.display())))?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "dll" => {
            if meta.len() > MAX_DLL {
                fail!("{label} is larger than {} MB, which no DLL mod is", MAX_DLL >> 20);
            }
            let dll = std::fs::read(path)?;
            inspect(&dll, game).map_err(|e| crate::Error(format!("{label}: {e}")))?;
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let folder = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
            // a bare dinput8.dll says more with the folder it came from
            let (hint, label) = match folder {
                Some(f) if game.proxy_names.contains(&stem.to_lowercase().as_str()) => {
                    (f.clone(), format!("{f}\\{label}"))
                }
                _ => (stem, label),
            };
            Ok(ModSource {
                label,
                sha256: sha256_hex(&dll),
                word_hint: word_hint(&hint),
                dll,
                files: Vec::new(),
                ignored: Vec::new(),
            })
        }
        "zip" => read_zip(path, &label, game),
        _ => fail!("{label}: pick a mod's .zip or its .dll"),
    }
}

fn read_zip(path: &Path, label: &str, game: &Game) -> Result<ModSource> {
    let bytes = std::fs::read(path)?;
    let sha256 = sha256_hex(&bytes);
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes))
        .map_err(|e| crate::Error(format!("{label} is not a zip dllchain can read: {e}")))?;
    if zip.len() > MAX_FILES {
        fail!("{label} has more than {MAX_FILES} files; that is not a DLL mod");
    }
    let mut entries: Vec<(RelPath, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| crate::Error(format!("{label}: {e}")))?;
        let name = f.name().map_err(|e| crate::Error(format!("{label}: a file name it cannot read: {e}")))?.to_string();
        if f.is_dir() {
            continue;
        }
        if f.encrypted() {
            fail!("{label}: {name} is password-protected");
        }
        if f.is_symlink() {
            fail!("{label}: {name} is a link, which dllchain does not install");
        }
        let rel = RelPath::parse(&name)
            .map_err(|e| crate::Error(format!("{label} has an unsafe path, so nothing was installed: {e}")))?;
        if entries.iter().any(|(r, _)| r.same(&rel)) {
            fail!("{label} has {name} twice");
        }
        let room = (MAX_TOTAL - total).min(MAX_DLL.max(MAX_TOTAL)) + 1;
        let mut data = Vec::new();
        f.by_ref()
            .take(room)
            .read_to_end(&mut data)
            .map_err(|e| crate::Error(format!("{label}: could not unpack {name}: {e}")))?;
        total += data.len() as u64;
        if total > MAX_TOTAL {
            fail!("{label} unpacks to more than {} MB; that is not a DLL mod", MAX_TOTAL >> 20);
        }
        entries.push((rel, data));
    }

    let host = game.host_dll;
    let dlls: Vec<usize> =
        (0..entries.len()).filter(|&i| entries[i].0.file_name().eq_ignore_ascii_case(host)).collect();
    if dlls.is_empty() {
        fail!("{label} has no {host}; dllchain installs mods that ship as {host}");
    }
    if dlls.len() > 1 {
        fail!("{label} has {} files called {host}; dllchain cannot tell which one to install", dlls.len());
    }
    let dll_rel = entries[dlls[0]].0.clone();
    let root = dll_rel.parent();
    let dll = entries[dlls[0]].1.clone();
    if dll.len() as u64 > MAX_DLL {
        fail!("{label}: its {host} is larger than {} MB", MAX_DLL >> 20);
    }
    inspect(&dll, game).map_err(|e| crate::Error(format!("{label}: its {host} is {e}")))?;

    let mut files = Vec::new();
    let mut ignored = Vec::new();
    for (i, (rel, data)) in entries.into_iter().enumerate() {
        if i == dlls[0] {
            continue;
        }
        let inside = match &root {
            None => Some(rel.clone()),
            Some(r) => rel.strip_prefix(r),
        };
        match inside {
            Some(r) => {
                check_target(&r, game).map_err(|e| crate::Error(format!("{label}: {e}")))?;
                files.push((r, data));
            }
            None => ignored.push(rel.to_string()),
        }
    }
    let stem = Path::new(label).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(ModSource { label: label.to_string(), sha256, word_hint: word_hint(&stem), dll, files, ignored })
}

/// A file a zip may not put into the game folder.
pub fn check_target(rel: &RelPath, game: &Game) -> Result<()> {
    let name = rel.file_name().to_lowercase();
    if game.protected_extensions.contains(&rel.extension().as_str()) {
        fail!("it would put {rel} into the game folder; dllchain never installs or replaces programs or game data");
    }
    if game.protected_names.contains(&name.as_str()) {
        fail!("it would replace {rel}, which the game, Steam or a loader owns");
    }
    let first = rel.first_part().to_lowercase();
    if first == OWN_DIR || game.other_tool_dirs.contains(&first.as_str()) {
        fail!("it would write into {first}\\, which belongs to another tool");
    }
    if rel.is_top_level() && rel.extension() == ".dll" {
        let base = name.trim_end_matches(".dll");
        if game.proxy_names.contains(&base) {
            fail!(
                "it also ships {rel}, a proxy DLL that other loaders use; dllchain only installs mods that ship as {}",
                game.host_dll
            );
        }
    }
    Ok(())
}

/// A word from a file name, without its version: "AutoLootPlus-170-1-1-8" gives "autolootplus".
pub fn word_hint(stem: &str) -> String {
    let s = slug(stem);
    let mut parts: Vec<&str> = s.split('-').collect();
    while parts.len() > 1 {
        let last = parts[parts.len() - 1];
        let version = last.chars().all(|c| c.is_ascii_digit())
            || (last.starts_with('v') && last.len() > 1 && last[1..].chars().all(|c| c.is_ascii_digit()))
            || ["beta", "alpha", "rc", "final"].contains(&last);
        if !version {
            break;
        }
        parts.pop();
    }
    let w = parts.join("-");
    if w.is_empty() || crate::game::ENSHROUDED.proxy_names.contains(&w.as_str()) { "dinput8-mod".into() } else { w }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dllinfo::fake_dll;
    use crate::game::ENSHROUDED;
    use std::io::Write;

    pub fn make_zip(path: &Path, files: &[(&str, &[u8])]) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        for (name, data) in files {
            z.start_file(
                *name,
                zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
            )
            .unwrap();
            z.write_all(data).unwrap();
        }
        z.finish().unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("chainkit-src-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn reads_a_zip_with_a_top_folder() {
        let dll = fake_dll(&["auto-loot-plus"], None);
        let p = tmp("AutoLootPlus-170-1-1-8.zip");
        make_zip(
            &p,
            &[
                ("readme.txt", b"r"),
                ("Auto Loot Plus/dinput8.dll", &dll),
                ("Auto Loot Plus/mods/auto-loot-plus/config.txt", b"c"),
            ],
        );
        let s = read(&p, &ENSHROUDED).unwrap();
        assert_eq!(s.word_hint, "autolootplus");
        assert_eq!(s.dll, dll);
        assert_eq!(s.files.len(), 1);
        assert_eq!(s.files[0].0.as_str(), "mods\\auto-loot-plus\\config.txt");
        assert_eq!(s.ignored, vec!["readme.txt".to_string()]);
    }

    #[test]
    fn refuses_hostile_zips() {
        let dll = fake_dll(&[], None);
        type Case<'a> = (&'a str, Vec<(&'a str, &'a [u8])>);
        let cases: Vec<Case> = vec![
            ("dotdot", vec![("dinput8.dll", &dll), ("../evil.dll", b"x")]),
            ("abs", vec![("dinput8.dll", &dll), ("C:/Windows/evil.dll", b"x")]),
            ("dev", vec![("dinput8.dll", &dll), ("CON.txt", b"x")]),
            ("exe", vec![("dinput8.dll", &dll), ("enshrouded.exe", b"x")]),
            ("kfc", vec![("dinput8.dll", &dll), ("enshrouded.kfc", b"x")]),
            ("chain", vec![("dinput8.dll", &dll), ("dinput8-chain.ini", b"x")]),
            ("own", vec![("dinput8.dll", &dll), ("dllchain/record.txt", b"x")]),
            ("emf", vec![("dinput8.dll", &dll), ("emf/moved.txt", b"x")]),
            ("proxy", vec![("dinput8.dll", &dll), ("winmm.dll", &dll)]),
            ("two", vec![("a/dinput8.dll", &dll), ("b/dinput8.dll", &dll)]),
            ("none", vec![("mod.dll", &dll)]),
            ("notdll", vec![("dinput8.dll", b"MZ not really")]),
        ];
        for (name, files) in cases {
            let p = tmp(&format!("{name}.zip"));
            make_zip(&p, &files);
            assert!(read(&p, &ENSHROUDED).is_err(), "{name} should be refused");
        }
    }

    #[test]
    fn a_bare_dll() {
        let dir = tmp("Some Mod");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("dinput8.dll");
        std::fs::write(&p, fake_dll(&[], None)).unwrap();
        let s = read(&p, &ENSHROUDED).unwrap();
        assert_eq!(s.word_hint, "some-mod");
        assert!(s.files.is_empty());
    }

    #[test]
    fn version_tails() {
        assert_eq!(word_hint("SmartQuickStack-0.9.0-beta"), "smartquickstack");
        assert_eq!(word_hint("xhl-v2"), "xhl");
        assert_eq!(word_hint("dinput8"), "dinput8-mod");
        assert_eq!(word_hint("1.2.3"), "1");
    }
}
