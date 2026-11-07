//! `dllchain\record.txt`: what dllchain installed and where it left each DLL.
//!
//! The record is read as untrusted input, like any other file in the game folder: every path is checked again, a
//! line that does not parse is skipped and reported, and nothing is deleted or moved on its word alone (the files'
//! SHA-256 must match too).

use crate::OWN_DIR;
use crate::hash::is_sha256;
use crate::paths::{RelPath, is_word};

pub const RECORD: &str = "dllchain\\record.txt";
const HEADER: &str = "dllchain-record\t1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Installed by dllchain from a zip or DLL the player picked.
    Installed,
    /// Another mod's `dinput8.dll` that dllchain found and chained; dllchain did not install it and never deletes it.
    Adopted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRec {
    pub rel: RelPath,
    /// What dllchain wrote.
    pub sha: String,
    /// A copy of the file that was there before, put back on uninstall.
    pub backup: Option<RelPath>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModRec {
    pub word: String,
    pub kind: Kind,
    /// When it came: the host is the newest mod that can host, when the current one cannot.
    pub order: u32,
    pub at: String,
    /// The SHA-256 of the zip or DLL it was installed from ("-" when adopted).
    pub source_sha: String,
    /// The zip's or DLL's file name, or how it was found.
    pub label: String,
    /// The name the player gave it, or the DLL's own product name; `None`: the known tool's name or its word.
    pub name: Option<String>,
    /// Where the zip or DLL was downloaded from, as Windows noted it (without the query part, which can hold ids).
    pub from_url: Option<String>,
    pub dll_sha: String,
    /// Where dllchain last saw or put its DLL.
    pub dll_at: RelPath,
    /// An earlier copy of the same tool, found as `dinput8.dll` and replaced by this install; uninstall puts it back.
    pub replaced_dll: Option<String>,
    /// Its version: from the DLL's version information, else from the name of the zip it came in.
    pub version: Option<String>,
    pub files: Vec<FileRec>,
    /// Files an older version or a hand install left in the mod's folder that dllchain took out (`sha` is what was
    /// there, `backup` its copy); uninstall puts them back.
    pub retired: Vec<FileRec>,
    /// Files of the mod that were in the game folder already, the same as in its zip, when dllchain listed it (a hand
    /// install it took over, or a loader another mod brought): dllchain checks them like its own, and uninstall leaves
    /// them as they were.
    pub found: Vec<FileRec>,
    /// Folders it created, for removing them again when empty.
    pub dirs: Vec<RelPath>,
}

impl ModRec {
    /// A mod at `dll_at` with nothing else known yet.
    pub fn new(word: String, kind: Kind, order: u32, dll_at: RelPath) -> ModRec {
        ModRec {
            word,
            kind,
            order,
            at: String::new(),
            source_sha: String::new(),
            label: String::new(),
            name: None,
            from_url: None,
            dll_sha: String::new(),
            dll_at,
            replaced_dll: None,
            version: None,
            files: Vec::new(),
            retired: Vec::new(),
            found: Vec::new(),
            dirs: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Record {
    pub mods: Vec<ModRec>,
    /// dllchain made `dinput8-chain.ini`; it deletes it again once no DLL is left in it.
    pub chain_created: bool,
    /// The chain file's DLL lines as dllchain last left them, to show what others changed since.
    pub chain: Vec<(RelPath, bool)>,
    /// The list file those lines and `chain_created` are about, when it is not the game's own chain file: the one
    /// another loader as the host reads (ModLoader Proxy's `chainloader.ini`).
    pub chain_file: Option<RelPath>,
}

/// Where dllchain keeps a copy of a mod's DLL, for repairs.
pub fn store_rel(word: &str, sha: &str) -> RelPath {
    RelPath::of(&format!("{OWN_DIR}\\mods\\{word}\\{sha}.dll"))
}

/// Text for one field of a tab-separated line: no tabs or line breaks.
pub(crate) fn clean(s: &str) -> String {
    s.chars().map(|c| if c == '\t' || c == '\r' || c == '\n' { ' ' } else { c }).collect()
}

/// A mod name as players see it: trimmed, at most 60 characters, no control characters. `None` when nothing is left.
pub fn clean_name(s: &str) -> Option<String> {
    let s: String = s.chars().filter(|c| !c.is_control()).collect();
    let s: String = s.trim().chars().take(60).collect();
    let s = s.trim_end().to_string();
    (!s.is_empty()).then_some(s)
}

/// A web address worth keeping: http or https, without the user part, the query and the fragment (download links
/// carry ids and keys there). `None` for anything else.
pub fn clean_url(s: &str) -> Option<String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let scheme = ["https://", "http://"].into_iter().find(|p| lower.starts_with(p))?;
    let rest = &s[scheme.len()..];
    let rest = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    let (host, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    let host = host.rsplit('@').next().unwrap_or_default();
    if host.is_empty() || !host.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)) {
        return None;
    }
    if path.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let url = format!("{scheme}{}{path}", host.to_ascii_lowercase());
    (url.len() <= 400).then_some(url)
}

impl Record {
    pub fn next_order(&self) -> u32 {
        self.mods.iter().map(|m| m.order).max().map_or(1, |o| o + 1)
    }

    pub fn find(&self, word: &str) -> Option<&ModRec> {
        self.mods.iter().find(|m| m.word == word)
    }

    pub fn is_empty(&self) -> bool {
        self.mods.is_empty() && !self.chain_created
    }

    pub fn to_text(&self) -> String {
        let mut out = vec![HEADER.to_string()];
        for m in &self.mods {
            let kind = if m.kind == Kind::Installed { "installed" } else { "adopted" };
            out.push(format!(
                "mod\t{}\t{kind}\t{}\t{}\t{}\t{}",
                m.word,
                m.order,
                clean(&m.at),
                m.source_sha,
                clean(&m.label)
            ));
            out.push(format!("dll\t{}\t{}\t{}", m.word, m.dll_at, m.dll_sha));
            if let Some(n) = &m.name {
                out.push(format!("name\t{}\t{}", m.word, clean(n)));
            }
            if let Some(u) = &m.from_url {
                out.push(format!("from\t{}\t{}", m.word, clean(u)));
            }
            if let Some(r) = &m.replaced_dll {
                out.push(format!("replaced-dll\t{}\t{r}", m.word));
            }
            if let Some(v) = &m.version {
                out.push(format!("version\t{}\t{}", m.word, clean(v)));
            }
            for (kind, list) in [("file", &m.files), ("retired", &m.retired)] {
                for f in list {
                    let b = f.backup.as_ref().map_or("-".to_string(), |b| b.to_string());
                    out.push(format!("{kind}\t{}\t{}\t{}\t{b}", m.word, f.rel, f.sha));
                }
            }
            for f in &m.found {
                out.push(format!("found\t{}\t{}\t{}", m.word, f.rel, f.sha));
            }
            for d in &m.dirs {
                out.push(format!("dir\t{}\t{d}", m.word));
            }
        }
        for (name, on) in &self.chain {
            out.push(format!("chain\t{name}\t{}", if *on { "on" } else { "off" }));
        }
        if self.chain_created {
            out.push("chain-created".into());
        }
        if let Some(f) = &self.chain_file {
            out.push(format!("chain-file\t{f}"));
        }
        out.join("\r\n") + "\r\n"
    }

    /// The record and a note for each line it skipped.
    pub fn parse(text: &str) -> (Record, Vec<String>) {
        let mut r = Record::default();
        let mut warn = Vec::new();
        let mut lines = text.lines();
        if lines.next().map(str::trim_end) != Some(HEADER) {
            warn.push("dllchain's record has an unknown format; it is treated as empty".into());
            return (r, warn);
        }
        for (n, line) in lines.enumerate() {
            let f: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
            if let Err(why) = r.parse_line(&f) {
                warn.push(format!("record line {} skipped ({why}): {}", n + 2, line.trim()));
            }
        }
        (r, warn)
    }

    fn parse_line(&mut self, f: &[&str]) -> Result<(), String> {
        let sha = |s: &str| {
            if is_sha256(s) { Ok(s.to_string()) } else { Err("not a SHA-256".to_string()) }
        };
        let rel = |s: &str| RelPath::parse(s).map_err(|e| e.0);
        let own = |r: RelPath| {
            if r.first_part().eq_ignore_ascii_case(OWN_DIR) {
                Ok(r)
            } else {
                Err("a backup outside dllchain's folder".to_string())
            }
        };
        let word = |s: &str| {
            if is_word(s) { Ok(s.to_string()) } else { Err("not a mod word".to_string()) }
        };
        match f {
            ["mod", w, kind, order, at, source, label] => {
                let kind = match *kind {
                    "installed" => Kind::Installed,
                    "adopted" => Kind::Adopted,
                    _ => return Err("unknown kind".into()),
                };
                let w = word(w)?;
                if self.find(&w).is_some() {
                    return Err("the same mod twice".into());
                }
                let source = if *source == "-" { "-".into() } else { sha(source)? };
                self.mods.push(ModRec {
                    at: at.to_string(),
                    source_sha: source,
                    label: label.to_string(),
                    ..ModRec::new(w, kind, order.parse().map_err(|_| "not a number")?, RelPath::of("dinput8.dll"))
                });
            }
            ["dll", w, at, s] => {
                let (at, s) = (rel(at)?, sha(s)?);
                let m = self.mod_mut(w)?;
                m.dll_at = at;
                m.dll_sha = s;
            }
            ["name", w, n] => {
                let n = clean_name(n).ok_or("not a name")?;
                self.mod_mut(w)?.name = Some(n);
            }
            ["from", w, u] => {
                let u = clean_url(u).ok_or("not a web address")?;
                self.mod_mut(w)?.from_url = Some(u);
            }
            ["replaced-dll", w, s] => {
                let s = sha(s)?;
                self.mod_mut(w)?.replaced_dll = Some(s);
            }
            ["file", w, at, s, backup] => {
                let backup = if *backup == "-" { None } else { Some(own(rel(backup)?)?) };
                let (at, s) = (rel(at)?, sha(s)?);
                self.mod_mut(w)?.files.push(FileRec { rel: at, sha: s, backup });
            }
            ["version", w, v] => {
                let v = clean_name(v).ok_or("not a version")?;
                self.mod_mut(w)?.version = Some(v);
            }
            ["retired", w, at, s, backup] => {
                let backup = own(rel(backup)?)?;
                let (at, s) = (rel(at)?, sha(s)?);
                self.mod_mut(w)?.retired.push(FileRec { rel: at, sha: s, backup: Some(backup) });
            }
            ["found", w, at, s] => {
                let (at, s) = (rel(at)?, sha(s)?);
                self.mod_mut(w)?.found.push(FileRec { rel: at, sha: s, backup: None });
            }
            ["dir", w, at] => {
                let at = rel(at)?;
                self.mod_mut(w)?.dirs.push(at);
            }
            ["chain", name, on] => {
                let on = match *on {
                    "on" => true,
                    "off" => false,
                    _ => return Err("not on or off".into()),
                };
                self.chain.push((rel(name)?, on));
            }
            ["chain-created"] => self.chain_created = true,
            ["chain-file", name] => self.chain_file = Some(rel(name)?),
            [""] => {}
            _ => return Err("unknown line".into()),
        }
        Ok(())
    }

    fn mod_mut(&mut self, w: &str) -> Result<&mut ModRec, String> {
        self.mods.iter_mut().find(|m| m.word == w).ok_or_else(|| "for a mod not listed above".to_string())
    }

    /// Drops mods whose `dll` line was missing or unreadable.
    pub fn drop_incomplete(&mut self, warn: &mut Vec<String>) {
        self.mods.retain(|m| {
            let ok = is_sha256(&m.dll_sha);
            if !ok {
                warn.push(format!("record: {} has no DLL line; left out", m.word));
            }
            ok
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Record {
        let s = "a".repeat(64);
        Record {
            mods: vec![ModRec {
                word: "auto-loot-plus".into(),
                kind: Kind::Installed,
                order: 2,
                at: "2026-11-03 10:00:00".into(),
                source_sha: s.clone(),
                label: "Auto\tLoot.zip".into(),
                name: Some("My loot mod".into()),
                from_url: Some("https://www.nexusmods.com/enshrouded/mods/170".into()),
                dll_sha: "b".repeat(64),
                dll_at: RelPath::of("dinput8.dll"),
                replaced_dll: Some("c".repeat(64)),
                version: Some("1.1.8".into()),
                files: vec![FileRec {
                    rel: RelPath::of("mods\\alp\\config.txt"),
                    sha: s,
                    backup: Some(RelPath::of("dllchain\\backup\\1\\x")),
                }],
                retired: vec![FileRec {
                    rel: RelPath::of("mods\\alp\\old.txt"),
                    sha: "d".repeat(64),
                    backup: Some(RelPath::of("dllchain\\originals\\alp\\mods\\alp\\old.txt")),
                }],
                found: vec![FileRec { rel: RelPath::of("mods\\alp\\icon.png"), sha: "e".repeat(64), backup: None }],
                dirs: vec![RelPath::of("mods\\alp")],
            }],
            chain_created: true,
            chain: vec![(RelPath::of("enstash.dll"), false)],
            chain_file: Some(RelPath::of("chainloader.ini")),
        }
    }

    #[test]
    fn round_trip() {
        let r = sample();
        let (back, warn) = Record::parse(&r.to_text());
        assert!(warn.is_empty(), "{warn:?}");
        let mut want = r.clone();
        want.mods[0].label = "Auto Loot.zip".into();
        assert_eq!(back, want);
    }

    #[test]
    fn names_and_web_addresses_are_cleaned() {
        assert_eq!(clean_name("  Loot\u{7}  "), Some("Loot".into()));
        assert_eq!(clean_name(" \t "), None);
        assert_eq!(clean_name(&"x".repeat(80)).map(|s| s.len()), Some(60));
        assert_eq!(
            clean_url("https://user:pw@Files.Nexus-CDN.com/3160/170/ALP.zip?md5=1&expires=2&user_id=123#x"),
            Some("https://files.nexus-cdn.com/3160/170/ALP.zip".into())
        );
        assert_eq!(
            clean_url("https://www.nexusmods.com/enshrouded/mods/170?tab=files"),
            Some("https://www.nexusmods.com/enshrouded/mods/170".into())
        );
        for bad in ["about:internet", "file:///C:/x.zip", "javascript:alert(1)", "https://", "https://a b/c"] {
            assert_eq!(clean_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn hostile_lines_are_skipped() {
        let s = "a".repeat(64);
        let text = format!(
            "{HEADER}\nmod\tx\tinstalled\t1\tnow\t-\tx.zip\ndll\tx\tx.dll\t{s}\nfile\tx\t..\\..\\Windows\\a.dll\t{s}\t-\n\
             file\tx\tC:\\a.dll\t{s}\t-\nfile\tx\tok.txt\t{s}\tmods\\elsewhere.bak\nfile\tx\tok.txt\tnot-a-sha\t-\n\
             dll\tnobody\ty.dll\t{s}\nmod\tBAD WORD\tinstalled\t1\tnow\t-\tl\nrm -rf\n"
        );
        let (r, warn) = Record::parse(&text);
        assert_eq!(r.mods.len(), 1);
        assert!(r.mods[0].files.is_empty());
        assert_eq!(warn.len(), 7, "{warn:?}");
        let (r, warn) = Record::parse("something else");
        assert!(r.mods.is_empty() && warn.len() == 1);
    }
}
