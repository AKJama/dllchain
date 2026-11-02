//! Paths inside the game folder, and the checks that keep every write there.
//!
//! EMF's rules for a chain entry (no drive, no UNC path, no leading slash, no `..`, `/` read as `\`, no device name)
//! apply here to every path dllchain reads from anywhere: a zip entry, a chain line, a line of its own record.

use crate::{Result, fail};
use std::path::{Path, PathBuf};

/// A path relative to the game folder that passed every check, with `\` between its parts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

const DEVICE_NAMES: &[&str] = &["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$"];

impl RelPath {
    pub fn parse(text: &str) -> Result<RelPath> {
        let s = text.replace('/', "\\");
        if s.is_empty() {
            fail!("an empty path");
        }
        if s.starts_with('\\') || s.contains(':') {
            fail!(
                "{text}: only paths inside the game folder are allowed (no drive, no leading slash, no network path)"
            );
        }
        if s.len() > 240 {
            fail!("{text}: the path is too long");
        }
        for part in s.split('\\') {
            if part.is_empty() || part == "." || part == ".." {
                fail!("{text}: only paths inside the game folder are allowed (no '..', '.' or empty parts)");
            }
            if part.ends_with('.') || part.ends_with(' ') || part.starts_with(' ') {
                fail!("{text}: a name that starts with a space or ends with a dot or space");
            }
            if part.chars().any(|c| (c as u32) < 32 || "<>\"|?*".contains(c)) {
                fail!("{text}: a character Windows does not allow in file names");
            }
            if is_device_name(part) {
                fail!("{text}: {part} is a Windows device name, not a file");
            }
        }
        Ok(RelPath(s))
    }

    /// A path known to be safe, for names dllchain builds itself.
    pub fn of(text: &str) -> RelPath {
        RelPath::parse(text).unwrap_or_else(|e| panic!("dllchain's own path is not safe: {e}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Lowercase, for comparing (Windows file names ignore case).
    pub fn key(&self) -> String {
        self.0.to_lowercase()
    }

    pub fn same(&self, other: &RelPath) -> bool {
        self.key() == other.key()
    }

    pub fn file_name(&self) -> &str {
        self.0.rsplit('\\').next().unwrap_or(&self.0)
    }

    /// The extension with its dot, lowercase (".dll"), or "".
    pub fn extension(&self) -> String {
        let name = self.file_name();
        match name.rfind('.') {
            Some(i) if i > 0 => name[i..].to_lowercase(),
            _ => String::new(),
        }
    }

    pub fn parent(&self) -> Option<RelPath> {
        self.0.rfind('\\').map(|i| RelPath(self.0[..i].to_string()))
    }

    pub fn is_top_level(&self) -> bool {
        !self.0.contains('\\')
    }

    pub fn first_part(&self) -> &str {
        self.0.split('\\').next().unwrap_or(&self.0)
    }

    pub fn join(&self, more: &str) -> Result<RelPath> {
        RelPath::parse(&format!("{}\\{}", self.0, more))
    }

    /// `self` with `prefix\` taken off, when it is inside `prefix`.
    pub fn strip_prefix(&self, prefix: &RelPath) -> Option<RelPath> {
        let p = format!("{}\\", prefix.key());
        if self.key().starts_with(&p) { Some(RelPath(self.0[p.len()..].to_string())) } else { None }
    }

    /// Every folder on the way, outermost first ("a", "a\b" for "a\b\c.txt").
    pub fn ancestors(&self) -> Vec<RelPath> {
        let parts: Vec<&str> = self.0.split('\\').collect();
        (1..parts.len()).map(|n| RelPath(parts[..n].join("\\"))).collect()
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `CON`, `nul.txt`, `COM1`, `lpt9.log`, `COM¹`: Windows opens a device for these, whatever the extension.
fn is_device_name(part: &str) -> bool {
    let stem = part.split('.').next().unwrap_or(part).trim_end().to_uppercase();
    if DEVICE_NAMES.contains(&stem.as_str()) {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = stem.strip_prefix(prefix) {
            let mut chars = rest.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                if c.is_ascii_digit() || "¹²³".contains(c) {
                    return true;
                }
            }
        }
    }
    false
}

/// The full path of `rel` in `game`, after checking that no folder on the way (nor the file itself) is a junction or
/// symbolic link: one inside the game folder could send a write anywhere on the PC.
pub fn resolve(game: &Path, rel: &RelPath) -> Result<PathBuf> {
    let mut at = game.to_path_buf();
    for part in rel.as_str().split('\\') {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) => {
                if is_link(&meta) {
                    fail!(
                        "{rel}: {} is a link to another place (junction or symbolic link); dllchain does not write through links",
                        at.display()
                    );
                }
            }
            Err(_) => break, // not there yet: nothing further down can be a link
        }
    }
    Ok(game.join(rel.as_str()))
}

#[cfg(windows)]
fn is_link(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link(meta: &std::fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// A short file-name word from free text: lowercase letters, digits and `-`, at most 40.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut s = out.trim_matches('-').to_string();
    if s.len() > 40 {
        s.truncate(40);
        s = s.trim_end_matches('-').to_string();
    }
    s
}

/// A word as dllchain stores it: 1 to 40 of `a-z`, `0-9`, `-`.
pub fn is_word(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_paths_inside() {
        assert_eq!(RelPath::parse("mods/my-mod/x.dll").unwrap().as_str(), "mods\\my-mod\\x.dll");
        assert_eq!(RelPath::parse("enstash.dll").unwrap().file_name(), "enstash.dll");
        assert_eq!(RelPath::parse("a\\b\\C.DLL").unwrap().extension(), ".dll");
        assert_eq!(RelPath::of("a\\b\\c").ancestors(), vec![RelPath::of("a"), RelPath::of("a\\b")]);
        assert_eq!(RelPath::of("a\\b\\c").strip_prefix(&RelPath::of("A")), Some(RelPath::of("b\\c")));
    }

    #[test]
    fn refuses_paths_outside_and_devices() {
        for bad in [
            "",
            "..\\x.dll",
            "a\\..\\..\\x.dll",
            "C:\\Windows\\x.dll",
            "\\\\server\\share\\x.dll",
            "\\x.dll",
            "/x.dll",
            "a\\\\b",
            ".\\x",
            "CON",
            "con.txt",
            "mods\\NUL\\x.dll",
            "COM1.dll",
            "lpt9",
            "COM\u{b9}",
            "x.dll.",
            "x.dll ",
            "a|b",
            "a\u{1}b",
            "C:x.dll",
        ] {
            assert!(RelPath::parse(bad).is_err(), "{bad:?} should be refused");
        }
        assert!(RelPath::parse("COM10.dll").is_ok());
        assert!(RelPath::parse("console.dll").is_ok());
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Auto Loot Plus!"), "auto-loot-plus");
        assert_eq!(slug("--x--"), "x");
        assert!(is_word("auto-loot-plus"));
        assert!(!is_word("Auto"));
        assert!(!is_word("a b"));
    }

    #[cfg(windows)]
    #[test]
    fn refuses_writes_through_a_junction() {
        let root = std::env::temp_dir().join(format!("chainkit-junction-{}", std::process::id()));
        let game = root.join("game");
        let outside = root.join("outside");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let link = game.join("mods");
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            let err = resolve(&game, &RelPath::of("mods\\x.txt")).unwrap_err();
            assert!(err.0.contains("link"), "{err}");
            assert!(resolve(&game, &RelPath::of("other\\x.txt")).is_ok());
        }
        let _ = std::fs::remove_dir(&link);
        let _ = std::fs::remove_dir_all(&root);
        assert!(ok, "could not make a junction for the test");
    }
}
