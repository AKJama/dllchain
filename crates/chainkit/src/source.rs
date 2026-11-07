//! Reading the mod the player picked: a zip, an unpacked mod folder (its DLL picked), or a DLL on its own. Nothing in
//! it is run.
//!
//! A mod is the folder that holds its DLL: `dinput8.dll` for a mod that joins the chain, else a DLL with another
//! loader name (`dbgcore.dll`, `powrprof.dll`, see [`Game::proxy_names`]) for a mod that brings its own loader. A zip
//! with a `client` and a `server` folder side by side is a mod with a dedicated-server part: only the client part is
//! the mod. A checksum list in the zip (`SHA256.json`, `SHA256SUMS.txt`) is checked against every file installed.

use crate::dllinfo::inspect;
use crate::game::Game;
use crate::hash::{is_sha256, sha256_hex};
use crate::paths::{RelPath, slug};
use crate::{OWN_DIR, Result, fail};
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;

pub const MAX_FILES: usize = 2000;
pub const MAX_TOTAL: u64 = 200 * 1024 * 1024;
pub const MAX_DLL: u64 = 64 * 1024 * 1024;

/// The folders a mod with a dedicated-server part ships side by side: the first is the game's.
const CLIENT_SERVER: (&str, &str) = ("client", "server");

#[derive(Clone, Debug)]
pub struct ModSource {
    /// The file name the player picked; for an unpacked mod folder, the folder's name and a `\`.
    pub label: String,
    /// The SHA-256 of that file (of the DLL, for a folder), to compare with the one the mod's author publishes.
    pub sha256: String,
    /// A word from the file name, for a DLL that does not say who it is.
    pub word_hint: String,
    /// The name the mod's DLL ships as: the host's (`dinput8.dll`) for a mod that joins the chain, or another loader
    /// name (`dbgcore.dll`) for a mod that brings its own loader, which is installed under that name.
    pub dll_name: String,
    /// The mod's DLL.
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
    /// What the mod's own `mod.json` says it is, for a mod that brings its own loader (whose DLL often names only the
    /// loader).
    pub about: Option<About>,
    /// What dllchain found in the zip worth saying: which part of it is installed, and its checksum list checked.
    pub notes: Vec<String>,
}

/// A mod's `mod.json`: its id, the name players see and its version, as far as it gives them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct About {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
}

impl ModSource {
    /// Whether the mod brings its own loader instead of joining the chain as `dinput8.dll`.
    pub fn own_loader(&self, game: &Game) -> bool {
        game.is_own_loader_name(&self.dll_name)
    }
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
            // a dinput8.dll (or a loader of a mod's own) with the rest of its mod around it: the folder is the mod
            let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let own_loader = game.is_own_loader_name(&label);
            let mut extras = Vec::new();
            if label.eq_ignore_ascii_case(game.host_dll) || own_loader {
                match beside(dir, &label) {
                    Beside::Mod => return Ok(ModSource { from_url, ..read_folder(dir, &label, game)? }),
                    Beside::Extras(e) => extras = e,
                }
            }
            let dll = std::fs::read(path)?;
            inspect(&dll, game).map_err(|e| crate::Error(format!("{label}: {e}")))?;
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let folder = path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
            let dll_name = if own_loader { label.to_ascii_lowercase() } else { game.host_dll.to_string() };
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
                dll_name,
                dll,
                files: Vec::new(),
                ignored: Vec::new(),
                extras,
                from_url,
                about: None,
                notes: Vec::new(),
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
    from_entries(entries, label, sha256, &stem, game, None)
}

/// The mod in a zip's or a folder's files: the folder that holds its one `dinput8.dll` (or, without one, its loader
/// under another of the game's loader names, nearest the top), and everything in it. `picked` is the DLL the player
/// picked in an unpacked folder, which is then the mod's.
fn from_entries(
    entries: Vec<(RelPath, Vec<u8>)>,
    label: &str,
    sha256: String,
    hint: &str,
    game: &Game,
    picked: Option<&str>,
) -> Result<ModSource> {
    let host = game.host_dll;
    let server = server_side(&entries);
    let mut notes = Vec::new();
    if !server.is_empty() {
        let (client, srv) = CLIENT_SERVER;
        notes.push(format!(
            "Only the {client}\\ part of the zip is installed; its {srv}\\ part ({} files) is for a dedicated server",
            server.len()
        ));
    }
    let named = |want: &dyn Fn(&str) -> bool| -> Vec<usize> {
        (0..entries.len()).filter(|&i| !server.contains(&i) && want(entries[i].0.file_name())).collect()
    };
    let mut dlls = match picked {
        Some(p) => named(&|n| n.eq_ignore_ascii_case(p)),
        None => named(&|n| n.eq_ignore_ascii_case(host)),
    };
    if dlls.is_empty() && picked.is_none() {
        // no dinput8.dll: a mod that brings its own loader ships it nearest the top
        let own = named(&|n| game.is_own_loader_name(n));
        let depth = |i: &usize| entries[*i].0.as_str().matches('\\').count();
        let top = own.iter().map(depth).min();
        dlls = own.into_iter().filter(|i| Some(depth(i)) == top).collect();
    }
    let dll_name = match dlls.first() {
        None => fail!(
            "{label} has no {host} and no other DLL the game loads by name (such as dbghelp.dll or winmm.dll); \
             dllchain installs mods that ship as one of them"
        ),
        Some(&i) => entries[i].0.file_name().to_ascii_lowercase(),
    };
    if dlls.len() > 1 {
        let names: Vec<&str> = dlls.iter().map(|&i| entries[i].0.as_str()).collect();
        fail!(
            "{label} has {} DLLs the game would load ({}); dllchain cannot tell which one to install",
            dlls.len(),
            names.join(", ")
        );
    }
    let dll_at = dlls[0];
    let root = entries[dll_at].0.parent();
    let dll = entries[dll_at].1.clone();
    if dll.len() as u64 > MAX_DLL {
        fail!("{label}: its {dll_name} is larger than {} MB", MAX_DLL >> 20);
    }
    inspect(&dll, game).map_err(|e| crate::Error(format!("{label}: its {dll_name} is {e}")))?;
    let own_loader = game.is_own_loader_name(&dll_name);
    let inside = |rel: &RelPath| match &root {
        None => Some(rel.clone()),
        Some(r) => rel.strip_prefix(r),
    };
    let installed = |rel: &RelPath| inside(rel).is_some_and(|r| !release_extra(&r));
    notes.extend(check_sums(&entries, &server, &installed).map_err(|e| crate::Error(format!("{label}: {e}")))?);
    let about = if own_loader { about(&entries, &server, &inside) } else { None };

    let mut files = Vec::new();
    let mut ignored = Vec::new();
    let mut extras = Vec::new();
    for (i, (rel, data)) in entries.into_iter().enumerate() {
        if i == dll_at || server.contains(&i) {
            continue;
        }
        match inside(&rel) {
            Some(r) if release_extra(&r) => extras.push(r.to_string()),
            Some(r) => {
                check_target(&r, game).map_err(|e| crate::Error(format!("{label}: {e}")))?;
                files.push((r, data));
            }
            // a read-me or checksum list at the top of a zip whose mod is in a folder (`client\`) is for the player too
            None if release_extra(&rel) => extras.push(rel.to_string()),
            None => ignored.push(rel.to_string()),
        }
    }
    Ok(ModSource {
        label: label.to_string(),
        sha256,
        word_hint: word_hint(hint),
        dll_name: if own_loader { dll_name } else { host.to_string() },
        dll,
        files,
        ignored,
        extras,
        from_url: None,
        about,
        notes,
    })
}

type Entries = [(RelPath, Vec<u8>)];

/// The entries in the `server` folder of a zip that has a `client` folder beside it: a mod with a dedicated-server
/// part, whose client part is the mod.
fn server_side(entries: &Entries) -> HashSet<usize> {
    let (client, server) = CLIENT_SERVER;
    // the folders, as `<parent>` lowercased, that hold a folder called `name` on the way to `rel`
    let holders = |rel: &RelPath, name: &str| -> Vec<String> {
        let parts: Vec<&str> = rel.as_str().split('\\').collect();
        (0..parts.len() - 1)
            .filter(|&k| parts[k].eq_ignore_ascii_case(name))
            .map(|k| parts[..k].join("\\").to_lowercase())
            .collect()
    };
    let clients: HashSet<String> = entries.iter().flat_map(|(r, _)| holders(r, client)).collect();
    (0..entries.len()).filter(|&i| holders(&entries[i].0, server).iter().any(|p| clients.contains(p))).collect()
}

/// A checksum list a mod's download carries: `SHA256.json` (path to hash) or `SHA256SUMS.txt` (`<hash>  <path>`).
fn is_checksum_list(rel: &RelPath) -> bool {
    let name = rel.file_name().to_lowercase();
    name == "sha256.json" || (name.starts_with("sha256sums") && ["", ".txt"].contains(&rel.extension().as_str()))
}

/// The paths (relative to the zip) and hashes a checksum list names; `None` when it is not one dllchain can read.
fn checksum_lines(list: &RelPath, data: &[u8]) -> Option<Vec<(RelPath, String)>> {
    let text = std::str::from_utf8(data).ok()?;
    let pairs: Vec<(String, String)> = if list.extension() == ".json" {
        crate::json::text_fields(text)?
    } else {
        let mut out = Vec::new();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            // BSD: `SHA256 (path) = hash`; GNU: `hash  path`, or `hash *path` for a file read as binary
            out.push(match line.strip_prefix("SHA256 (") {
                Some(rest) => {
                    let (path, hash) = rest.rsplit_once(") = ")?;
                    (path.to_string(), hash.to_string())
                }
                None => {
                    let (hash, path) = line.split_once(char::is_whitespace)?;
                    (path.trim_start().trim_start_matches('*').to_string(), hash.to_string())
                }
            });
        }
        out
    };
    let dir = list.parent();
    pairs
        .into_iter()
        .map(|(path, hash)| {
            let path = path.trim().trim_start_matches("./");
            let rel = match &dir {
                None => RelPath::parse(path).ok()?,
                Some(d) => d.join(path).ok()?,
            };
            let hash = hash.trim().to_ascii_lowercase();
            is_sha256(&hash).then_some((rel, hash))
        })
        .collect()
}

/// Checks the files dllchain installs (`installed`) against every checksum list in the zip: refused when one differs
/// or is missing from the download (a listed archive, the release's own download, may be missing); otherwise notes
/// say how many matched, and which lists dllchain could not read.
fn check_sums(entries: &Entries, server: &HashSet<usize>, installed: &dyn Fn(&RelPath) -> bool) -> Result<Vec<String>> {
    let mut lists = Vec::new();
    let mut notes = Vec::new();
    let mut matched = 0;
    for (list, data) in entries.iter().filter(|(r, _)| is_checksum_list(r)) {
        let Some(lines) = checksum_lines(list, data) else {
            notes.push(format!(
                "{list} is not a checksum list dllchain can read, so the files were not checked against it"
            ));
            continue;
        };
        for (rel, hash) in lines {
            if !installed(&rel) {
                continue;
            }
            match entries.iter().position(|(r, _)| r.same(&rel)) {
                Some(i) if server.contains(&i) => {}
                Some(i) if sha256_hex(&entries[i].1) != hash => {
                    fail!(
                        "{rel} does not match its checksum in {list}; the download may be damaged, so download it again"
                    )
                }
                Some(_) => matched += 1,
                None if is_archive(&rel) => {}
                None => fail!(
                    "{list} lists {rel}, which is not in the download; it may be incomplete, so download it again"
                ),
            }
        }
        lists.push(list.to_string());
    }
    if !lists.is_empty() {
        let s = if matched == 1 { " matches" } else { "s match" };
        notes.insert(0, format!("{matched} file{s} the checksums in {}", lists.join(" and ")));
    }
    Ok(notes)
}

/// A packed download a checksum list may name beside the files: the release's own zip.
fn is_archive(rel: &RelPath) -> bool {
    matches!(rel.extension().as_str(), ".zip" | ".7z" | ".rar")
}

/// What the one `mod.json` in the mod's folder says, for a mod that brings its own loader.
fn about(entries: &Entries, server: &HashSet<usize>, inside: &dyn Fn(&RelPath) -> Option<RelPath>) -> Option<About> {
    let mut found = entries.iter().enumerate().filter(|(i, (r, _))| {
        !server.contains(i) && r.file_name().eq_ignore_ascii_case("mod.json") && inside(r).is_some()
    });
    let (_, (_, data)) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    let fields = crate::json::text_fields(std::str::from_utf8(data).ok()?)?;
    let get =
        |k: &str| fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.trim().to_string()).filter(|v| !v.is_empty());
    let a = About { id: get("id"), name: get("name"), version: get("version") };
    (a != About::default()).then_some(a)
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
            || [".sha256", ".sha512", ".sha1", ".md5", ".sig", ".asc", ".minisig"].contains(&ext.as_str())
            || is_checksum_list(rel);
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

/// A `dinput8.dll` (or a loader of its own) in an unpacked mod folder: the folder is the mod, as if it were the zip. Refused when the folder
/// looks like more than one mod's files (programs or archives in it, or a folder like Downloads itself).
fn read_folder(dir: &Path, picked: &str, game: &Game) -> Result<ModSource> {
    let real = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let name = real.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let label = format!("{name}\\");
    let own_folder = "Pick the mod's zip, or put the mod's files in a folder of their own first.";
    if is_personal_folder(dir) {
        fail!("{picked} is loose in {}, not in a folder of its own. {own_folder}", dir.display());
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
    let mut s = from_entries(entries, &label, String::new(), &name, game, Some(picked))?;
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
            fail!("it also ships {rel}, a second DLL the game loads by name; dllchain installs one per mod");
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
        // a read-me beside the mod's folder is the player's, like one next to the DLL
        assert!(s.ignored.is_empty());
        assert_eq!(s.extras, ["readme.txt"]);
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

/// Zips whose mod brings its own loader (`dbgcore.dll`, `powrprof.dll`), a `client\` and `server\` part, and checksum
/// lists.
#[cfg(test)]
mod own_loader_tests {
    use super::tests::make_zip;
    use super::*;
    use crate::dllinfo::{fake_dll, fake_dll_with};
    use crate::game::ENSHROUDED;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("chainkit-own-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    fn greeneye() -> Vec<u8> {
        fake_dll_with(&["greeneye"], &[("ProductName", "Greeneye Loader"), ("ProductVersion", "1.2.2")])
    }

    const MOD_JSON: &[u8] =
        br#"{ "id": "town-portal", "name": "Town Portals", "version": "1.0.12", "deps": [1, {"x": "y"}] }"#;

    /// A checksum list for some files: its name and text.
    type Sums<'a> = &'a dyn Fn(&[(&str, &[u8])]) -> (String, Vec<u8>);

    /// Town Portals' layout: the client part with the loader and the mod's folder, a server part, a read-me and the
    /// checksums of both parts at the top. `sums` gives the checksum list's text for the files.
    fn town_portals(name: &str, sums: Sums) -> std::path::PathBuf {
        let dll = greeneye();
        let files: Vec<(&str, &[u8])> = vec![
            ("client/dbgcore.dll", &dll),
            ("client/greeneye/town-portal/mod.json", MOD_JSON),
            ("client/greeneye/town-portal/town-portal.dll", b"tp"),
            ("server/dbgcore.dll", &dll),
            ("server/greeneye/town-portal/town-portal.dll", b"tp-server"),
        ];
        let (list, text) = sums(&files);
        let mut all = files.clone();
        all.push(("README.txt", b"read me"));
        all.push((&list, &text));
        let p = tmp(name);
        make_zip(&p, &all);
        p
    }

    fn json(files: &[(&str, &[u8])]) -> (String, Vec<u8>) {
        let lines: Vec<String> = files.iter().map(|(n, b)| format!("  \"{n}\": \"{}\"", sha256_hex(b))).collect();
        ("SHA256.json".into(), format!("{{\n{}\n}}", lines.join(",\n")).into_bytes())
    }

    #[test]
    fn a_client_and_server_zip_installs_its_client_part_with_its_loader() {
        let s = read(&town_portals("tp.zip", &json), &ENSHROUDED).unwrap();
        assert_eq!(s.dll_name, "dbgcore.dll");
        assert!(s.own_loader(&ENSHROUDED));
        assert_eq!(s.dll, greeneye());
        let names: Vec<&str> = s.files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, [r"greeneye\town-portal\mod.json", r"greeneye\town-portal\town-portal.dll"]);
        assert_eq!(s.extras, ["README.txt", "SHA256.json"]);
        assert!(s.ignored.is_empty(), "{:?}", s.ignored);
        assert_eq!(
            s.about,
            Some(About {
                id: Some("town-portal".into()),
                name: Some("Town Portals".into()),
                version: Some("1.0.12".into())
            })
        );
        assert_eq!(
            s.notes,
            [
                r"Only the client\ part of the zip is installed; its server\ part (2 files) is for a dedicated server",
                "3 files match the checksums in SHA256.json",
            ]
        );
    }

    #[test]
    fn a_file_that_does_not_match_its_checksum_is_refused() {
        let damaged = |files: &[(&str, &[u8])]| {
            let (n, t) = json(files);
            let t = String::from_utf8(t).unwrap().replace(&sha256_hex(b"tp"), &sha256_hex(b"other"));
            (n, t.into_bytes())
        };
        let e = read(&town_portals("tp-bad.zip", &damaged), &ENSHROUDED).unwrap_err();
        assert!(
            e.0.contains(r"greeneye\town-portal\town-portal.dll does not match its checksum in SHA256.json"),
            "{e}"
        );
        // a listed file the zip does not have
        let more = |files: &[(&str, &[u8])]| {
            let mut f = files.to_vec();
            f.push(("client/greeneye/town-portal/gone.txt", b"g"));
            json(&f)
        };
        let e = read(&town_portals("tp-gone.zip", &more), &ENSHROUDED).unwrap_err();
        assert!(
            e.0.contains(r"SHA256.json lists client\greeneye\town-portal\gone.txt, which is not in the download"),
            "{e}"
        );
    }

    #[test]
    fn checksum_lists_in_their_usual_forms() {
        // GNU (with a binary-mode star and a comment), BSD, and the release's own zip, which is not in it
        let gnu = |files: &[(&str, &[u8])]| {
            let mut t = String::from("# sums\n");
            for (n, b) in files {
                t.push_str(&format!("{} *./{n}\n", sha256_hex(b).to_uppercase()));
            }
            t.push_str(&format!("{}  TownPortals.zip\n", sha256_hex(b"zip")));
            ("SHA256SUMS".to_string(), t.into_bytes())
        };
        let s = read(&town_portals("tp-gnu.zip", &gnu), &ENSHROUDED).unwrap();
        assert!(s.notes.iter().any(|n| n == "3 files match the checksums in SHA256SUMS"), "{:?}", s.notes);
        let bsd = |files: &[(&str, &[u8])]| {
            let t: String = files.iter().map(|(n, b)| format!("SHA256 ({n}) = {}\n", sha256_hex(b))).collect();
            ("sha256sums.txt".to_string(), t.into_bytes())
        };
        let s = read(&town_portals("tp-bsd.zip", &bsd), &ENSHROUDED).unwrap();
        assert!(s.notes.iter().any(|n| n == "3 files match the checksums in sha256sums.txt"), "{:?}", s.notes);
        // one dllchain cannot read checks nothing, and says so
        let odd = |_: &[(&str, &[u8])]| ("SHA256.json".to_string(), b"[1, 2]".to_vec());
        let s = read(&town_portals("tp-odd.zip", &odd), &ENSHROUDED).unwrap();
        assert!(s.notes.iter().any(|n| n.contains("not a checksum list dllchain can read")), "{:?}", s.notes);
    }

    #[test]
    fn a_loader_at_the_top_with_the_mods_folder_beside_it() {
        let dll = fake_dll_with(&[], &[("ProductName", "Auto Doors"), ("ProductVersion", "1.1.0")]);
        let p = tmp("Auto Doors 1.1.0.zip");
        make_zip(&p, &[("powrprof.dll", &dll), ("AutoDoors/bin/ad_core.dll", b"core"), ("AutoDoors/README.txt", b"r")]);
        let s = read(&p, &ENSHROUDED).unwrap();
        assert_eq!(s.dll_name, "powrprof.dll");
        assert_eq!(s.about, None);
        let names: Vec<&str> = s.files.iter().map(|f| f.0.as_str()).collect();
        assert_eq!(names, [r"AutoDoors\bin\ad_core.dll", r"AutoDoors\README.txt"]);
        // the same, unpacked, picked by its DLL
        let dir = tmp("Auto Doors unpacked");
        std::fs::create_dir_all(dir.join(r"AutoDoors\bin")).unwrap();
        std::fs::write(dir.join("powrprof.dll"), &dll).unwrap();
        std::fs::write(dir.join(r"AutoDoors\bin\ad_core.dll"), b"core").unwrap();
        let s = read(&dir.join("powrprof.dll"), &ENSHROUDED).unwrap();
        assert_eq!(s.dll_name, "powrprof.dll");
        assert_eq!(s.files.iter().map(|f| f.0.as_str()).collect::<Vec<_>>(), [r"AutoDoors\bin\ad_core.dll"]);
    }

    #[test]
    fn a_zip_without_one_loader_it_can_tell_is_refused() {
        let dll = fake_dll(&[], None);
        let p = tmp("none.zip");
        make_zip(&p, &[("mod.dll", &dll)]);
        let e = read(&p, &ENSHROUDED).unwrap_err();
        assert!(e.0.contains("has no dinput8.dll and no other DLL the game loads by name"), "{e}");
        let p = tmp("two-loaders.zip");
        make_zip(&p, &[("winmm.dll", &dll), ("powrprof.dll", &dll)]);
        let e = read(&p, &ENSHROUDED).unwrap_err();
        assert!(e.0.contains("has 2 DLLs the game would load (winmm.dll, powrprof.dll)"), "{e}");
        // a loader deeper than another is not a second one: the top one is the mod's, and the deeper one a file
        let p = tmp("deeper.zip");
        make_zip(&p, &[("powrprof.dll", &dll), ("AutoDoors/bin/winmm.dll", &dll)]);
        assert_eq!(read(&p, &ENSHROUDED).unwrap().dll_name, "powrprof.dll");
    }
}
