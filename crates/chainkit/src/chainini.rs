//! A loader's list of DLLs: `dinput8-chain.ini` by EMF's documented rules ([`CHAIN`]), or ModLoader Proxy's
//! `chainloader.ini` ([`LOAD`]).
//!
//! Lines in the list's section (`[Chain]` or `[Load]`; for `dinput8-chain.ini` also lines before any section) count;
//! other sections, such as `[Options]`, `[Settings]` or EMF's `[EMF]`, are another tool's business and stay as they
//! are. Blank lines and `;` `#` `//` comments are skipped; in `dinput8-chain.ini` a `key=value` line counts with its
//! value when that names a `.dll`; `; ` in front of a DLL switches it off. Only DLLs inside the game folder count,
//! `dinput8.dll` never does, each DLL once, at most 256. The file is written back in the encoding it was found in, and
//! every line keeps its own line ending (a new line takes the file's first one; a new file has CRLF).

use crate::paths::RelPath;

pub const MAX_ENTRIES: usize = 256;

/// How a loader's list file is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// The section that lists the DLLs: `Chain` for `[Chain]`.
    pub section: &'static str,
    /// Lines before any section are in the list too.
    pub open_lines: bool,
    /// A `key=value` line names its value (`dinput8-chain.ini`); otherwise the whole line is a path.
    pub keys: bool,
    /// The setting that hands the game's own calls (DirectInput, for `dinput8.dll`) to one listed mod:
    /// (section, key).
    pub forward: (&'static str, &'static str),
}

/// `dinput8-chain.ini`, as XHL-Core began it and EMF, ShroudForge and dllchain read it: `[Chain]`, and `[Options]`
/// with `Forward=`.
pub const CHAIN: Format = Format { section: "Chain", open_lines: true, keys: true, forward: ("Options", "Forward") };

/// ModLoader Proxy's `chainloader.ini` (its src/main.cpp, read 2026-11-07): `[Load]` has one path per line, relative
/// to the file; `[Settings]` has `ForwardTo=`.
pub const LOAD: Format = Format { section: "Load", open_lines: false, keys: false, forward: ("Settings", "ForwardTo") };

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
    lines: Vec<String>,
    /// Each line's ending as found: `\r\n`, `\n`, or nothing (the last line of a file without a final line break).
    ends: Vec<&'static str>,
    pub encoding: Encoding,
    pub format: Format,
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
    /// A new `dinput8-chain.ini`.
    pub fn new() -> ChainFile {
        ChainFile::new_as(CHAIN)
    }

    pub fn new_as(format: Format) -> ChainFile {
        ChainFile { lines: Vec::new(), ends: Vec::new(), encoding: Encoding::Utf8, format }
    }

    /// `dinput8-chain.ini`.
    pub fn decode(bytes: &[u8]) -> ChainFile {
        ChainFile::decode_as(bytes, CHAIN)
    }

    pub fn decode_as(bytes: &[u8], format: Format) -> ChainFile {
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
        let mut f = ChainFile { encoding, ..ChainFile::new_as(format) };
        let mut pieces = text.split('\n').peekable();
        while let Some(piece) = pieces.next() {
            let last = pieces.peek().is_none();
            if last && piece.is_empty() {
                break; // the file ended with a line break
            }
            let (line, end) = match piece.strip_suffix('\r') {
                Some(l) if !last => (l, "\r\n"),
                _ => (piece, if last { "" } else { "\n" }),
            };
            f.lines.push(line.to_string());
            f.ends.push(end);
        }
        f
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The ending a new line gets: the file's first one, else CRLF.
    fn eol(&self) -> &'static str {
        self.ends.iter().copied().find(|e| !e.is_empty()).unwrap_or("\r\n")
    }

    /// Puts a line in at `at`. At the end of a file without a final line break, the line before gets one and the new
    /// last line goes without, as the file did.
    fn insert(&mut self, at: usize, line: String) {
        let eol = self.eol();
        let mut end = eol;
        if at == self.lines.len() {
            if let Some(last) = self.ends.last_mut().filter(|e| e.is_empty()) {
                *last = eol;
                end = "";
            }
        }
        self.lines.insert(at, line);
        self.ends.insert(at, end);
    }

    /// Takes line `i` out; a missing final line break passes to the line before it.
    fn remove_line(&mut self, i: usize) {
        self.lines.remove(i);
        let end = self.ends.remove(i);
        if end.is_empty() && i == self.lines.len() {
            if let Some(last) = self.ends.last_mut() {
                *last = "";
            }
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut text = String::new();
        for (l, e) in self.lines.iter().zip(&self.ends) {
            text.push_str(l);
            text.push_str(e);
        }
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

    /// Indexes of the lines in `section` (`None`: before any section).
    fn in_section(&self, section: Option<&str>) -> Vec<usize> {
        let mut current: Option<&str> = None;
        let mut out = Vec::new();
        for (i, l) in self.lines.iter().enumerate() {
            if let Some(name) = section_name(l) {
                current = Some(name);
                continue;
            }
            let here = match (current, section) {
                (None, None) => true,
                (Some(c), Some(s)) => c.eq_ignore_ascii_case(s),
                _ => false,
            };
            if here {
                out.push(i);
            }
        }
        out
    }

    /// Indexes of the lines that count: in the list's section, and before any section when the format says so.
    fn counted(&self) -> Vec<usize> {
        let mut out = self.in_section(Some(self.format.section));
        if self.format.open_lines {
            out.extend(self.in_section(None));
            out.sort_unstable();
        }
        out
    }

    /// The forward setting (`Forward=` in `[Options]`, `ForwardTo=` in `[Settings]`) when one is set: its value as
    /// written, without quotes or a comment at the end.
    pub fn forward(&self) -> Option<String> {
        let (section, key) = self.format.forward;
        self.in_section(Some(section)).into_iter().find_map(|i| {
            let t = self.lines[i].trim();
            if t.starts_with([';', '#']) || t.starts_with("//") {
                return None;
            }
            let (k, v) = t.split_once('=')?;
            let v = strip_comment(v.trim()).trim_matches('"').trim();
            (k.trim().eq_ignore_ascii_case(key) && !v.is_empty()).then(|| v.to_string())
        })
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
            let Some(name) = dll_name(body, self.format.keys) else { continue }; // a comment or a setting
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

    /// Lists `name` at the end of the list's section. A line that switched it off is switched on again when
    /// `switch_on`, else left off. Returns what happened.
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
        let section = self.format.section;
        let header_line = format!("[{section}]");
        if self.lines.is_empty() {
            self.insert(0, header_line);
            self.insert(1, name.to_string());
            return AddResult::Added;
        }
        let header = self.lines.iter().rposition(|l| section_name(l).is_some_and(|n| n.eq_ignore_ascii_case(section)));
        let any_section = self.lines.iter().any(|l| section_name(l).is_some());
        let at = match header {
            None if !any_section && self.format.open_lines => {
                let mut at = self.lines.len();
                while at > 0 && self.lines[at - 1].trim().is_empty() {
                    at -= 1;
                }
                at
            }
            None => {
                let end = self.lines.len();
                self.insert(end, String::new());
                self.insert(end + 1, header_line);
                self.lines.len()
            }
            Some(h) => {
                let mut at = h + 1;
                while at < self.lines.len() && section_name(&self.lines[at]).is_none() {
                    at += 1;
                }
                while at > h + 1 && self.lines[at - 1].trim().is_empty() {
                    at -= 1; // before the blank lines that end the section
                }
                at
            }
        };
        self.insert(at, name.to_string());
        AddResult::Added
    }

    /// Lists `name` before the list's first DLL, so its loader starts it first; with no DLL listed yet, as [`add`]
    /// does. A line that is already there stays where it is.
    ///
    /// [`add`]: ChainFile::add
    pub fn add_first(&mut self, name: &RelPath, host_dll: &str, switch_on: bool) -> AddResult {
        let p = self.parse(host_dll);
        match p.entries.first() {
            Some(first) if p.find(name).is_none() => {
                self.insert(first.line, name.to_string());
                AddResult::Added
            }
            _ => self.add(name, host_dll, switch_on),
        }
    }

    /// Takes every counted line for `name` (on or off) out. Returns whether one was there.
    pub fn remove(&mut self, name: &RelPath) -> bool {
        let mut drop: Vec<usize> = Vec::new();
        for i in self.counted() {
            let t = self.lines[i].trim();
            let body = t.trim_start_matches(';').trim();
            if !t.starts_with('#') && !t.starts_with("//") {
                if let Some(n) = dll_name(body, self.format.keys) {
                    if RelPath::parse(&n).is_ok_and(|r| r.same(name)) {
                        drop.push(i);
                    }
                }
            }
        }
        for i in drop.iter().rev() {
            self.remove_line(*i);
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

/// A section header's name: `Chain` for `[Chain]`.
fn section_name(line: &str) -> Option<&str> {
    let t = line.trim();
    t.strip_prefix('[').map(|r| r.strip_suffix(']').unwrap_or(r).trim())
}

/// A value without the comment at its end.
fn strip_comment(line: &str) -> &str {
    let mut line = line;
    for mark in [" ;", "\t;", " #", "\t#"] {
        if let Some(at) = line.find(mark) {
            line = line[..at].trim();
        }
    }
    line
}

/// The DLL a line body names, after a comment at the end, a `key=` (when `keys`) and quotes are taken off; `None` for
/// anything that is not a `.dll` (a setting, a comment).
fn dll_name(body: &str, keys: bool) -> Option<String> {
    let mut line = strip_comment(body.trim()).to_string();
    if let Some(eq) = line.find('=').filter(|_| keys) {
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

    /// The shapes other tools leave: XHL-Core's `[Options]` with `Forward=`, EMF's own `[EMF]` section, comments,
    /// LF or CRLF or both, with or without a final line break. Adding and taking out dllchain's line gives back the
    /// file byte for byte, and no other line changes or moves.
    #[test]
    fn round_trips_other_tools_files() {
        let shapes: [&[u8]; 6] = [
            b"; XHL-Core chain\n[Chain]\nfirst.dll\n\n[Options]\nForward=first.dll\n",
            b"[Chain]\r\nfirst.dll\r\n; off.dll\r\n\r\n[Options]\r\nForward = \"first.dll\" ; DirectInput\r\n",
            b"[EMF]\r\nversion=1\r\nmanaged=first.dll\r\n[Chain]\r\nfirst.dll\r\n[Options]\r\nForward=first.dll",
            b"[Chain]\nfirst.dll\r\nsecond.dll\n[EMF]\r\nx=1\n",
            b"first.dll\nsecond.dll",
            b"# hand made\n\nfirst.dll\n\n",
        ];
        for bytes in shapes {
            let shown = String::from_utf8_lossy(bytes);
            let f = ChainFile::decode(bytes);
            assert_eq!(f.encode(), bytes, "{shown}");
            let mut g = f.clone();
            assert_eq!(g.add(&RelPath::of("mine.dll"), HOST, true), AddResult::Added);
            let added = g.encode();
            let text = String::from_utf8_lossy(&added).into_owned();
            // every line of the other tool's file is still there, in its order
            let others: Vec<&String> = g.lines().iter().filter(|l| l.as_str() != "mine.dll").collect();
            assert_eq!(others, f.lines().iter().collect::<Vec<_>>(), "{text}");
            // the new line ends as the file's first line does
            let n = bytes.iter().position(|&b| b == b'\n').unwrap();
            let eol = if n > 0 && bytes[n - 1] == b'\r' { "\r\n" } else { "\n" };
            assert!(text.contains(&format!("mine.dll{eol}")) || text.ends_with("mine.dll"), "{text:?}");
            assert_eq!(g.forward(), f.forward());
            let mut back = ChainFile::decode(&added);
            assert!(back.remove(&RelPath::of("mine.dll")));
            assert_eq!(back.encode(), bytes, "{shown}");
        }
        assert_eq!(ChainFile::decode(shapes[0]).forward().as_deref(), Some("first.dll"));
        assert_eq!(ChainFile::decode(shapes[1]).forward().as_deref(), Some("first.dll"));
        assert_eq!(ChainFile::decode(shapes[4]).forward(), None);
        // a Forward= outside [Options] is not the setting, and a switched-off one is not set
        assert_eq!(ChainFile::decode(b"[Chain]\nForward=a.dll\n[Options]\n;Forward=b.dll\n").forward(), None);
        // the setting is no chain entry
        let opts = ChainFile::decode(b"[Chain]\nfirst.dll\n[Options]\nForward=other.dll\n");
        assert_eq!(names(&opts), vec![("first.dll".into(), true)]);
    }

    #[test]
    fn modloader_proxy_files() {
        // ModLoader Proxy's own chainloader.ini, as it ships
        let shipped = "; modloader-proxy configuration. Place next to the renamed proxy DLL.\r\n\r\n[Settings]\r\n\
                       Log = true\r\nDeferred = false\r\nDelayMs = 0\r\n\r\nLoadFromFolder = false\r\nModsFolder = mods\r\n\
                       ; ForwardTo = mods\\XHL\\dinput8.dll\r\n\r\n[Load]\r\n; Explicit entries, loaded first.\r\n\
                       ; ExampleMod.dll\r\n; somefolder\\AnotherMod.dll\r\n";
        let f = ChainFile::decode_as(shipped.as_bytes(), LOAD);
        assert_eq!(f.forward(), None);
        // its examples are switched off
        assert!(f.parse(HOST).on().next().is_none());
        let mut g = f.clone();
        assert_eq!(g.add(&RelPath::of("enstash.dll"), HOST, true), AddResult::Added);
        assert_eq!(g.lines().last().map(String::as_str), Some("enstash.dll"));
        assert_eq!(names(&g).last(), Some(&("enstash.dll".to_string(), true)));
        assert!(g.remove(&RelPath::of("enstash.dll")));
        assert_eq!(g.encode(), shipped.as_bytes());

        // a path with '=' is a path; lines before any section are not in the list; ForwardTo is read
        let f = ChainFile::decode_as(b"top.dll\n[Settings]\nForwardTo = mods\\x\\dinput8.dll\n[Load]\na=b.dll\n", LOAD);
        assert_eq!(names(&f), vec![("a=b.dll".into(), true)]);
        assert_eq!(f.forward().as_deref(), Some("mods\\x\\dinput8.dll"));

        // without [Load], one is added at the end; a new file starts with it
        let mut f = ChainFile::decode_as(b"[Settings]\nLog = true\n", LOAD);
        f.add(&RelPath::of("a.dll"), HOST, true);
        assert_eq!(f.lines(), ["[Settings]", "Log = true", "", "[Load]", "a.dll"]);
        let mut f = ChainFile::new_as(LOAD);
        f.add(&RelPath::of("a.dll"), HOST, true);
        assert_eq!(f.encode(), b"[Load]\r\na.dll\r\n");
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
    fn adds_first_before_the_first_dll_and_back() {
        let shapes: [&[u8]; 3] =
            [b"[Chain]\r\nenstash.dll\r\n", b"; note\n[Chain]\n; off.dll\nb.dll\n[Options]\nForward=b.dll", b"x.dll"];
        for bytes in shapes {
            let mut f = ChainFile::decode(bytes);
            let first = f.parse(HOST).entries[0].name.to_string();
            assert_eq!(f.add_first(&RelPath::of("old-host.dll"), HOST, true), AddResult::Added);
            let got = names(&f);
            assert_eq!(got[0].0, "old-host.dll");
            assert_eq!(got[1].0, first);
            assert_eq!(f.add_first(&RelPath::of("OLD-HOST.dll"), HOST, true), AddResult::AlreadyOn);
            assert!(f.remove(&RelPath::of("old-host.dll")));
            assert_eq!(f.encode(), bytes);
        }
        assert_eq!(
            ChainFile::decode(b"[Chain]\r\nenstash.dll\r\n").tap_first("a.dll"),
            b"[Chain]\r\na.dll\r\nenstash.dll\r\n"
        );
        // nothing listed yet: as add does
        assert_eq!(ChainFile::new().tap_first("a.dll"), b"[Chain]\r\na.dll\r\n");
    }

    impl ChainFile {
        fn tap_first(mut self, name: &str) -> Vec<u8> {
            self.add_first(&RelPath::of(name), HOST, true);
            self.encode()
        }
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
