//! `dinput8-chain.ini`, read and written by EMF's documented rules.
//!
//! Lines in `[Chain]` (and lines before any section) count; other sections are another tool's business and stay as
//! they are. Blank lines and `;` `#` `//` comments are skipped; a `key=value` line counts with its value when that
//! names a `.dll`; `; ` in front of a DLL switches it off. Only DLLs inside the game folder count, `dinput8.dll` never
//! does, each DLL once, at most 256. The file is written back in the encoding it was found in, with CRLF.

use crate::paths::RelPath;

pub const MAX_ENTRIES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf8Bom,
    Utf16Bom,
    /// Not UTF-8: read and written byte for byte (as Latin-1), so nothing in the file changes that dllchain did not
    /// change. A non-ASCII DLL name in such a file is compared as those bytes.
    Ansi,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainFile {
    pub lines: Vec<String>,
    pub encoding: Encoding,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: RelPath,
    pub on: bool,
    pub line: usize,
}

/// A line that names a DLL the rules refuse, kept for the status report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub line: usize,
    pub text: String,
    pub why: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Parsed {
    pub entries: Vec<Entry>,
    pub refused: Vec<Refused>,
}

impl Parsed {
    pub fn on(&self) -> impl Iterator<Item = &Entry> {
        self.entries.iter().filter(|e| e.on)
    }

    pub fn find(&self, name: &RelPath) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name.same(name))
    }
}

impl ChainFile {
    pub fn new() -> ChainFile {
        ChainFile { lines: Vec::new(), encoding: Encoding::Utf8 }
    }

    pub fn decode(bytes: &[u8]) -> ChainFile {
        let (text, encoding) = if bytes.starts_with(&[0xFF, 0xFE]) {
            let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            (String::from_utf16_lossy(&units), Encoding::Utf16Bom)
        } else if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            (String::from_utf8_lossy(&bytes[3..]).into_owned(), Encoding::Utf8Bom)
        } else if let Ok(s) = std::str::from_utf8(bytes) {
            (s.to_string(), Encoding::Utf8)
        } else {
            (bytes.iter().map(|&b| b as char).collect(), Encoding::Ansi)
        };
        let mut lines: Vec<String> = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
        if lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        ChainFile { lines, encoding }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut text = self.lines.join("\r\n");
        text.push_str("\r\n");
        match self.encoding {
            Encoding::Utf8 => text.into_bytes(),
            Encoding::Utf8Bom => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
            Encoding::Utf16Bom => {
                let mut out = vec![0xFF, 0xFE];
                for u in text.encode_utf16() {
                    out.extend_from_slice(&u.to_le_bytes());
                }
                out
            }
            Encoding::Ansi => text.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect(),
        }
    }

    /// Indexes of the lines that count: before any section, and in `[Chain]`.
    fn counted(&self) -> Vec<usize> {
        let mut inside = true;
        let mut out = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            let t = l.trim();
            if t.starts_with('[') {
                inside = t.eq_ignore_ascii_case("[chain]");
                continue;
            }
            if inside {
                out.push(i);
            }
        }
        out
    }

    pub fn parse(&self, host_dll: &str) -> Parsed {
        let mut p = Parsed::default();
        let mut on_count = 0;
        for i in self.counted() {
            let t = self.lines[i].trim();
            if t.is_empty() {
                continue;
            }
            let (body, on) = if let Some(rest) = t.strip_prefix(';') {
                (rest.trim_start_matches(';').trim(), false)
            } else if t.starts_with('#') || t.starts_with("//") {
                continue;
            } else {
                (t, true)
            };
            let Some(name) = dll_name(body) else { continue }; // a comment or a setting
            if on && on_count >= MAX_ENTRIES {
                p.refused.push(Refused { line: i, text: t.into(), why: format!("more than {MAX_ENTRIES} entries") });
                continue;
            }
            let rel = match RelPath::parse(&name) {
                Ok(r) => r,
                Err(e) => {
                    if on {
                        p.refused.push(Refused { line: i, text: t.into(), why: e.0 });
                    }
                    continue;
                }
            };
            if rel.key() == host_dll.to_lowercase() {
                if on {
                    p.refused.push(Refused { line: i, text: t.into(), why: format!("{host_dll} is the host itself") });
                }
                continue;
            }
            if p.find(&rel).is_some() {
                continue; // each DLL once: the first line counts
            }
            if on {
                on_count += 1;
            }
            p.entries.push(Entry { name: rel, on, line: i });
        }
        p
    }

    /// Lists `name` at the end of `[Chain]`. A line that switched it off is switched on again when `switch_on`, else
    /// left off. Returns what happened.
    pub fn add(&mut self, name: &RelPath, host_dll: &str, switch_on: bool) -> AddResult {
        if let Some(e) = self.parse(host_dll).find(name).cloned() {
            if e.on {
                return AddResult::AlreadyOn;
            }
            if !switch_on {
                return AddResult::KeptOff;
            }
            self.lines[e.line] = name.to_string();
            return AddResult::SwitchedOn;
        }
        if self.lines.is_empty() {
            self.lines = vec!["[Chain]".into(), name.to_string()];
            return AddResult::Added;
        }
        let header = self.lines.iter().rposition(|l| l.trim().eq_ignore_ascii_case("[chain]"));
        let any_section = self.lines.iter().any(|l| l.trim().starts_with('['));
        let at = match header {
            None if !any_section => {
                let mut at = self.lines.len();
                while at > 0 && self.lines[at - 1].trim().is_empty() {
                    at -= 1;
                }
                at
            }
            None => {
                self.lines.push(String::new());
                self.lines.push("[Chain]".into());
                self.lines.len()
            }
            Some(h) => {
                let mut at = h + 1;
                while at < self.lines.len() && !self.lines[at].trim().starts_with('[') {
                    at += 1;
                }
                while at > h + 1 && self.lines[at - 1].trim().is_empty() {
                    at -= 1; // before the blank lines that end the section
                }
                at
            }
        };
        self.lines.insert(at, name.to_string());
        AddResult::Added
    }

    /// Takes every counted line for `name` (on or off) out. Returns whether one was there.
    pub fn remove(&mut self, name: &RelPath) -> bool {
        let mut drop: Vec<usize> = Vec::new();
        for i in self.counted() {
            let t = self.lines[i].trim();
            let body = t.trim_start_matches(';').trim();
            if !t.starts_with('#') && !t.starts_with("//") {
                if let Some(n) = dll_name(body) {
                    if RelPath::parse(&n).is_ok_and(|r| r.same(name)) {
                        drop.push(i);
                    }
                }
            }
        }
        for i in drop.iter().rev() {
            self.lines.remove(*i);
        }
        !drop.is_empty()
    }

    /// Whether any line (on or off) still names a DLL.
    pub fn names_any_dll(&self, host_dll: &str) -> bool {
        !self.parse(host_dll).entries.is_empty()
    }
}

impl Default for ChainFile {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddResult {
    Added,
    AlreadyOn,
    SwitchedOn,
    KeptOff,
}

/// The DLL a line body names, after a comment at the end, a `key=` and quotes are taken off; `None` for anything that
/// is not a `.dll` (a setting, a comment).
fn dll_name(body: &str) -> Option<String> {
    let mut line = body.trim().to_string();
    for mark in [" ;", "\t;", " #", "\t#"] {
        if let Some(at) = line.find(mark) {
            line = line[..at].trim().to_string();
        }
    }
    if let Some(eq) = line.find('=') {
        line = line[eq + 1..].trim().to_string();
    }
    if line.len() >= 2 && line.starts_with('"') && line.ends_with('"') {
        line = line[1..line.len() - 1].to_string();
    }
    if line.len() < 5 || !line.to_lowercase().ends_with(".dll") {
        return None;
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST: &str = "dinput8.dll";

    fn names(f: &ChainFile) -> Vec<(String, bool)> {
        f.parse(HOST).entries.iter().map(|e| (e.name.to_string(), e.on)).collect()
    }

    #[test]
    fn parses_by_emf_rules() {
        let text = "a.dll\r\n[Chain]\r\n; off.dll\r\n# note\r\n// note\r\nmods/x/y.dll ; comment\r\nkey = \"q.dll\"\r\n\
                    Delay=5\r\nC:\\evil.dll\r\n..\\up.dll\r\n\\\\srv\\s.dll\r\nDINPUT8.dll\r\na.dll\r\nnul.dll\r\n\
                    [Other]\r\nz.dll\r\n[chain]\r\nlast.dll\r\n";
        let f = ChainFile::decode(text.as_bytes());
        let p = f.parse(HOST);
        assert_eq!(
            names(&f),
            vec![
                ("a.dll".into(), true),
                ("off.dll".into(), false),
                ("mods\\x\\y.dll".into(), true),
                ("q.dll".into(), true),
                ("last.dll".into(), true)
            ]
        );
        assert_eq!(p.refused.len(), 5, "{:?}", p.refused); // drive, .., UNC, dinput8.dll, nul
    }

    #[test]
    fn caps_entries() {
        let text: String = (0..300).map(|i| format!("m{i}.dll\n")).collect();
        let f = ChainFile::decode(text.as_bytes());
        assert_eq!(f.parse(HOST).on().count(), MAX_ENTRIES);
    }

    #[test]
    fn keeps_encoding_and_other_sections() {
        let body = "[Other]\r\nkeep=1\r\n[Chain]\r\nfirst.dll\r\n\r\n[More]\r\nx=2\r\n";
        let mut utf16 = vec![0xFF, 0xFE];
        for u in body.encode_utf16() {
            utf16.extend_from_slice(&u.to_le_bytes());
        }
        let bom = [&[0xEF, 0xBB, 0xBF][..], body.as_bytes()].concat();
        let ansi = [body.as_bytes(), b"; caf\xe9.dll\r\n"].concat();
        for bytes in [body.as_bytes().to_vec(), bom, utf16, ansi] {
            let mut f = ChainFile::decode(&bytes);
            assert_eq!(f.encode(), bytes, "unchanged file is written back byte for byte");
            assert_eq!(f.add(&RelPath::of("new.dll"), HOST, true), AddResult::Added);
            let again = ChainFile::decode(&f.encode());
            assert_eq!(again.encoding, f.encoding);
            assert_eq!(&again.lines[..6], &["[Other]", "keep=1", "[Chain]", "first.dll", "new.dll", ""]);
            let mut back = again.clone();
            assert!(back.remove(&RelPath::of("NEW.dll")));
            assert_eq!(back.encode(), bytes);
        }
    }

    #[test]
    fn adds_where_emf_does() {
        let mut f = ChainFile::new();
        f.add(&RelPath::of("a.dll"), HOST, true);
        assert_eq!(f.lines, vec!["[Chain]", "a.dll"]);

        let mut f = ChainFile::decode(b"x.dll\n\n");
        f.add(&RelPath::of("a.dll"), HOST, true);
        assert_eq!(f.lines, vec!["x.dll", "a.dll", ""]);

        let mut f = ChainFile::decode(b"[Other]\nk=v\n");
        f.add(&RelPath::of("a.dll"), HOST, true);
        assert_eq!(f.lines, vec!["[Other]", "k=v", "", "[Chain]", "a.dll"]);
    }

    #[test]
    fn switched_off_lines() {
        let mut f = ChainFile::decode(b"[Chain]\n; a.dll\n");
        assert_eq!(f.add(&RelPath::of("a.dll"), HOST, false), AddResult::KeptOff);
        assert_eq!(f.lines[1], "; a.dll");
        assert_eq!(f.add(&RelPath::of("A.DLL"), HOST, true), AddResult::SwitchedOn);
        assert_eq!(f.lines[1], "A.DLL");
        assert_eq!(f.add(&RelPath::of("a.dll"), HOST, true), AddResult::AlreadyOn);
        assert!(f.remove(&RelPath::of("a.dll")));
        assert!(!f.names_any_dll(HOST));
    }
}
