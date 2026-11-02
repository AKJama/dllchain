//! Reading the mod the player picked: a zip, an unpacked mod folder (its `dinput8.dll` picked), or a DLL on its own.
//! Nothing in it is run.

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
    /// The file name the player picked; for an unpacked mod folder, the folder's name and a `\`.
    pub label: String,
    /// The SHA-256 of that file (of the DLL, for a folder), to compare with the one the mod's author publishes.
    pub sha256: String,
    /// A word from the file name, for a DLL that does not say who it is.
    pub word_hint: String,
    /// The mod's `dinput8.dll`.
    pub dll: Vec<u8>,
    /// Its other files, relative to the game folder.
    pub files: Vec<(RelPath, Vec<u8>)>,
    /// Zip entries outside the folder that holds the DLL, which are not installed.
    pub ignored: Vec<String>,
    /// Files next to the DLL that are for the player to read, not for the game (licences, checksums, read-me files),
    /// which are not installed either; see [`release_extra`].
    pub extras: Vec<String>,
    /// Where the file was downloaded from, from the note Windows keeps with a download (`Zone.Identifier`).
    pub from_url: Option<String>,
}

pub fn read(path: &Path, game: &Game) -> Result<ModSource> {
    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let from_url = downloaded_from(path);
    let meta = std::fs::metadata(path).map_err(|e| crate::Error(format!("could not open {}: {e}", path.display())))?;
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "dll" => {
            if meta.len() > MAX_DLL {
                fail!("{label} is larger than {} MB, which no DLL mod is", MAX_DLL >> 20);
            }
            // a dinput8.dll with the rest of its mod around it: the folder is the mod
            let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let mut extras = Vec::new();
            if label.eq_ignore_ascii_case(game.host_dll) {
                match beside(dir, &label) {
                    Beside::Mod => return Ok(ModSource { from_url, ..read_folder(dir, game)? }),
                    Beside::Extras(e) => extras = e,
                }
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
                extras,
                from_url,
            })
        }
        "zip" => Ok(ModSource { from_url, ..read_zip(path, &label, game)? }),
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
    let stem = Path::new(label).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    from_entries(entries, label, sha256, &stem, game)
}

/// The mod in a zip's or a folder's files: its one `dinput8.dll`, and the files next to it and below.
fn from_entries(
    entries: Vec<(RelPath, Vec<u8>)>,
    label: &str,
    sha256: String,
    hint: &str,
    game: &Game,
) -> Result<ModSource> {
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
    let mut extras = Vec::new();
    for (i, (rel, data)) in entries.into_iter().enumerate() {
        if i == dlls[0] {
            continue;
        }
        let inside = match &root {
            None => Some(rel.clone()),
            Some(r) => rel.strip_prefix(r),
        };
        match inside {
            Some(r) if release_extra(&r) => extras.push(r.to_string()),
            Some(r) => {
                check_target(&r, game).map_err(|e| crate::Error(format!("{label}: {e}")))?;
                files.push((r, data));
            }
            None => ignored.push(rel.to_string()),
        }
    }
    Ok(ModSource {
        label: label.to_string(),
        sha256,
        word_hint: word_hint(hint),
        dll,
        files,
        ignored,
        extras,
        from_url: None,
    })
}

/// Files Explorer keeps in folders by itself, never part of a mod.
fn explorer_file(name: &str) -> bool {
    ["desktop.ini", "thumbs.db"].contains(&name.to_lowercase().as_str())
}

/// A file a mod's download carries for the player, not for the game: its licences, checksums, signatures and read-me
/// files, next to its `dinput8.dll`. They are not installed, so they never lie loose in the game folder, and an
/// update that ships only them and the DLL is a DLL-only update. Files in the mod's own folders always count as the
/// mod's.
pub fn release_extra(rel: &RelPath) -> bool {
    if !rel.is_top_level() {
        return false;
    }
    let name = rel.file_name().to_lowercase();
    let ext = rel.extension();
    let stem = name.strip_suffix(ext.as_str()).unwrap_or(&name);
    let checksums =
        ["sha256sums", "sha512sums", "sha1sums", "md5sums", "checksums"].iter().any(|c| stem.starts_with(c))
            || [".sha256", ".sha512", ".sha1", ".md5", ".sig", ".asc", ".minisig"].contains(&ext.as_str());
    let papers = ["license", "licence", "readme", "changelog", "copying", "notice", "third-party", "third_party"]
        .iter()
        .any(|w| stem.contains(w))
        && ["", ".txt", ".md", ".markdown", ".rst", ".html", ".htm", ".pdf"].contains(&ext.as_str());
    checksums || papers
}

/// What lies next to a picked `dinput8.dll`.
enum Beside {
    /// The rest of a mod unpacked there: the folder is the mod.
    Mod,
    /// Nothing but these release extras (and Explorer's own files): the DLL is the whole mod.
    Extras(Vec<String>),
}

fn beside(dir: &Path, dll_name: &str) -> Beside {
    let Ok(list) = std::fs::read_dir(dir) else { return Beside::Extras(Vec::new()) };
    let mut extras = Vec::new();
    for e in list.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.eq_ignore_ascii_case(dll_name) || explorer_file(&name) {
            continue;
        }
        let file = e.file_type().is_ok_and(|t| t.is_file());
        match RelPath::parse(&name) {
            Ok(r) if file && release_extra(&r) => extras.push(name),
            _ => return Beside::Mod,
        }
    }
    extras.sort();
    Beside::Extras(extras)
}

/// Files that are never part of a DLL mod wherever they are in its folder: programs, archives, disk images and
/// unfinished downloads. A folder holding one is a downloads folder, not one mod.
const NOT_A_MOD_ANYWHERE: &[&str] = &[
    "exe",
    "msi",
    "msix",
    "appx",
    "zip",
    "7z",
    "rar",
    "tar",
    "gz",
    "tgz",
    "bz2",
    "xz",
    "cab",
    "iso",
    "img",
    "vhd",
    "vhdx",
    "torrent",
    "crdownload",
    "part",
    "partial",
];

/// Files no mod puts next to its `dinput8.dll`, but that a downloads or desktop folder is full of: pictures, music,
/// video and office documents. (A mod may keep pictures or sounds in its own folders.)
const NOT_A_MOD_BESIDE_THE_DLL: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "webp", "bmp", "heic", "svg", "mp3", "wav", "flac", "m4a", "aac", "mp4", "mkv", "avi",
    "mov", "webm", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "csv", "epub",
];

/// A `dinput8.dll` in an unpacked mod folder: the folder is the mod, as if it were the zip. Refused when the folder
/// looks like more than one mod's files (programs or archives in it, or a folder like Downloads itself).
fn read_folder(dir: &Path, game: &Game) -> Result<ModSource> {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let label = format!("{name}\\");
    let own_folder = "Pick the mod's zip, or put the mod's files in a folder of their own first.";
    if is_personal_folder(dir) {
        fail!("{} is loose in {}, not in a folder of its own. {own_folder}", game.host_dll, dir.display());
    }
    let mut entries: Vec<(RelPath, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let list = std::fs::read_dir(&d).map_err(|e| crate::Error(format!("could not read {}: {e}", d.display())))?;
        let mut items = list.collect::<std::io::Result<Vec<_>>>()?;
        // the same order every time, as in a zip
        items.sort_by_key(|i| i.file_name());
        for item in items {
            let p = item.path();
            let file = item.file_name().to_string_lossy().into_owned();
            let meta = std::fs::symlink_metadata(&p)?;
            if meta.file_type().is_symlink() || is_reparse_point(&meta) {
                fail!("{name}: {file} is a link, which dllchain does not install");
            }
            if meta.is_dir() {
                todo.push(p);
                continue;
            }
            if explorer_file(&file) {
                continue;
            }
            let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            if NOT_A_MOD_ANYWHERE.contains(&ext.as_str())
                || (d == dir && NOT_A_MOD_BESIDE_THE_DLL.contains(&ext.as_str()))
            {
                fail!("{name} also holds {file}, so it is not one mod's unpacked folder. {own_folder}");
            }
            if entries.len() >= MAX_FILES {
                fail!("{name} has more than {MAX_FILES} files; that is not a DLL mod");
            }
            total += meta.len();
            if total > MAX_TOTAL {
                fail!("{name} holds more than {} MB; that is not a DLL mod", MAX_TOTAL >> 20);
            }
            let rel = p.strip_prefix(dir).map_err(|_| crate::Error(format!("{name}: {file} is outside it")))?;
            let rel = RelPath::parse(&rel.to_string_lossy())
                .map_err(|e| crate::Error(format!("{name} has a file dllchain cannot install safely: {e}")))?;
            entries.push((rel, std::fs::read(&p)?));
        }
    }
    let mut s = from_entries(entries, &label, String::new(), &name, game)?;
    s.sha256 = sha256_hex(&s.dll);
    Ok(s)
}

/// Drive roots and the player's own folders, where loose files are not one mod.
fn is_personal_folder(dir: &Path) -> bool {
    let real = |p: &Path| std::fs::canonicalize(p).ok();
    let Some(d) = real(dir) else { return false };
    d.parent().is_none() || personal_folders().iter().any(|f| real(f).is_some_and(|f| f == d))
}

/// The profile folder, Downloads, Desktop and Documents, wherever Windows keeps them.
fn personal_folders() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE").map(std::path::PathBuf::from) {
        for sub in ["Downloads", "Desktop", "Documents", r"OneDrive\Desktop", r"OneDrive\Documents"] {
            out.push(home.join(sub));
        }
        out.push(home);
    }
    out.extend(known_folders());
    out
}

#[cfg(windows)]
fn known_folders() -> Vec<std::path::PathBuf> {
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Profile, SHGetKnownFolderPath,
    };
    let mut out = Vec::new();
    for id in [FOLDERID_Downloads, FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Profile] {
        let mut p = std::ptr::null_mut();
        // SAFETY: a known folder's id; the path Windows returns is read up to its NUL and freed with CoTaskMemFree.
        unsafe {
            if SHGetKnownFolderPath(&id, 0, std::ptr::null_mut(), &mut p) == 0 && !p.is_null() {
                let n = (0..).take_while(|&i| *p.add(i) != 0).count();
                out.push(std::path::PathBuf::from(String::from_utf16_lossy(std::slice::from_raw_parts(p, n))));
            }
            CoTaskMemFree(p as *const std::ffi::c_void);
        }
    }
    out
}

#[cfg(not(windows))]
fn known_folders() -> Vec<std::path::PathBuf> {
    Vec::new()
}

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    // FILE_ATTRIBUTE_REPARSE_POINT: junctions and other links
    meta.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

/// The web page or address a downloaded file came from. Browsers save it next to the file, in the
/// `Zone.Identifier` stream Windows uses for its "downloaded from the internet" warning; the page it was linked from
/// (`ReferrerUrl`) is preferred over the download link (`HostUrl`). Only the site and path are kept.
pub fn downloaded_from(path: &Path) -> Option<String> {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let bytes = std::fs::read(&stream).ok()?;
    if bytes.len() > 64 * 1024 {
        return None;
    }
    zone_url(&String::from_utf8_lossy(&bytes))
}

fn zone_url(text: &str) -> Option<String> {
    let value = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            k.trim().eq_ignore_ascii_case(key).then(|| v.trim().to_string())
        })
    };
    ["ReferrerUrl", "HostUrl"].into_iter().find_map(|k| value(k).and_then(|v| crate::record::clean_url(&v)))
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
        assert_eq!(s.label, "Some Mod\\dinput8.dll");
        assert!(s.files.is_empty());
        // Explorer's own files do not make it a mod folder
        std::fs::write(dir.join("desktop.ini"), "[.ShellClassInfo]").unwrap();
        assert_eq!(read(&p, &ENSHROUDED).unwrap().label, "Some Mod\\dinput8.dll");
    }

    #[test]
    fn an_unpacked_mod_folder() {
        let dir = tmp("AutoLootPlus 1.1.8 170");
        let dll = fake_dll(&["auto-loot-plus"], None);
        std::fs::create_dir_all(dir.join("mods\\auto-loot-plus")).unwrap();
        std::fs::create_dir_all(dir.join("licenses")).unwrap();
        std::fs::write(dir.join("dinput8.dll"), &dll).unwrap();
        std::fs::write(dir.join("mods\\auto-loot-plus\\mod.lua"), "m").unwrap();
        std::fs::write(dir.join("mods\\auto-loot-plus\\config.txt"), "c").unwrap();
        std::fs::write(dir.join("licenses\\LICENSE.txt"), "l").unwrap();
        std::fs::write(dir.join("Thumbs.db"), "t").unwrap();
        let s = read(&dir.join("dinput8.dll"), &ENSHROUDED).unwrap();
        assert_eq!(s.label, "AutoLootPlus 1.1.8 170\\");
        assert_eq!(s.sha256, sha256_hex(&dll));
        assert_eq!(s.dll, dll);
        let mut names: Vec<&str> = s.files.iter().map(|f| f.0.as_str()).collect();
        names.sort();
        assert_eq!(
            names,
            ["licenses\\LICENSE.txt", "mods\\auto-loot-plus\\config.txt", "mods\\auto-loot-plus\\mod.lua"]
        );
        assert!(s.ignored.is_empty());
    }

    #[test]
    fn refuses_folders_that_are_not_one_mod() {
        let dll = fake_dll(&[], None);
        let cases: [(&str, &str); 4] =
            [("dl-exe", "setup.exe"), ("dl-zip", "Other Mod.zip"), ("dl-7z", "x.7z"), ("dl-game", "enshrouded.kfc")];
        for (name, other) in cases {
            let dir = tmp(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("dinput8.dll"), &dll).unwrap();
            std::fs::write(dir.join("notes.txt"), "n").unwrap();
            std::fs::write(dir.join(other), "x").unwrap();
            let e = read(&dir.join("dinput8.dll"), &ENSHROUDED).unwrap_err();
            assert!(e.0.contains(other) || e.0.contains("game data"), "{name}: {}", e.0);
        }
        // a second dinput8.dll below: which is the mod?
        let dir = tmp("dl-two");
        std::fs::create_dir_all(dir.join("old")).unwrap();
        std::fs::write(dir.join("dinput8.dll"), &dll).unwrap();
        std::fs::write(dir.join("old\\dinput8.dll"), &dll).unwrap();
        assert!(read(&dir.join("dinput8.dll"), &ENSHROUDED).is_err());
    }

    #[test]
    fn a_release_folder_with_only_the_dll_is_the_dll() {
        // the fix2 release: the DLL, its licences and its checksums, nothing for the game folder but the DLL
        let dir = tmp("alp-fix2");
        std::fs::create_dir_all(&dir).unwrap();
        let dll = fake_dll(&["auto-loot-plus"], None);
        std::fs::write(dir.join("dinput8.dll"), &dll).unwrap();
        for f in ["SHA256SUMS.txt", "AutoLootPlus-LICENSE.txt", "DearImGui-LICENSE.txt", "Vulkan-Headers-LICENSE.md"] {
            std::fs::write(dir.join(f), "x").unwrap();
        }
        let s = read(&dir.join("dinput8.dll"), &ENSHROUDED).unwrap();
        assert_eq!(s.label, "alp-fix2\\dinput8.dll");
        assert!(s.files.is_empty());
        assert_eq!(
            s.extras,
            ["AutoLootPlus-LICENSE.txt", "DearImGui-LICENSE.txt", "SHA256SUMS.txt", "Vulkan-Headers-LICENSE.md"]
        );
        // with the mod's own folder beside it, it is a full mod folder, and the extras still stay out
        std::fs::create_dir_all(dir.join("mods\\auto-loot-plus")).unwrap();
        std::fs::write(dir.join("mods\\auto-loot-plus\\mod.lua"), "m").unwrap();
        std::fs::write(dir.join("AutoLootPlus-README.txt"), "r").unwrap();
        let s = read(&dir.join("dinput8.dll"), &ENSHROUDED).unwrap();
        assert_eq!(s.label, "alp-fix2\\");
        assert_eq!(s.files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["mods\\auto-loot-plus\\mod.lua"]);
        assert_eq!(s.extras.len(), 5);
    }

    #[test]
    fn release_extras_only_next_to_the_dll() {
        let extra = |p: &str| release_extra(&RelPath::parse(p).unwrap());
        for p in
            ["SHA256SUMS.txt", "sha256sums", "LICENSE", "LICENCE-MIT.md", "AutoLootPlus-README.txt", "x.zip.sha256"]
        {
            assert!(extra(p), "{p}");
        }
        for p in ["licenses\\LICENSE.txt", "enstash-data.txt", "config.ini", "readme.lua", "license.dll"] {
            assert!(!extra(p), "{p}");
        }
        // a zip with its licence next to the DLL installs only the DLL and the mod's folder
        let p = tmp("with-extras.zip");
        let dll = fake_dll(&[], None);
        make_zip(&p, &[("m/dinput8.dll", &dll), ("m/LICENSE.txt", b"l"), ("m/mods/m/a.lua", b"a")]);
        let s = read(&p, &ENSHROUDED).unwrap();
        assert_eq!(s.files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), ["mods\\m\\a.lua"]);
        assert_eq!(s.extras, ["LICENSE.txt"]);
    }

    #[test]
    fn a_crowded_folder_is_not_swept_in() {
        let dll = fake_dll(&[], None);
        for (name, other) in [("crowd-jpg", "holiday.jpg"), ("crowd-docx", "cv.docx"), ("crowd-iso", "win.iso")] {
            let dir = tmp(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("dinput8.dll"), &dll).unwrap();
            std::fs::write(dir.join(other), "x").unwrap();
            let e = read(&dir.join("dinput8.dll"), &ENSHROUDED).unwrap_err();
            assert!(e.0.contains(other), "{name}: {}", e.0);
        }
        // a mod may keep pictures in its own folder
        let dir = tmp("mod-with-icons");
        std::fs::create_dir_all(dir.join("mods\\m\\icons")).unwrap();
        std::fs::write(dir.join("dinput8.dll"), &dll).unwrap();
        std::fs::write(dir.join("mods\\m\\icons\\a.png"), "p").unwrap();
        assert_eq!(read(&dir.join("dinput8.dll"), &ENSHROUDED).unwrap().files.len(), 1);
    }

    #[test]
    fn the_players_own_folders_are_not_a_mod() {
        let home = std::path::PathBuf::from(std::env::var_os("USERPROFILE").unwrap());
        assert!(is_personal_folder(&home));
        assert!(is_personal_folder(&home.join("Downloads")) || !home.join("Downloads").exists());
        let root = home.ancestors().last().unwrap();
        assert!(is_personal_folder(root));
        assert!(!is_personal_folder(&std::env::temp_dir()));
    }

    #[test]
    fn where_a_download_came_from() {
        let dir = tmp("downloads");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Mod-1-0.zip");
        make_zip(&p, &[("dinput8.dll", &fake_dll(&[], None))]);
        assert_eq!(read(&p, &ENSHROUDED).unwrap().from_url, None);
        let mut stream = p.as_os_str().to_owned();
        stream.push(":Zone.Identifier");
        std::fs::write(
            &stream,
            "[ZoneTransfer]\r\nZoneId=3\r\nReferrerUrl=https://www.nexusmods.com/enshrouded/mods/170?tab=files\r\n\
             HostUrl=https://cf-files.nexusmods.com/x/Mod.zip?user_id=123&key=abc\r\n",
        )
        .unwrap();
        let from = read(&p, &ENSHROUDED).unwrap().from_url;
        assert_eq!(from.as_deref(), Some("https://www.nexusmods.com/enshrouded/mods/170"));
        assert_eq!(
            zone_url("[ZoneTransfer]\nZoneId=3\nReferrerUrl=about:blank\nHostUrl=https://example.com/a.zip?t=1"),
            Some("https://example.com/a.zip".into())
        );
        assert_eq!(zone_url("[ZoneTransfer]\nZoneId=3\n"), None);
    }

    #[test]
    fn version_tails() {
        assert_eq!(word_hint("SmartQuickStack-0.9.0-beta"), "smartquickstack");
        assert_eq!(word_hint("xhl-v2"), "xhl");
        assert_eq!(word_hint("dinput8"), "dinput8-mod");
        assert_eq!(word_hint("1.2.3"), "1");
    }
}
