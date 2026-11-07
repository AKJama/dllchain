//! Install, repair and uninstall, each in two halves: a [`Plan`] that says what would change (and changes nothing),
//! and [`apply`], which makes the changes in one undoable run and checks the folder afterwards.
//!
//! Who hosts: only the DLL called `dinput8.dll` runs, and it starts the others from `dinput8-chain.ini`. With a loader
//! that only loads mods as `dinput8.dll` (EMF's, XHL-Core, ModLoader Proxy; [`Game::loaders`]), every mod is a guest in
//! the file that loader reads, and the loader itself is never moved, replaced or deleted. Otherwise the host is the mod that is `dinput8.dll` now when it can
//! host, else the newest mod that can; a mod that cannot read the chain file is only ever the host when it is alone.
//! So the result is the same whatever order mods come in.

use crate::chainini::{AddResult, CHAIN, ChainFile, Format, Parsed};
use crate::dllinfo::{DllInfo, WordFrom, contains_text, inspect};
use crate::game::{Game, Loader, PackageHost, Requirement};
use crate::hash::{sha256_hex, short};
use crate::paths::{RelPath, is_word, resolve};
use crate::record::{FileRec, Kind, ModRec, RECORD, Record, clean_name, store_rel};
use crate::source::{ModSource, check_target, release_extra};
use crate::steam::is_game_folder;
use crate::txn::{Txn, recover};
use crate::{Error, OWN_DIR, Result, clock, fail};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const LOG: &str = "dllchain\\installer.log";

pub struct Ctx {
    pub dir: PathBuf,
    pub game: &'static Game,
    /// Whether Windows has `<name>.dll` in System32 (a renamed mod must not get that name).
    pub in_system32: fn(&str) -> bool,
}

pub fn system32_has(base: &str) -> bool {
    let root = std::env::var_os("SystemRoot").or_else(|| std::env::var_os("windir"));
    root.is_some_and(|r| Path::new(&r).join("System32").join(format!("{base}.dll")).exists())
}

impl Ctx {
    pub fn new(dir: &Path, game: &'static Game) -> Result<Ctx> {
        if !is_game_folder(dir, game) {
            fail!("{} is not {}'s folder: there is no {} in it", dir.display(), game.name, game.exes.join(" or "));
        }
        Ok(Ctx { dir: dir.to_path_buf(), game, in_system32: system32_has })
    }

    fn sha(&self, rel: &RelPath) -> Option<String> {
        resolve(&self.dir, rel).ok().and_then(|p| crate::hash::file_sha256(&p))
    }

    fn read(&self, rel: &RelPath) -> Option<Vec<u8>> {
        let p = resolve(&self.dir, rel).ok()?;
        if p.is_file() { std::fs::read(p).ok() } else { None }
    }

    fn exists(&self, rel: &RelPath) -> bool {
        resolve(&self.dir, rel).map(|p| p.exists()).unwrap_or(true)
    }

    /// A copy dllchain kept, only when it is still the one it kept.
    fn read_checked(&self, rel: &RelPath, sha: &str) -> Option<Vec<u8>> {
        self.read(rel).filter(|b| sha256_hex(b) == sha)
    }

    fn modified(&self, rel: &RelPath) -> Option<std::time::SystemTime> {
        resolve(&self.dir, rel).ok().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
    }

    fn host(&self) -> RelPath {
        RelPath::of(self.game.host_dll)
    }

    fn chain_rel(&self) -> RelPath {
        RelPath::of(self.game.chain_file)
    }

    /// The layout of a list file: a loader's own, else the chain file's.
    fn format_of(&self, list: &RelPath) -> Format {
        self.game.loaders.iter().find(|l| list.same(&RelPath::of(l.list))).map_or(CHAIN, |l| l.format)
    }

    /// Undoes a run that was cut off, when there is one.
    pub fn recover(&self) -> Result<Option<Vec<String>>> {
        recover(&self.dir)
    }
}

#[derive(Clone, Debug)]
enum Step {
    Write(RelPath, Vec<u8>),
    Rename(RelPath, RelPath),
    Delete(RelPath),
    RemoveDirIfEmpty(RelPath),
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    /// What the action is, for the log ("install AutoLootPlus.zip").
    pub action: String,
    /// What will change, one line each.
    pub summary: Vec<String>,
    /// What dllchain found and will not change, and warnings.
    pub notes: Vec<String>,
    /// Files that are there now, were not installed by dllchain, and would be replaced (after a backup).
    pub replaces: Vec<RelPath>,
    /// No record yet: the saves are backed up before this runs.
    pub first_install: bool,
    /// The mod being installed or uninstalled, as players see it (empty for a repair or uninstalling everything).
    pub name: String,
    /// Every file in the game folder the run changes, one line each ("mods\a\b.txt (new)"), for the details.
    pub files: Vec<String>,
    /// What the mod being installed needs and the game folder does not have: a warning, never a reason to refuse.
    pub missing: Vec<&'static Requirement>,
    /// For an uninstall: what each mod made while it ran (its log, backups, reports), with the name players see. The
    /// run deletes them only when asked to (`plan_uninstall_with`); otherwise they stay.
    pub made: Vec<(String, Vec<Made>)>,
    steps: Vec<Step>,
    record: Record,
    /// Files as they must be after the run (`None`: not there).
    expect_files: Vec<(RelPath, Option<String>)>,
    /// Chain lines as they must be after the run, and the list file they are in.
    expect_chain: Vec<(RelPath, bool)>,
    list: Option<RelPath>,
    remove_own_dir: bool,
    /// For tests: fail after this many steps.
    #[doc(hidden)]
    pub fail_after: Option<usize>,
}

impl Plan {
    /// Nothing on disk would change.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Undo can take the run back: dllchain's own folder, with its copies, stays.
    pub fn undoable(&self) -> bool {
        !self.remove_own_dir
    }

    /// The files the run takes out of the game folder for good (dllchain's own folder aside), by the folder they are
    /// in: `""` for the game folder itself, else `mods\x\`. A file the run puts back afterwards is not one of them.
    pub fn removed_by_folder(&self) -> Vec<(String, Vec<String>)> {
        // keyed on the folder in lower case, so one folder spelt two ways is one
        let mut out: BTreeMap<String, (String, Vec<String>)> = BTreeMap::new();
        for (i, s) in self.steps.iter().enumerate() {
            let Step::Delete(r) = s else { continue };
            if is_own(r) || put_back_after(&self.steps, i, r) {
                continue;
            }
            let folder = r.parent().map_or(String::new(), |p| format!("{p}\\"));
            out.entry(folder.to_lowercase()).or_insert_with(|| (folder, Vec::new())).1.push(r.file_name().to_string());
        }
        out.into_values()
            .map(|(folder, mut names)| {
                names.sort_by_key(|n| n.to_lowercase());
                (folder, names)
            })
            .collect()
    }
}

/// Whether the file is in dllchain's own folder.
fn is_own(r: &RelPath) -> bool {
    r.first_part().eq_ignore_ascii_case(OWN_DIR)
}

/// Where dllchain keeps its copy of a file of the mod's that it took out or replaced.
fn backup_of(word: &str, rel: &RelPath) -> Result<RelPath> {
    RelPath::parse(&format!("{OWN_DIR}\\originals\\{word}\\{rel}"))
}

/// Whether a step after `steps[i]` renames or writes a file onto `r` again.
fn put_back_after(steps: &[Step], i: usize, r: &RelPath) -> bool {
    steps[i + 1..].iter().any(|s| matches!(s, Step::Rename(_, b) | Step::Write(b, _) if b.same(r)))
}

/// What is in the game folder now.
struct Scan {
    record: Record,
    notes: Vec<String>,
    /// The file the host reads the guests from: the chain file, or the one the loader that hosts reads.
    list: RelPath,
    chain: Option<ChainFile>,
    parsed: Parsed,
    /// The list file the record's lines are in, when the host reads another one now (ModLoader Proxy came in as
    /// `dinput8.dll`, or left): dllchain's lines leave it.
    old_list: Option<(RelPath, Option<ChainFile>)>,
    /// `dinput8.dll` now, with what it is.
    host: Option<(Vec<u8>, DllInfo)>,
    /// Guests replaced by hand with another build of the same mod: (word, the file's bytes, what it is).
    rebuilt: Vec<(String, Vec<u8>, DllInfo)>,
}

impl Scan {
    fn rebuilt(&self, word: &str) -> Option<&(String, Vec<u8>, DllInfo)> {
        self.rebuilt.iter().find(|r| r.0 == word)
    }

    /// The loader that hosts, when `dinput8.dll` is one that only loads mods.
    fn loader(&self) -> Option<&'static Loader> {
        self.host.as_ref().and_then(|(_, i)| i.loader)
    }
}

fn scan(ctx: &Ctx) -> Result<Scan> {
    if ctx.exists(&RelPath::of(crate::txn::JOURNAL)) {
        fail!("an earlier dllchain run was not finished; start dllchain again to undo it first");
    }
    let mut notes = Vec::new();
    let mut record = match ctx.read(&RelPath::of(RECORD)) {
        Some(bytes) => {
            let (r, w) = Record::parse(&String::from_utf8_lossy(&bytes));
            notes.extend(w);
            r
        }
        None => Record::default(),
    };
    record.drop_incomplete(&mut notes);
    keep_safe(&mut record, ctx.game, &mut notes);
    let host = match ctx.read(&ctx.host()) {
        Some(bytes) => {
            let info = inspect(&bytes, ctx.game).map_err(|e| {
                Error(format!(
                    "{} in the game folder is {e}; dllchain leaves it alone and changes nothing",
                    ctx.game.host_dll
                ))
            })?;
            Some((bytes, info))
        }
        None => None,
    };
    let loader = host.as_ref().and_then(|(_, i)| i.loader);
    let list = loader.map_or_else(|| ctx.chain_rel(), |l| RelPath::of(l.list));
    let format = ctx.format_of(&list);
    let chain = ctx.read(&list).map(|b| ChainFile::decode_as(&b, format));
    let parsed = chain.as_ref().map(|c| c.parse(ctx.game.host_dll)).unwrap_or_default();
    let record_list = record.chain_file.clone().unwrap_or_else(|| ctx.chain_rel());
    let old_list = (!record_list.same(&list) && (record.chain_created || !record.chain.is_empty())).then(|| {
        let old = ctx.read(&record_list).map(|b| ChainFile::decode_as(&b, ctx.format_of(&record_list)));
        (record_list, old)
    });
    if !record.mods.is_empty() && old_list.is_none() {
        for (name, on) in &record.chain {
            match parsed.find(name) {
                None => notes
                    .push(format!("{list}: the line for {name} was taken out by someone else since dllchain last ran")),
                Some(e) if e.on != *on => notes.push(format!(
                    "{list}: {name} was switched {} by someone else since dllchain last ran",
                    if e.on { "on" } else { "off" }
                )),
                _ => {}
            }
        }
        for e in &parsed.entries {
            if !record.chain.iter().any(|(n, _)| n.same(&e.name)) {
                let what = match ctx.sha(&e.name) {
                    Some(s) => format!("SHA-256 {}", short(&s)),
                    None => "the file is not there".into(),
                };
                notes.push(format!("{list}: {} was added by someone else since dllchain last ran ({what})", e.name));
            }
        }
    }
    for r in &parsed.refused {
        notes.push(if format == CHAIN {
            format!("{list} line {} ignored, as every chain loader does: {} ({})", r.line + 1, r.text, r.why)
        } else {
            format!(
                "{list} line {}: {} is not a DLL in the game folder ({}); dllchain leaves it as it is",
                r.line + 1,
                r.text,
                r.why
            )
        });
    }
    let mut scan = Scan { record, notes, list, chain, parsed, old_list, host, rebuilt: Vec::new() };
    scan.rebuilt = scan.record.mods.iter().filter_map(|m| rebuilt(ctx, &scan, m)).collect();
    Ok(scan)
}

/// A guest's DLL that someone replaced by hand where dllchain left it, with a newer build of the same mod: dllchain
/// takes it as that mod's DLL from now on instead of putting its own copy back. It must still be a 64-bit DLL, not
/// EMF and not another mod on the list, and if it says which mod it is, that is this one. A missing file, or anything
/// else in its place, is still repaired. (`dinput8.dll` itself is [`Work::adopt_host`]'s.)
fn rebuilt(ctx: &Ctx, scan: &Scan, m: &ModRec) -> Option<(String, Vec<u8>, DllInfo)> {
    if !is_chained(ctx.game, m) || m.dll_at.same(&ctx.host()) || locate(ctx, scan, m).is_some() {
        return None;
    }
    let bytes = ctx.read(&m.dll_at)?;
    let info = inspect(&bytes, ctx.game).ok()?;
    let same_mod = match &info.word {
        None => true,
        // a second copy of a mod is "<word>-2"
        Some((w, _)) => {
            m.word == *w
                || m.word
                    .strip_prefix(w.as_str())
                    .and_then(|r| r.strip_prefix('-'))
                    .is_some_and(|n| n.parse::<u32>().is_ok())
        }
    };
    let theirs = scan.record.mods.iter().any(|o| o.dll_sha == info.sha256);
    (same_mod && info.loader.is_none() && !theirs).then(|| (m.word.clone(), bytes, info))
}

/// What a mod with these files needs and the game folder does not have. Only DLLs the game loads count: the host,
/// the chain's switched-on entries and the other proxy names.
fn missing(ctx: &Ctx, scan: &Scan, files: &[&RelPath]) -> Vec<&'static Requirement> {
    let needed: Vec<&'static Requirement> =
        ctx.game.requirements.iter().filter(|r| r.needed_by(files.iter().copied())).collect();
    if needed.is_empty() {
        return needed;
    }
    let mut loaded = vec![ctx.host()];
    let others = ctx.game.proxy_names.iter().map(|p| RelPath::of(&format!("{p}.dll")));
    for rel in scan.parsed.on().map(|e| e.name.clone()).chain(others) {
        if !loaded.iter().any(|l| l.same(&rel)) {
            loaded.push(rel);
        }
    }
    let dlls: Vec<Vec<u8>> = loaded.iter().filter_map(|r| ctx.read(r)).collect();
    needed.into_iter().filter(|r| !dlls.iter().any(|b| r.texts.iter().any(|t| contains_text(b, t)))).collect()
}

/// Leaves out what the record may not point at, whoever wrote it: a mod's DLL is a `.dll` next to the game that the
/// game, Steam or another loader does not own (or, for a mod that brings its own loader, that loader); its files are
/// ones a zip could install, besides that loader; its backups are in dllchain's `originals` folder.
fn keep_safe(record: &mut Record, game: &Game, notes: &mut Vec<String>) {
    let originals = format!("{OWN_DIR}\\originals\\");
    let host_stem = game.host_dll.trim_end_matches(".dll");
    record.mods.retain_mut(|m| {
        let at = &m.dll_at;
        let name = at.file_name().to_lowercase();
        let own_loader = is_own_loader(game, m);
        let dll_ok = at.is_top_level()
            && at.extension() == ".dll"
            && !game.protected_names.contains(&name.as_str())
            && (own_loader || game.proxy_names.iter().all(|p| *p == host_stem || format!("{p}.dll") != name));
        if !dll_ok && !game.package_hosts.iter().any(|h| is_package_dll(game, h, &m.word, at)) {
            notes.push(format!("record: {} would have its DLL at {at}, which dllchain never uses; left out", m.word));
            return false;
        }
        let (word, at) = (m.word.clone(), at.clone());
        for (what, list) in [("file", &mut m.files), ("old file", &mut m.retired), ("found file", &mut m.found)] {
            list.retain(|f| {
                let loader = own_loader && what != "old file" && f.rel.same(&at);
                let ok = (loader || check_target(&f.rel, game).is_ok())
                    && f.backup.as_ref().is_none_or(|b| b.key().starts_with(&originals));
                if !ok {
                    notes.push(format!("record: {word}'s {what} {} is not one dllchain installs; left out", f.rel));
                }
                ok
            });
        }
        true
    });
    if let Some(f) = record.chain_file.take() {
        if game.is_list_file(f.as_str()) {
            record.chain_file = Some(f);
        } else {
            notes.push(format!("record: {f} is no loader's list file dllchain knows; its chain lines are left out"));
            record.chain.clear();
            record.chain_created = false;
        }
    }
}

/// The folder a package host runs a mod from: `mods\<word>`.
fn package_dir(host: &PackageHost, word: &str) -> RelPath {
    RelPath::of(&format!("{}\\{word}", host.dir))
}

/// The mod's DLL there, under the name it shipped with: `mods\<word>\dinput8.dll`, or `mods\<word>-dll\dinput8.dll`
/// for a mod whose zip has a script package of its own in `mods\<word>\`.
fn is_package_dll(game: &Game, host: &PackageHost, word: &str, at: &RelPath) -> bool {
    [package_dir(host, word), native_dir(host, word)].iter().any(|d| d.join(game.host_dll).is_ok_and(|d| d.same(at)))
}

/// The DLL's own folder when the mod's zip already uses `mods\<word>\` for a script package: `mods\<word>-dll`.
fn native_dir(host: &PackageHost, word: &str) -> RelPath {
    package_dir(host, &format!("{word}-dll"))
}

/// A package host's declaration that loads the DLL dllchain puts next to it.
fn declaration(dll: &str) -> Vec<u8> {
    format!("[Plugin]\r\nEnabled=1\r\nDll={dll}\r\n").into_bytes()
}

/// A mod dllchain put into a package host's folder rather than the chain.
fn is_package(m: &ModRec) -> bool {
    !m.dll_at.is_top_level()
}

/// A mod that brings its own loader (`dbgcore.dll`, `powrprof.dll`): the game loads that by its Windows name, so it is
/// not in the chain. Its DLL is the loader, which is one of its files (or one it shares with another such mod).
fn is_own_loader(game: &Game, m: &ModRec) -> bool {
    m.dll_at.is_top_level() && game.is_own_loader_name(m.dll_at.as_str())
}

/// A mod in the chain (or the host alone).
fn is_chained(game: &Game, m: &ModRec) -> bool {
    !is_package(m) && !is_own_loader(game, m)
}

/// Whether the mod lists `rel` as its own file, or as one that was there before it.
fn lists(m: &ModRec, rel: &RelPath) -> bool {
    m.files.iter().chain(&m.found).any(|f| f.rel.same(rel))
}

/// Takes `rel`'s entry out of the mod's files or found files: with whether it was a found one.
fn take_listed(m: &mut ModRec, rel: &RelPath) -> Option<(FileRec, bool)> {
    for (found, list) in [(false, &mut m.files), (true, &mut m.found)] {
        if let Some(i) = list.iter().position(|f| f.rel.same(rel)) {
            return Some((list.remove(i), found));
        }
    }
    None
}

/// A loader that stops the game at a folder in the package host's folder without its off switch (EML, which wants
/// `mod.json` there), so the host's mods cannot run beside it; `None` for the host's own DLL and any other.
fn strict_loader(game: &Game, ph: &PackageHost, bytes: &[u8]) -> Option<&'static Requirement> {
    if inspect(bytes, game).ok().and_then(|i| i.word).is_some_and(|(w, _)| w == ph.word) {
        return None;
    }
    game.requirements.iter().find(|r| {
        r.dir.eq_ignore_ascii_case(ph.dir)
            && r.file.eq_ignore_ascii_case(ph.off_switch)
            && r.texts.iter().any(|t| contains_text(bytes, t))
    })
}

/// The loader in the game that runs DLL mods from folders of their own (ShroudForge): the game's `dinput8.dll`, or a
/// mod on dllchain's list (an older dllchain chained ShroudForge like any other mod).
fn package_host(ctx: &Ctx, scan: &Scan) -> Option<&'static PackageHost> {
    let host = scan.host.as_ref().and_then(|(_, i)| i.word.as_ref()).map(|w| w.0.as_str());
    let listed = scan.record.mods.iter().filter(|m| is_chained(ctx.game, m)).map(|m| m.word.as_str());
    host.into_iter().chain(listed).find_map(|w| ctx.game.package_host(w))
}

/// Where a mod's DLL is now: where dllchain left it, `dinput8.dll`, or any chain entry with the same bytes (EMF
/// renames the DLL it finds as `dinput8.dll`).
fn locate(ctx: &Ctx, scan: &Scan, m: &ModRec) -> Option<RelPath> {
    let mut cands = vec![m.dll_at.clone(), ctx.host()];
    cands.extend(scan.parsed.entries.iter().map(|e| e.name.clone()));
    cands.into_iter().find(|c| ctx.sha(c).as_deref() == Some(m.dll_sha.as_str()))
}

/// A mod's DLL: dllchain's copy, else the file where it is.
fn load_dll(ctx: &Ctx, scan: &Scan, m: &ModRec) -> Option<Vec<u8>> {
    let stored = ctx.read_checked(&store_rel(&m.word, &m.dll_sha), &m.dll_sha);
    stored.or_else(|| locate(ctx, scan, m).and_then(|r| ctx.read(&r)))
}

/// The plan being built, and the state it is built from.
struct Work<'a> {
    ctx: &'a Ctx,
    scan: &'a Scan,
    /// The mods in the chain (or the host alone).
    mods: Vec<ModRec>,
    /// The mods a package host runs from their own folders: never in the chain.
    packages: Vec<ModRec>,
    /// The mods that bring their own loader, which the game loads by itself: never in the chain.
    loaders: Vec<ModRec>,
    dlls: HashMap<String, Vec<u8>>,
    plan: Plan,
    /// DLL files to delete before anything moves: (where, its SHA-256, the mod whose DLL it is or was).
    drop: Vec<(RelPath, String, String)>,
    /// The mod whose chain line is switched on even if the player switched it off (the one being installed).
    switch_on: Option<String>,
    /// Uninstalling everything: several mods that cannot host are left as they are, not refused.
    allow_no_host: bool,
}

impl<'a> Work<'a> {
    fn new(ctx: &'a Ctx, scan: &'a Scan, action: String) -> Result<Work<'a>> {
        let mut w = Work {
            ctx,
            scan,
            mods: Vec::new(),
            packages: Vec::new(),
            loaders: Vec::new(),
            dlls: HashMap::new(),
            plan: Plan {
                action,
                notes: scan.notes.clone(),
                first_install: scan.record.mods.is_empty(),
                ..Plan::default()
            },
            drop: Vec::new(),
            switch_on: None,
            allow_no_host: false,
        };
        for m in &scan.record.mods {
            if is_package(m) {
                w.packages.push(m.clone());
                continue;
            }
            if is_own_loader(ctx.game, m) {
                w.loaders.push(m.clone());
                continue;
            }
            if let Some((_, bytes, info)) = scan.rebuilt(&m.word) {
                w.plan.summary.push(format!(
                    "The {} in your game is not the copy dllchain kept, so dllchain keeps yours from now on.",
                    name_of(ctx.game, m)
                ));
                let mut m = m.clone();
                m.dll_sha = info.sha256.clone();
                w.dlls.insert(m.word.clone(), bytes.clone());
                w.mods.push(m);
                continue;
            }
            match load_dll(ctx, scan, m) {
                Some(b) => {
                    w.dlls.insert(m.word.clone(), b);
                    w.mods.push(m.clone());
                }
                None => w.plan.notes.push(format!(
                    "{}'s DLL is gone and dllchain's copy of it too; it is taken off dllchain's list (install it again to bring it back)",
                    ctx.game.display(&m.word)
                )),
            }
        }
        Ok(w)
    }

    /// Every mod on the list.
    fn all(&self) -> impl Iterator<Item = &ModRec> {
        self.mods.iter().chain(&self.packages).chain(&self.loaders)
    }

    fn all_mut(&mut self) -> impl Iterator<Item = &mut ModRec> {
        self.mods.iter_mut().chain(&mut self.packages).chain(&mut self.loaders)
    }

    fn display(&self, word: &str) -> String {
        match self.all().find(|m| m.word == word) {
            Some(m) => name_of(self.ctx.game, m),
            None => self.ctx.game.display(word),
        }
    }

    fn unique_word(&self, word: &str) -> String {
        let mut w = word.to_string();
        let mut n = 2;
        while self.all().any(|m| m.word == w) {
            w = format!("{word}-{n}");
            n += 1;
        }
        w
    }

    /// A `dinput8.dll` that is neither a loader nor one of dllchain's mods: another mod copied over, or there before
    /// dllchain. It joins the chain like any mod (a newer copy of a mod on the list takes that mod's place); dllchain
    /// keeps a copy and never deletes it. With `installing`, the word and DLL of the mod being installed: a copy of that
    /// same tool (or that same loader) is replaced by the install, or taken on when it is the same file, and comes back
    /// on uninstall; its SHA-256 is returned.
    fn adopt_host(&mut self, installing: Option<(&str, &DllInfo)>) -> Option<String> {
        let (bytes, info) = self.scan.host.as_ref()?;
        if self.mods.iter().any(|m| m.dll_sha == info.sha256) {
            return None;
        }
        let host = self.ctx.host();
        let word = info.word.as_ref().map(|w| w.0.clone());
        let loader_word = |i: &DllInfo| i.loader.map(|l| l.word);
        let same_tool = installing
            .filter(|(w, new)| word.as_deref() == Some(*w) && loader_word(new) == loader_word(info))
            .map(|(w, new)| (w.to_string(), new.sha256 == info.sha256));
        // a loader put in by hand stays as it is, unless this installs that same loader over it
        if info.loader.is_some() && same_tool.is_none() {
            return None;
        }
        // a package host runs its mods without a chain: it joins one only when there is one already
        if self.mods.is_empty() && word.as_deref().is_some_and(|w| self.ctx.game.package_host(w).is_some()) {
            return None;
        }
        if let Some((w, identical)) = same_tool {
            let name = self.display(&w);
            if identical {
                self.plan.summary.push(format!(
                    "{name} is already in your game, the same file: dllchain lists it, and Uninstall leaves it."
                ));
            } else {
                self.plan.summary.push(format!("The new {name} takes the place of the older copy in your game."));
                self.plan.summary.push("dllchain keeps the older copy, and Uninstall puts it back.".into());
                self.drop.push((host, info.sha256.clone(), w.clone()));
            }
            self.write_store(&w, bytes);
            return Some(info.sha256.clone());
        }
        if let Some(i) = word.as_ref().and_then(|w| self.mods.iter().position(|m| &m.word == w)) {
            let old = self.mods[i].clone();
            if let Some(at) = locate(self.ctx, self.scan, &old) {
                if !at.same(&host) {
                    self.drop.push((at.clone(), old.dll_sha.clone(), old.word.clone()));
                }
            }
            self.plan.summary.push(format!(
                "The {} in your game is newer than dllchain's copy, so dllchain uses it from now on.",
                self.display(&old.word)
            ));
            let m = &mut self.mods[i];
            m.dll_sha = info.sha256.clone();
            m.dll_at = host;
            self.dlls.insert(old.word.clone(), bytes.clone());
            return None;
        }
        let w = self.unique_word(&word.unwrap_or_else(|| "dinput8-previous".into()));
        let name = default_name(info).unwrap_or_else(|| self.display(&w));
        self.plan.summary.push(format!("{name} is already in your game and keeps working."));
        let order = self.next_order();
        self.mods.push(ModRec {
            at: clock::now_text(),
            source_sha: "-".into(),
            label: format!("found as {}", self.ctx.game.host_dll),
            name: default_name(info),
            dll_sha: info.sha256.clone(),
            version: info.version.as_deref().and_then(clean_name),
            ..ModRec::new(w.clone(), Kind::Adopted, order, host)
        });
        self.dlls.insert(w, bytes.clone());
        None
    }

    /// "enstash keeps working.", unless a line already says what happens to that mod.
    fn keeps_working(&mut self, name: &str) {
        let start = format!("{name} ");
        if !self.plan.summary.iter().any(|l| l.starts_with(&start)) {
            self.plan.summary.push(format!("{name} keeps working."));
        }
    }

    fn next_order(&self) -> u32 {
        self.all().map(|m| m.order).max().unwrap_or(0).max(self.scan.record.next_order().saturating_sub(1)) + 1
    }

    fn write_store(&mut self, word: &str, bytes: &[u8]) {
        let rel = store_rel(word, &sha256_hex(bytes));
        if self.ctx.read(&rel).is_none_or(|b| b != bytes)
            && !self.plan.steps.iter().any(|s| matches!(s, Step::Write(r, _) if r.same(&rel)))
        {
            self.plan.steps.push(Step::Write(rel, bytes.to_vec()));
        }
    }

    /// Picks the host and the guests' names, and adds the steps that move the DLLs and edit the chain file.
    fn layout(&mut self) -> Result<()> {
        let ctx = self.ctx;
        let host = ctx.host();
        let n = self.mods.len();
        // (it can start the others, it is a loader dllchain installs)
        let (can_host, is_loader): (Vec<bool>, Vec<bool>) = self
            .mods
            .iter()
            .map(|m| {
                inspect(&self.dlls[&m.word], ctx.game)
                    .map_or((false, false), |i| (i.can_host, i.loader.is_some_and(|l| l.install.is_none())))
            })
            .unzip();
        let located: Vec<Option<RelPath>> = self.mods.iter().map(|m| locate(ctx, self.scan, m)).collect();
        // a DLL of ours that goes before anything moves (only while it is still the one dllchain means)
        let dropping = |r: &RelPath, drop: &[(RelPath, String, String)]| {
            drop.iter().any(|(d, s, _)| d.same(r) && ctx.sha(r).as_deref() == Some(s.as_str()))
        };
        // the mod whose DLL is dinput8.dll now, and stays there unless another mod takes its place
        let holder =
            (0..n).find(|&i| located[i].as_ref().is_some_and(|l| l.same(&host)) && !dropping(&host, &self.drop));
        // the mod that hosts now: also one being updated, whose older DLL there goes for the new one
        let current = holder.or_else(|| {
            let (_, _, word) =
                self.drop.iter().find(|(d, s, _)| d.same(&host) && ctx.sha(&host).as_deref() == Some(s.as_str()))?;
            self.mods.iter().position(|m| &m.word == word)
        });

        // a loader that refuses every other DLL next to the game must stay dinput8.dll itself
        let package_host = (0..n).find(|&i| ctx.game.package_host(&self.mods[i].word).is_some());
        // a loader that is dinput8.dll now and stays: one dllchain did not install, or its own (the holder)
        let loader_stays = self.scan.loader().is_some() && !dropping(&host, &self.drop);
        // a loader on dllchain's list (being installed, or updated) hosts, and the others are its guests
        let loader_mod = (0..n).filter(|&i| is_loader[i]).max_by_key(|&i| self.mods[i].order);
        // a loader that goes gives dinput8.dll back to the mod it took it from, which it listed first
        let first_listed = || {
            let parsed = self.scan.chain.as_ref()?.parse(ctx.game.host_dll);
            (0..n)
                .filter(|&i| can_host[i])
                .filter_map(|i| Some((i, parsed.find(located[i].as_ref()?)?.line)))
                .min_by_key(|&(_, line)| line)
                .map(|(i, _)| i)
        };
        let loader_goes = self.scan.loader().is_some() && !loader_stays;
        let host_i = if loader_stays || n == 0 {
            holder.filter(|_| loader_stays)
        } else if loader_mod.is_some() {
            loader_mod
        } else if n == 1 {
            Some(0)
        } else if package_host.is_some() {
            package_host
        } else if let Some(h) = current.filter(|&h| can_host[h]) {
            Some(h)
        } else if let Some(h) = first_listed().filter(|_| loader_goes) {
            Some(h)
        } else if let Some(h) = (0..n).filter(|&i| can_host[i]).max_by_key(|&i| self.mods[i].order) {
            Some(h)
        } else if self.allow_no_host {
            let h = (0..n).max_by_key(|&i| self.mods[i].order).unwrap_or(0);
            self.plan.notes.push(format!(
                "Only {} will run: none of {} can start the others. The others stay in the game folder.",
                self.display(&self.mods[h].word),
                self.mods.iter().map(|m| self.display(&m.word)).collect::<Vec<_>>().join(", ")
            ));
            Some(h)
        } else {
            fail!(
                "{} cannot run together: none of them reads {}, so whichever is {} would not start the others. A loader such as EMF can host them all.",
                self.mods.iter().map(|m| self.display(&m.word)).collect::<Vec<_>>().join(" and "),
                ctx.game.chain_file,
                ctx.game.host_dll
            );
        };
        if let Some(h) = host_i {
            if !can_host[h] {
                let others: Vec<String> = self
                    .scan
                    .parsed
                    .on()
                    .filter(|e| !self.mods.iter().any(|m| m.dll_at.same(&e.name)))
                    .map(|e| e.name.to_string())
                    .collect();
                if !others.is_empty() {
                    self.plan.notes.push(format!(
                        "{} cannot start other mods, so these will not run: {}",
                        self.display(&self.mods[h].word),
                        others.join(", ")
                    ));
                }
            }
        }

        // targets
        let mut targets: Vec<RelPath> = Vec::with_capacity(n);
        for (i, (m, loc)) in self.mods.iter().zip(&located).enumerate() {
            let t = if Some(i) == host_i {
                host.clone()
            } else {
                let sha = &m.dll_sha;
                let taken = |r: &RelPath, targets: &[RelPath]| {
                    targets.iter().any(|t| t.same(r))
                        || r.same(&host)
                        || (ctx.exists(r) && ctx.sha(r).as_deref() != Some(sha.as_str()) && !dropping(r, &self.drop))
                        // someone else's line for a DLL that is not there (this mod's own line is not)
                        || self.scan.parsed.entries.iter().any(|e| {
                            e.name.same(r)
                                && !r.same(&m.dll_at)
                                && ctx.sha(r).as_deref() != Some(sha.as_str())
                                && !dropping(r, &self.drop)
                        })
                };
                match loc {
                    Some(l) if !l.same(&host) && !taken(l, &targets) => l.clone(),
                    _ => {
                        let mut base = m.word.clone();
                        if ctx.game.proxy_names.contains(&base.as_str())
                            || (ctx.in_system32)(&base)
                            || ctx.game.renamed_by_others(&base)
                        {
                            base.push_str("-mod");
                        }
                        let mut k = 1;
                        loop {
                            let name = if k == 1 { format!("{base}.dll") } else { format!("{base}-{k}.dll") };
                            let r = RelPath::parse(&name)?;
                            if !taken(&r, &targets) {
                                break r;
                            }
                            k += 1;
                            if k > 99 {
                                fail!("too many files called {base}-<n>.dll in the game folder");
                            }
                        }
                    }
                }
            };
            targets.push(t);
        }

        // store copies first, so every DLL is safe before anything moves
        let words: Vec<String> = self.mods.iter().map(|m| m.word.clone()).collect();
        for w in words {
            let b = self.dlls[&w].clone();
            self.write_store(&w, &b);
        }
        for (r, sha, _) in self.drop.clone() {
            if ctx.sha(&r).as_deref() == Some(sha.as_str()) {
                self.plan.steps.push(Step::Delete(r.clone()));
                self.plan.expect_files.push((r, None));
            }
        }
        // the DLL in dinput8.dll moves out first
        let mut moved_from: Vec<RelPath> = Vec::new();
        if let Some(h) = holder {
            if !targets[h].same(&host) {
                let w = self.display(&self.mods[h].word);
                if ctx.sha(&targets[h]).as_deref() == Some(self.mods[h].dll_sha.as_str()) {
                    self.plan.steps.push(Step::Delete(host.clone()));
                } else {
                    self.plan.steps.push(Step::Rename(host.clone(), targets[h].clone()));
                    self.keeps_working(&w);
                }
                moved_from.push(host.clone());
            }
        }
        for (i, t) in targets.iter().enumerate() {
            if Some(i) == holder && !t.same(&host) {
                continue;
            }
            let w = self.display(&self.mods[i].word);
            let sha = self.mods[i].dll_sha.clone();
            // (not when a step above deletes that file: the same bytes as another mod's that goes)
            let in_place = ctx.sha(t).as_deref() == Some(sha.as_str())
                && !(t.same(&host) && moved_from.iter().any(|m| m.same(&host)))
                && !dropping(t, &self.drop);
            if in_place {
                // a second copy dllchain left elsewhere goes
                let old = &self.mods[i].dll_at;
                if !old.same(t)
                    && ctx.sha(old).as_deref() == Some(sha.as_str())
                    && !targets.iter().any(|x| x.same(old))
                    && !old.same(&host)
                {
                    self.plan.steps.push(Step::Delete(old.clone()));
                    self.plan.expect_files.push((old.clone(), None));
                }
            } else if let Some(l) = located[i].as_ref().filter(|l| !l.same(t) && !moved_from.iter().any(|m| m.same(l)))
            {
                self.plan.steps.push(Step::Rename(l.clone(), t.clone()));
                moved_from.push(l.clone());
                self.keeps_working(&w);
            } else {
                self.plan.steps.push(Step::Write(t.clone(), self.dlls[&self.mods[i].word].clone()));
                match self.scan.record.mods.iter().find(|m| m.word == self.mods[i].word) {
                    None => self.plan.summary.push(format!("{w} will be added to your game.")),
                    // under another name when something else is where it was
                    Some(m) if m.dll_sha == sha && m.dll_at.same(t) => {
                        self.plan.summary.push(format!("{w} is put back from dllchain's copy."))
                    }
                    Some(m) if m.dll_sha == sha => {
                        self.plan.summary.push(format!("{w} is put back from dllchain's copy, as {t}."))
                    }
                    // a new version, which plan_install words
                    Some(_) => {}
                }
            }
            self.plan.expect_files.push((t.clone(), Some(sha)));
        }

        // the chain file, or the list file of the loader that hosts
        let list = self.scan.list.clone();
        let mut chain = self.scan.chain.clone().unwrap_or_else(|| ChainFile::new_as(ctx.format_of(&list)));
        let before = chain.clone();
        let mut ours: Vec<RelPath> =
            self.scan.record.mods.iter().filter(|m| is_chained(ctx.game, m)).map(|m| m.dll_at.clone()).collect();
        ours.extend(located.iter().flatten().cloned());
        ours.extend(self.drop.iter().map(|d| d.0.clone()));
        let guests: Vec<usize> = (0..n).filter(|&i| !targets[i].same(&host)).collect();
        // the file list shows the chain file changing; the lines above say what that means for the player
        for r in &ours {
            if !r.same(&host) && !guests.iter().any(|&g| targets[g].same(r)) {
                chain.remove(r);
            }
        }
        let mut expect_chain = Vec::new();
        for &g in &guests {
            let word = &self.mods[g].word;
            let listed_before = self.scan.parsed.find(&targets[g]).is_some();
            let on = self.switch_on.as_deref() == Some(word.as_str()) || !listed_before;
            // the mod that was dinput8.dll ran before the others, and still does
            let r = if Some(g) == holder {
                chain.add_first(&targets[g], ctx.game.host_dll, on)
            } else {
                chain.add(&targets[g], ctx.game.host_dll, on)
            };
            if r == AddResult::KeptOff {
                self.plan.notes.push(format!(
                    "{} is switched off, so it stays off. To switch it on, take the '; ' away from the start of its line in {list}",
                    self.display(word),
                ));
            }
            if r == AddResult::SwitchedOn {
                self.plan.summary.push(format!("{} is switched back on.", self.display(word)));
            }
            expect_chain.push((targets[g].clone(), r != AddResult::KeptOff));
        }
        for e in &self.scan.parsed.entries {
            let mine = ours.iter().any(|o| o.same(&e.name)) || targets.iter().any(|t| t.same(&e.name));
            if !mine {
                expect_chain.push((e.name.clone(), e.on)); // others' lines stay as they are
            }
        }
        // the record's `chain-created` is about the file its lines are in
        let mut created = self.scan.record.chain_created && self.scan.old_list.is_none();
        if chain != before {
            if let Some(l) = self.scan.loader().filter(|_| loader_stays && !guests.is_empty()) {
                let (name, host) = (l.name, ctx.game.host_dll);
                self.plan.notes.push(format!(
                    "{name} is {host}, so dllchain lists the mods in {list}, where {name} loads them, and leaves {name} itself as it is."
                ));
            }
            created = self.write_list(&list, self.scan.chain.as_ref(), &chain, created);
        }
        // the lines in the file the host read before go: dllchain's mods are in the new host's file now
        if let Some((old_rel, Some(old))) = &self.scan.old_list {
            let mut c = old.clone();
            for r in &ours {
                c.remove(r);
            }
            if c != *old {
                self.plan.summary.push(format!("dllchain's lines move from {old_rel} to {list}."));
                self.write_list(old_rel, Some(old), &c, self.scan.record.chain_created);
            }
        }
        self.plan.expect_chain = expect_chain;
        self.plan.list = Some(list.clone());

        for (m, t) in self.mods.iter_mut().zip(&targets) {
            m.dll_at = t.clone();
        }
        // dllchain's copies of DLLs no mod uses any more
        let keep: Vec<RelPath> = self
            .mods
            .iter()
            .flat_map(|m| {
                std::iter::once(store_rel(&m.word, &m.dll_sha))
                    .chain(m.replaced_dll.iter().map(|r| store_rel(&m.word, r)))
            })
            .collect();
        for m in self.scan.record.mods.iter().filter(|m| !is_own_loader(ctx.game, m)) {
            for sha in std::iter::once(&m.dll_sha).chain(m.replaced_dll.iter()) {
                let r = store_rel(&m.word, sha);
                if !keep.iter().any(|k| k.same(&r)) && ctx.exists(&r) {
                    self.plan.steps.push(Step::Delete(r));
                }
            }
        }
        self.plan.record = Record {
            mods: self.all().cloned().collect(),
            chain_created: created,
            chain: chain.parse(ctx.game.host_dll).entries.iter().map(|e| (e.name.clone(), e.on)).collect(),
            chain_file: (!list.same(&ctx.chain_rel())).then_some(list),
        };
        Ok(())
    }

    /// Writes a list file as `chain` has it, or deletes it when dllchain made it (`created`) and no DLL is left in it.
    /// Returns whether dllchain made the file that is there afterwards.
    fn write_list(&mut self, rel: &RelPath, now: Option<&ChainFile>, chain: &ChainFile, created: bool) -> bool {
        if !chain.names_any_dll(self.ctx.game.host_dll) && (created || now.is_none()) {
            if now.is_some() {
                self.plan.steps.push(Step::Delete(rel.clone()));
            }
            self.plan.expect_files.push((rel.clone(), None));
            return false;
        }
        let bytes = chain.encode();
        self.plan.expect_files.push((rel.clone(), Some(sha256_hex(&bytes))));
        self.plan.steps.push(Step::Write(rel.clone(), bytes));
        created || now.is_none()
    }

    /// A single mod dllchain did not install, back as dinput8.dll with no chain: nothing left to keep track of, so it
    /// leaves dllchain's list (the mods a package host runs from their own folders stay).
    fn forget_lone_host(&mut self) {
        let ctx = self.ctx;
        let chained: Vec<usize> =
            (0..self.plan.record.mods.len()).filter(|&i| is_chained(ctx.game, &self.plan.record.mods[i])).collect();
        let [i] = chained[..] else { return };
        let m = &self.plan.record.mods[i];
        if m.kind != Kind::Adopted || !m.dll_at.same(&ctx.host()) {
            return;
        }
        let m = self.plan.record.mods.remove(i);
        let r = store_rel(&m.word, &m.dll_sha);
        self.plan.steps.retain(|s| !matches!(s, Step::Write(x, _) if x.same(&r)));
        if ctx.exists(&r) {
            self.plan.steps.push(Step::Delete(r));
        }
        self.plan.record.chain.clear();
    }

    /// When the list file's forward setting (`Forward=`, `ForwardTo=`) names a file this run takes away, says so
    /// plainly. The setting is the host's, not dllchain's, so dllchain leaves it as it is (and Uninstall gives back the
    /// host's file as it was, apart from dllchain's own lines); the player decides what gets the game's calls now.
    fn say_forward_gone(&mut self) {
        let Some(chain) = &self.scan.chain else { return };
        let Some(target) = chain.forward().and_then(|v| RelPath::parse(&v).ok()) else { return };
        if !self.ctx.exists(&target) {
            return;
        }
        let mut there = true;
        for s in &self.plan.steps {
            match s {
                Step::Write(r, _) if r.same(&target) => there = true,
                Step::Delete(r) if r.same(&target) => there = false,
                Step::Rename(a, b) if a.same(&target) || b.same(&target) => there = b.same(&target),
                _ => {}
            }
        }
        if there {
            return;
        }
        // (a loader's name comes from `Game::display`, which knows the loaders)
        let host = self.scan.host.as_ref().and_then(|(_, info)| {
            default_name(info).or_else(|| info.word.as_ref().map(|w| self.ctx.game.display(&w.0)))
        });
        let host = host.unwrap_or_else(|| "the loader".into());
        let (section, key) = chain.format.forward;
        let list = &self.scan.list;
        self.plan.summary.push(format!(
            "The {key}= line in [{section}] of {list} hands the game's own calls to {target}, which this takes away. dllchain leaves that line as it is, as it is {host}'s setting: change or remove it in {list}, or {host} looks for a file that is not there."
        ));
    }

    /// Writes the record last (or plans to remove dllchain's folder when nothing is left).
    fn finish(mut self) -> Plan {
        self.say_forward_gone();
        let rel = RelPath::of(RECORD);
        if self.plan.record.is_empty() {
            if self.ctx.exists(&RelPath::of(OWN_DIR)) {
                self.plan.remove_own_dir = true;
                if !self.plan.steps.is_empty() {
                    self.plan.summary.push("dllchain's own folder is removed too: nothing is left to undo.".into());
                }
            }
        } else {
            // a mod whose entry is otherwise as it was (the same zip again) keeps when it was installed
            for m in &mut self.plan.record.mods {
                if let Some(old) = self.scan.record.mods.iter().find(|o| o.word == m.word && o.at != m.at) {
                    if *old == (ModRec { at: old.at.clone(), ..m.clone() }) {
                        m.at = old.at.clone();
                    }
                }
            }
            let text = self.plan.record.to_text().into_bytes();
            if self.ctx.read(&rel).as_deref() != Some(text.as_slice()) {
                // a record-only change with nothing else said: the layout followed another tool's move
                if self.plan.steps.is_empty() && self.plan.summary.is_empty() {
                    self.plan.summary.push("dllchain notes that another tool moved a mod.".into());
                }
                self.plan.expect_files.push((rel.clone(), Some(sha256_hex(&text))));
                self.plan.steps.push(Step::Write(rel, text));
            }
        }
        // renames first, then the files next to the game, then the mods' folders, each in name order
        let own = is_own;
        let steps = &self.plan.steps;
        let mut renamed_away: Vec<&RelPath> = Vec::new();
        let mut lines: Vec<(u8, String, String)> = Vec::new();
        for (i, s) in steps.iter().enumerate() {
            let (key, rel, line) = match s {
                Step::Write(r, _) if !own(r) => {
                    let there = self.ctx.exists(r) && !renamed_away.iter().any(|g| g.same(r));
                    (1, r, format!("{r} ({})", if there { "replaced" } else { "new" }))
                }
                Step::Rename(a, b) => {
                    renamed_away.push(a);
                    (0, a, format!("{a} renamed to {b}"))
                }
                // a file another one is renamed or written onto later: that step says it
                Step::Delete(r) if !own(r) && !put_back_after(steps, i, r) => (1, r, format!("{r} (deleted)")),
                _ => continue,
            };
            let key = if key == 1 && !rel.is_top_level() { 2 } else { key };
            lines.push((key, rel.key(), line));
        }
        lines.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        self.plan.files = lines.into_iter().map(|(_, _, l)| l).collect();
        if self.plan.summary.is_empty() && !self.plan.files.is_empty() {
            let k = self.plan.files.len();
            self.plan.summary.push(if k == 1 { "Tidies up 1 file.".into() } else { format!("Tidies up {k} files.") });
        }
        self.plan
    }
}

/// Install (or update) the mod in `src`.
pub fn plan_install(ctx: &Ctx, src: &ModSource) -> Result<Plan> {
    let scan = scan(ctx)?;
    let info = inspect(&src.dll, ctx.game)?;
    if let Some(l) = info.loader {
        check_loader_install(ctx, &scan, src, l)?;
    }
    let action = format!("install {} (SHA-256 {})", src.label, src.sha256);
    if src.own_loader(ctx.game) {
        return plan_install_own_loader(ctx, &scan, src, &info, action);
    }
    let word = match &info.word {
        Some((w, WordFrom::Known | WordFrom::VersionInfo)) => w.clone(),
        None => src.word_hint.clone(),
    };
    if !is_word(&word) {
        fail!("{}: could not make a file name from it", src.label);
    }
    if let Some(m) = scan.record.mods.iter().find(|m| m.word == word && is_own_loader(ctx.game, m)) {
        fail!(
            "{} is on dllchain's list as a mod that brings its own loader ({}), and {} ships as {}. Uninstall it first if this is the same mod.",
            name_of(ctx.game, m),
            m.dll_at,
            src.label,
            ctx.game.host_dll
        );
    }
    if let Some(ph) = package_host(ctx, &scan) {
        return plan_install_package(ctx, &scan, src, &info, &word, ph, action);
    }
    let mut w = Work::new(ctx, &scan, action)?;
    let replaced_now = w.adopt_host(Some((&word, &info)));
    w.switch_on = Some(word.clone());
    let display = w.display(&word);
    w.plan.name = match w.mods.iter().find(|m| m.word == word) {
        Some(_) => display.clone(),
        None => default_name(&info).unwrap_or_else(|| display.clone()),
    };

    let prev = w.mods.iter().position(|m| m.word == word);
    let mut rec = match prev {
        Some(i) => {
            let old = w.mods[i].clone();
            if old.dll_sha == info.sha256 {
                w.plan.notes.push(format!("{display} is already installed, so dllchain only checks its files"));
            } else {
                w.plan.summary.push(format!("{display} will be updated to the new version."));
                if let Some(at) = locate(ctx, &scan, &old) {
                    w.drop.push((at, old.dll_sha.clone(), word.clone()));
                }
            }
            let mut r = old.clone();
            if old.kind == Kind::Adopted {
                // installed by hand before; it comes back on uninstall, so its copy must be there
                r.replaced_dll = Some(old.dll_sha.clone());
                let b = w.dlls[&word].clone();
                w.write_store(&word, &b);
                r.kind = Kind::Installed;
            }
            r
        }
        None => ModRec::new(word.clone(), Kind::Installed, w.next_order(), ctx.host()),
    };
    if replaced_now.is_some() {
        rec.replaced_dll = replaced_now;
    }
    fill_source(&mut rec, src, &info);

    let before = w.plan.summary.len();
    retire(&mut w, &mut rec, None, &src.files, None)?;
    plan_files(&mut w, &mut rec, &src.files, &src.label, src.files.is_empty(), &|_| false, NOT_IN_NEW)?;
    if let Some(i) = prev {
        w.mods[i] = rec;
    } else {
        w.mods.push(rec);
    }
    w.dlls.insert(word, src.dll.clone());
    // the DLLs first, then the mod's other files
    let file_lines = w.plan.summary.split_off(before);
    w.layout()?;
    // the mod being installed first, then what that means for the others
    let added = format!("{display} will be added to your game.");
    if let Some(i) = w.plan.summary.iter().position(|l| *l == added) {
        let l = w.plan.summary.remove(i);
        w.plan.summary.insert(0, l);
        // a loader takes over from the mod that was the host
        if let Some(l) = info.loader.filter(|_| scan.loader().is_none() && w.mods.len() > 1) {
            let (host, list) = (ctx.game.host_dll, l.list);
            w.plan.summary.insert(1, format!("{display} becomes {host} and starts the other mods from {list}."));
        }
    }
    w.plan.summary.extend(file_lines);
    source_notes(&mut w, src);
    let files: Vec<&RelPath> = src.files.iter().map(|(r, _)| r).collect();
    w.plan.missing = missing(ctx, &scan, &files);
    Ok(w.finish())
}

/// Whether dllchain may install the loader `l` (its DLL is `src`) as the host: only one the game's table says it
/// installs, and never over another loader or next to a loader that runs mods from folders of their own.
fn check_loader_install(ctx: &Ctx, scan: &Scan, src: &ModSource, l: &Loader) -> Result<()> {
    if let Some(how) = l.install {
        fail!(
            "{} is {}, a loader for other mods, not a mod: {how}. Once it is in, dllchain adds the mods you install to its list.",
            src.label,
            l.name
        );
    }
    let host = ctx.game.host_dll;
    if let Some(ph) = package_host(ctx, scan) {
        fail!(
            "{} is {}, a loader for other mods, and {} runs your mods from {}\\<mod>\\ as {host}: only one of them can be {host}. Take {} out first if you want {} instead.",
            src.label,
            l.name,
            ctx.game.display(ph.word),
            ph.dir,
            ctx.game.display(ph.word),
            l.name
        );
    }
    if let Some(other) = scan.loader().filter(|o| o.word != l.word) {
        fail!(
            "{} is {}, a loader for other mods, and {host} is already {}, another one. dllchain never replaces one loader with another: remove {} yourself first if you want {} instead.",
            src.label,
            l.name,
            other.name,
            other.name,
            l.name
        );
    }
    Ok(())
}

/// What the install records about where the mod came from.
fn fill_source(rec: &mut ModRec, src: &ModSource, info: &DllInfo) {
    rec.at = clock::now_text();
    rec.source_sha = src.sha256.clone();
    rec.label = src.label.clone();
    rec.from_url = src.from_url.clone();
    if rec.name.is_none() {
        rec.name = default_name(info);
    }
    rec.dll_sha = info.sha256.clone();
    rec.version = info.version.as_deref().and_then(clean_name).or_else(|| version_in(&src.label));
}

/// A version in a file name: `SmartQuickStack-0.12.2-beta-03c1cbd.zip` gives `0.12.2-beta`. Only numbers with dots
/// count (Nexus names keep the mod's number and dashes, which say nothing certain).
pub fn version_in(label: &str) -> Option<String> {
    // a folder's label ends with a backslash
    let trimmed = label.trim_end_matches(['\\', '/']);
    let name = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    let stem = name.strip_suffix(".zip").or_else(|| name.strip_suffix(".dll")).unwrap_or(name);
    let b = stem.as_bytes();
    let starts = (0..b.len())
        .filter(|&i| b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'v'));
    for i in starts {
        let num = version_numbers(&stem[i..]);
        if !num.contains('.') {
            continue;
        }
        let rest = &stem[i + num.len()..];
        let tag = ["-beta", "-alpha", "-rc"].into_iter().find(|t| rest.to_ascii_lowercase().starts_with(t));
        return Some(match tag {
            Some(t) => format!("{num}{t}"),
            None => num.to_string(),
        });
    }
    None
}

/// The notes about the zip's files that stay out of the game folder, and what dllchain checked in it.
fn source_notes(w: &mut Work, src: &ModSource) {
    w.plan.notes.extend(src.notes.iter().cloned());
    for i in &src.ignored {
        w.plan.notes.push(format!("{i} is outside the mod's folder in the zip, so it is not installed"));
    }
    if !src.extras.is_empty() {
        let k = src.extras.len();
        let list = match k {
            1..=4 => src.extras.join(", "),
            _ => format!("{} and {} more", src.extras[..3].join(", "), k - 3),
        };
        w.plan.notes.push(format!(
            "Licence, checksum and read-me files are for you, not the game, so they stay out of the game folder: {list}"
        ));
    }
}

/// Installs (or updates) a mod into a package host's folder, `mods\<word>\`, where that loader runs it: the mod's
/// files as its zip has them, without the off switch (`mod.json`), which would keep the folder switched off. The host
/// itself is never changed. A mod an older dllchain chained (the host renamed) moves into the folder, and the host
/// becomes `dinput8.dll` again.
fn plan_install_package(
    ctx: &Ctx,
    scan: &Scan,
    src: &ModSource,
    info: &DllInfo,
    word: &str,
    ph: &'static PackageHost,
    action: String,
) -> Result<Plan> {
    let mut w = Work::new(ctx, scan, action)?;
    let host = ctx.game.display(ph.word);
    let (decl, off) = (RelPath::of(ph.declaration), RelPath::of(ph.off_switch));
    let prev = w.packages.iter().position(|m| m.word == word);
    let name = match prev {
        Some(i) => name_of(ctx.game, &w.packages[i]),
        None => default_name(info).unwrap_or_else(|| w.display(word)),
    };
    let dll_only = src.files.is_empty();
    let shipped_decl = src.files.iter().find(|(r, _)| r.same(&decl));
    if let Some((_, b)) = shipped_decl {
        check_declaration(b, ctx.game.host_dll).map_err(|e| Error(format!("{}: {e}", src.label)))?;
    }
    // A mod made for the game folder (no declaration of its own): its files go where its zip puts them, as without
    // the host, and its DLL into a folder of its own with a declaration dllchain writes. A new DLL alone keeps the
    // layout the mod has.
    let game_layout = shipped_decl.is_none() && !(dll_only && prev.is_some());
    // the zip's own mods\<word>\ (a script package with mod.json) is not the DLL's folder
    let own_dir = package_dir(ph, word);
    let dir = if game_layout && src.files.iter().any(|(r, _)| r.strip_prefix(&own_dir).is_some()) {
        native_dir(ph, word)
    } else {
        prev.and_then(|i| w.packages[i].dll_at.parent()).unwrap_or(own_dir)
    };
    let dll_rel = dir.join(ctx.game.host_dll)?;
    // a loader that stops the game at a folder in mods\ without mod.json (EML), other than the package host's own
    for p in ctx.game.proxy_names {
        let rel = RelPath::of(&format!("{p}.dll"));
        let Some(b) = ctx.read(&rel) else { continue };
        if let Some(r) = strict_loader(ctx.game, ph, &b) {
            fail!(
                "{rel} is {}, not {host}'s own. {} stops the game at a folder in {}\\ without {off}, and {host} keeps a folder with {off} in it switched off, so {name} cannot run with both. Put {host}'s own {rel} back (install {host} again), then install {name}.",
                r.short,
                r.short,
                ph.dir
            );
        }
    }

    let chained = w.mods.iter().position(|m| m.word == word);
    let mut adopted_back = None;
    let mut rec = match (prev, chained) {
        (Some(i), _) => w.packages.remove(i),
        (None, Some(i)) => {
            let old = w.mods.remove(i);
            if let Some(at) = locate(ctx, scan, &old) {
                if old.kind == Kind::Adopted {
                    // not dllchain's: kept, and Uninstall puts it back next to the game (under its own name, as
                    // dinput8.dll is the host's)
                    let rel =
                        if at.same(&ctx.host()) { RelPath::parse(&format!("{}.dll", old.word))? } else { at.clone() };
                    // its bytes, checked against its SHA-256 when dllchain read the folder
                    let Some(bytes) = w.dlls.get(&old.word).cloned() else { fail!("could not read {at}") };
                    let backup = backup_of(&old.word, &rel)?;
                    w.plan.steps.push(Step::Write(backup.clone(), bytes));
                    w.plan.expect_files.push((backup.clone(), Some(old.dll_sha.clone())));
                    w.plan.summary.push(format!(
                        "Your copy of {name} that was {at} before dllchain is kept, and Uninstall puts it back as {rel}."
                    ));
                    adopted_back = Some(FileRec { rel, sha: old.dll_sha.clone(), backup: Some(backup) });
                }
                w.drop.push((at, old.dll_sha.clone(), old.word.clone()));
            }
            w.plan.summary.push(format!(
                "{name} moves from the game folder into {dir}\\, where {host} loads it ({host} loads no DLL next to the game)."
            ));
            if w.mods.iter().any(|m| m.word == ph.word && !m.dll_at.same(&ctx.host())) {
                w.plan.summary.push(format!("{host} becomes {} again, as it needs to be.", ctx.game.host_dll));
            }
            // the player's own files of the mod (marks, settings, switches) stay where they are, untouched
            let prefix = format!("{}-", old.word);
            let mut mine: Vec<String> = std::fs::read_dir(&ctx.dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.to_ascii_lowercase().starts_with(&prefix))
                .filter(|n| !old.files.iter().any(|f| f.rel.as_str().eq_ignore_ascii_case(n)))
                .collect();
            mine.sort();
            if !mine.is_empty() {
                w.plan.notes.push(format!(
                    "Your own {name} files next to the game stay there, as dllchain never moves them: {}. {name} now reads its files from {dir}\\, so move them there yourself to keep using them.",
                    mine.join(", ")
                ));
            }
            if old.replaced_dll.is_some() {
                w.plan.notes.push(format!(
                    "Uninstall no longer puts your earlier copy of {name} back as {}: {host} needs that name.",
                    ctx.game.host_dll
                ));
            }
            // a mod that was there before dllchain is dllchain's from now on: Uninstall takes the new files out
            ModRec { kind: Kind::Installed, replaced_dll: None, ..old }
        }
        (None, None) => ModRec::new(word.to_string(), Kind::Installed, w.next_order(), dll_rel.clone()),
    };
    let updating = prev.is_some();
    if updating {
        if rec.dll_sha == info.sha256 {
            w.plan.notes.push(format!("{name} is already installed, so dllchain only checks its files"));
        } else {
            w.plan.summary.push(format!("{name} will be updated to the new version, in {dir}\\."));
        }
    } else if chained.is_none() {
        w.plan.summary.push(format!("{name} will be added to {dir}\\, where {host} loads it."));
    }
    fill_source(&mut rec, src, info);
    rec.dll_at = dll_rel.clone();
    rec.retired.extend(adopted_back);

    let mut files = vec![(dll_rel, src.dll.clone())];
    if game_layout {
        files.extend(src.files.iter().cloned());
        files.push((dir.join(ph.declaration)?, declaration(ctx.game.host_dll)));
        w.plan.notes.push(format!(
            "{name} has no {decl}, so dllchain writes one in {dir}\\: it tells {host} to load {name}. Uninstall takes it out."
        ));
        // a script package in the zip (mods\x\ with mod.json) is one of the host's packages, off until enabled
        let scripts: Vec<String> = src
            .files
            .iter()
            .filter(|(r, _)| r.file_name().eq_ignore_ascii_case(ph.off_switch))
            .filter_map(|(r, _)| r.parent().filter(|p| p.first_part().eq_ignore_ascii_case(ph.dir)))
            .map(|p| format!("{p}\\"))
            .collect();
        if !scripts.is_empty() {
            w.plan.notes.push(format!(
                "{host} keeps {} switched off until you enable it in {host}'s window: {name} needs it on to work fully.",
                scripts.join(", ")
            ));
        }
    } else {
        for (r, b) in &src.files {
            if !r.same(&off) {
                files.push((dir.join(r.as_str())?, b.clone()));
            }
        }
    }
    retire(&mut w, &mut rec, (!game_layout).then_some(&dir), &files, Some((&dir.join(ph.off_switch)?, &host)))?;
    let gone = if chained.is_some() { "it had next to the game" } else { NOT_IN_NEW };
    plan_files(&mut w, &mut rec, &files, &src.label, dll_only, &|r| r.strip_prefix(&dir).is_some(), gone)?;
    w.packages.push(rec);
    w.layout()?;
    w.forget_lone_host();
    if !game_layout && src.files.iter().any(|(r, _)| r.same(&off)) {
        w.plan.notes.push(format!(
            "{off} from the zip is left out: with it, {host} keeps the folder switched off until you enable it."
        ));
    }
    w.plan.notes.push(format!("{host} stays {} and dllchain does not change it.", ctx.game.host_dll));
    source_notes(&mut w, src);
    let rels: Vec<&RelPath> = files.iter().map(|(r, _)| r).collect();
    w.plan.missing = missing(ctx, scan, &rels);
    w.plan.name = name;
    Ok(w.finish())
}

/// Installs (or updates) a mod that brings its own loader: its DLL keeps its Windows name (`dbgcore.dll`), which the
/// game loads by itself, so it never joins the chain. Its files go where its zip puts them.
///
/// The loader can be one that other mods use too (Greeneye's loader runs every Greeneye mod). The same file already
/// there is shared: the mod that installed it keeps it on its list, and when that mod goes, the loader stays for the
/// others. Another build of the same loader (the same tool, by its DLL) is updated when the zip's is newer, with a
/// copy kept, and kept when it is the same or newer. Anything else under that name is another tool's, and the install
/// is refused.
fn plan_install_own_loader(ctx: &Ctx, scan: &Scan, src: &ModSource, info: &DllInfo, action: String) -> Result<Plan> {
    let game = ctx.game;
    let about = src.about.clone().unwrap_or_default();
    // the DLL names the mod only when its own version information does; a known loader's name is the loader's
    let dll_names_mod = matches!(info.word, Some((_, WordFrom::VersionInfo)));
    let word = about
        .id
        .as_deref()
        .map(crate::paths::slug)
        .filter(|w| is_word(w))
        .or_else(|| info.word.as_ref().filter(|_| dll_names_mod).map(|w| w.0.clone()))
        .unwrap_or_else(|| src.word_hint.clone());
    if !is_word(&word) {
        fail!("{}: could not make a file name from it", src.label);
    }
    let loader = RelPath::parse(&src.dll_name)?;
    let mut w = Work::new(ctx, scan, action)?;
    if let Some(m) = w.mods.iter().chain(&w.packages).find(|m| m.word == word) {
        fail!(
            "{} is on dllchain's list as a mod that ships as {}, and {} brings its own loader ({loader}). Uninstall it first if this is the same mod.",
            name_of(game, m),
            game.host_dll,
            src.label
        );
    }
    let prev = w.loaders.iter().position(|m| m.word == word);
    let name =
        about.name.as_deref().and_then(clean_name).or_else(|| dll_names_mod.then(|| default_name(info)).flatten());
    let display =
        name.clone().or_else(|| prev.and_then(|i| w.loaders[i].name.clone())).unwrap_or_else(|| game.display(&word));
    if let Some(ph) = package_host(ctx, scan) {
        if let Some(r) = strict_loader(game, ph, &src.dll) {
            let host = game.display(ph.word);
            fail!(
                "{display}'s {loader} is {}, which stops the game at a folder in {}\\ without {}, and {host} needs folders like that, so {display} cannot run with {host}.",
                r.short,
                ph.dir,
                ph.off_switch
            );
        }
    }
    let mut rec = match prev {
        Some(i) => w.loaders.remove(i),
        None => ModRec::new(word.clone(), Kind::Installed, w.next_order(), loader.clone()),
    };
    if !rec.dll_at.same(&loader) {
        fail!(
            "{display} used {} before and now brings {loader}. Uninstall it first, then install the new version.",
            rec.dll_at
        );
    }
    let before = w.plan.summary.len();
    let steps_before = w.plan.steps.len();
    let old_sha = rec.dll_sha.clone();

    // the loader: this mod's entry for it, or the other mod's that installed it
    let mine = take_listed(&mut rec, &loader);
    let holder = w.loaders.iter().position(|m| lists(m, &loader));
    let holder_name = holder.map(|h| name_of(game, &w.loaders[h]));
    let new_sha = sha256_hex(&src.dll);
    let family = |i: &DllInfo| {
        i.word.as_ref().map_or_else(|| loader.to_string(), |w| default_name(i).unwrap_or_else(|| game.display(&w.0)))
    };
    // the zip's loader in its place, listed by this mod, and every mod on it told
    let write_loader = |w: &mut Work, backup: Option<RelPath>| {
        w.plan.steps.push(Step::Write(loader.clone(), src.dll.clone()));
        w.plan.expect_files.push((loader.clone(), Some(new_sha.clone())));
        for m in w.loaders.iter_mut().filter(|m| m.dll_at.same(&loader)) {
            m.dll_sha = new_sha.clone();
        }
        Some((FileRec { rel: loader.clone(), sha: new_sha.clone(), backup }, false))
    };
    // the loader there stays: this mod's entry for it, shared with the mod that lists it, or found
    let keep_there = |w: &mut Work, mine: Option<(FileRec, bool)>, sha: &str, say_found: bool| match (
        mine,
        &holder_name,
    ) {
        (Some(m), _) => Some(m),
        (None, Some(h)) => {
            w.plan.summary.push(format!("Shares {loader}, its loader, with {h}."));
            None
        }
        (None, None) => {
            if say_found {
                w.plan.summary.push(format!(
                    "Finds {loader}, its loader, already in your game, the same as in the zip: dllchain lists it, and Uninstall leaves it."
                ));
            }
            Some((FileRec { rel: loader.clone(), sha: sha.to_string(), backup: None }, true))
        }
    };
    let (keep, dll_sha) = match ctx.read(&loader).map(|b| (sha256_hex(&b), b)) {
        None => {
            // gone (or never there): written, and listed by whoever had it
            let had = mine.or_else(|| holder.and_then(|h| take_listed(&mut w.loaders[h], &loader)));
            w.plan.summary.push(format!(
                "Adds {loader}, the loader {display} brings: the game loads it by itself, not through {}.",
                game.chain_file
            ));
            (write_loader(&mut w, had.and_then(|(f, _)| f.backup)), new_sha.clone())
        }
        Some((now_sha, _)) if now_sha == new_sha => (keep_there(&mut w, mine, &now_sha, true), now_sha),
        Some((now_sha, now)) => {
            // dllchain put it there for this mod alone: the zip's is the one it runs on, whatever its version
            let only_mine = matches!(&mine, Some((f, false)) if f.sha == now_sha)
                && !w.loaders.iter().any(|o| o.dll_at.same(&loader));
            let theirs = inspect(&now, game).map_err(|e| Error(format!("{loader} in the game folder is {e}")))?;
            let same_tool = matches!((&theirs.word, &info.word), (Some(a), Some(b)) if a.0 == b.0);
            if !same_tool && !only_mine {
                fail!(
                    "{loader} in your game is {}, not the loader {display} brings ({}). Only one {loader} can be next to the game, so dllchain changes nothing. If you no longer use what put it there, remove it first.",
                    theirs.product_name.clone().unwrap_or_else(|| family(&theirs)),
                    family(info)
                );
            }
            let tool = family(info);
            let (v_new, v_now) = (info.version.clone().unwrap_or_default(), theirs.version.clone().unwrap_or_default());
            if only_mine || newer_version(&v_new, &v_now) {
                let had = mine.or_else(|| holder.and_then(|h| take_listed(&mut w.loaders[h], &loader)));
                let backup = match had {
                    // dllchain's own copy: what was there before it stays the one Uninstall puts back
                    Some((f, false)) if f.sha == now_sha => f.backup,
                    Some((f, _)) if f.sha != now_sha => fail!(
                        "{loader} changed since dllchain listed it, so dllchain does not replace it. Put back the {tool} {display} needs, or uninstall {} first.",
                        holder_name.clone().unwrap_or_else(|| display.clone())
                    ),
                    // there before dllchain: kept, and Uninstall puts it back
                    _ => {
                        let b = backup_of(&word, &loader)?;
                        if ctx.exists(&b) {
                            fail!("dllchain already keeps another copy of {loader} in {b}, so it does not replace it");
                        }
                        w.plan.steps.push(Step::Write(b.clone(), now));
                        w.plan.expect_files.push((b.clone(), Some(now_sha)));
                        Some(b)
                    }
                };
                w.plan.summary.push(if only_mine {
                    format!("Updates {loader} to the one in the zip.")
                } else {
                    format!(
                        "Updates {loader}, the {tool} in your game, from {v_now} to {v_new}: {display} needs the newer one."
                    )
                });
                if backup.is_some() {
                    w.plan.summary.push("dllchain keeps the older one, and Uninstall puts it back.".into());
                }
                (write_loader(&mut w, backup), new_sha.clone())
            } else {
                w.plan.notes.push(format!(
                    "{loader} in your game is {tool} {v_now}, the same or newer than the {v_new} {display} brings, so dllchain keeps it."
                ));
                (keep_there(&mut w, mine, &now_sha, false), now_sha)
            }
        }
    };

    rec.at = clock::now_text();
    rec.source_sha = src.sha256.clone();
    rec.label = src.label.clone();
    rec.from_url = src.from_url.clone();
    if name.is_some() {
        rec.name = name;
    }
    rec.dll_sha = dll_sha;
    rec.version = about
        .version
        .as_deref()
        .and_then(clean_name)
        .or_else(|| info.version.as_deref().filter(|_| dll_names_mod).and_then(clean_name))
        .or_else(|| version_in(&src.label));
    // its own folders' files, and those an earlier copy put in by hand are this mod's
    plan_files(&mut w, &mut rec, &src.files, &src.label, src.files.is_empty(), &|r| !r.is_top_level(), NOT_IN_NEW)?;
    match keep {
        Some((f, true)) => rec.found.insert(0, f),
        Some((f, false)) => rec.files.insert(0, f),
        None => {}
    }
    let changes = w.plan.steps[steps_before..].iter().any(|s| !matches!(s, Step::Write(r, _) if is_own(r)));
    let first = match prev {
        Some(_) if !changes && old_sha == rec.dll_sha => {
            w.plan.notes.push(format!("{display} is already installed, so dllchain only checks its files"));
            None
        }
        Some(_) => Some(format!("{display} will be updated to the new version.")),
        // its own files there already, not only a loader another mod left
        None if rec.found.iter().any(|f| !f.rel.same(&loader) || src.files.is_empty()) => {
            Some(format!("{display} is already in your game, put there by hand: dllchain lists it from now on."))
        }
        None => Some(format!("{display} will be added to your game.")),
    };
    if let Some(l) = first {
        w.plan.summary.insert(before, l);
    }
    // back in its place, so the record stays as it was when nothing changed
    w.loaders.insert(prev.unwrap_or(w.loaders.len()), rec);
    w.layout()?;
    source_notes(&mut w, src);
    let files: Vec<&RelPath> = src.files.iter().map(|(r, _)| r).collect();
    w.plan.missing = missing(ctx, scan, &files);
    w.plan.name = display;
    Ok(w.finish())
}

/// Whether version `a` is newer than `b`, by their numbers (`1.2.10` is newer than `1.2.9`); never when either has
/// none.
fn newer_version(a: &str, b: &str) -> bool {
    let nums = |v: &str| -> Vec<u64> { version_numbers(v).split('.').filter_map(|p| p.parse().ok()).collect() };
    let (a, b) = (nums(a), nums(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let n = a.len().max(b.len());
    let pad = |v: &[u64]| (0..n).map(|i| v.get(i).copied().unwrap_or(0)).collect::<Vec<_>>();
    pad(&a) > pad(&b)
}

/// A package host's declaration (`native-plugin.ini`) must load the DLL dllchain puts there, and be switched on.
fn check_declaration(bytes: &[u8], dll: &str) -> Result<()> {
    let text = String::from_utf8_lossy(bytes);
    let value = |key: &str| {
        text.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            k.trim().eq_ignore_ascii_case(key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    match value("Dll") {
        Some(d) if d.trim_start_matches("./").trim_start_matches(".\\").eq_ignore_ascii_case(dll) => {}
        Some(d) => fail!(
            "its native-plugin.ini loads {d}, not the {dll} it ships, so dllchain cannot tell which DLL is the mod"
        ),
        None => fail!("its native-plugin.ini names no Dll"),
    }
    if value("Enabled").is_some_and(|e| e == "0" || e.eq_ignore_ascii_case("false")) {
        fail!("its native-plugin.ini switches it off (Enabled=0)");
    }
    Ok(())
}

/// Takes out what an older version or a hand install left in the mod's folder (`None`: next to the game): the files
/// the game's tool table says a newer version replaced, and a package host's off switch (with the host's name). Each
/// is kept in dllchain's originals, and Uninstall puts it back. Files the mod's earlier install made are not these:
/// [`plan_files`] takes those away.
fn retire(
    w: &mut Work,
    rec: &mut ModRec,
    folder: Option<&RelPath>,
    shipped: &[(RelPath, Vec<u8>)],
    off: Option<(&RelPath, &str)>,
) -> Result<()> {
    let ctx = w.ctx;
    let at = |name: &str| match folder {
        Some(d) => d.join(name),
        None => RelPath::parse(name),
    };
    let ships = |r: &RelPath| shipped.iter().any(|(s, _)| s.same(r));
    let mut out: Vec<(RelPath, String)> = Vec::new();
    for l in ctx.game.tool(&rec.word).map_or(&[][..], |t| t.leftovers) {
        let (old, new) = (at(l.old)?, at(l.new)?);
        if ships(&new) && !ships(&old) {
            out.push((old, format!("which an older version left (the new one uses {})", l.new)));
        }
    }
    if let Some((o, host)) = off {
        if !ships(o) {
            out.push((
                o.clone(),
                format!("so {host} loads the mod (with it there, {host} keeps the folder switched off)"),
            ));
        }
    }
    for (rel, why) in out {
        if rec.files.iter().chain(&rec.retired).any(|f| f.rel.same(&rel)) {
            continue;
        }
        let Some(bytes) = ctx.read(&rel) else { continue };
        let backup = backup_of(&rec.word, &rel)?;
        if ctx.exists(&backup) {
            w.plan.notes.push(format!("{rel} stays: dllchain already keeps another copy of it in {backup}"));
            continue;
        }
        let sha = sha256_hex(&bytes);
        w.plan.steps.push(Step::Write(backup.clone(), bytes));
        w.plan.steps.push(Step::Delete(rel.clone()));
        w.plan.expect_files.push((backup.clone(), Some(sha.clone())));
        w.plan.expect_files.push((rel.clone(), None));
        w.plan.summary.push(format!("Takes out {rel}, {why}. dllchain keeps it, and Uninstall puts it back."));
        rec.retired.push(FileRec { rel, sha, backup: Some(backup) });
    }
    Ok(())
}

/// Why an update deletes the files of the earlier version that the new one does not ship.
const NOT_IN_NEW: &str = "the new version does not have";

/// The mod's other files: created, updated, kept when the player changed them, and the old version's leftovers.
/// `earlier(rel)`: a file there now that dllchain did not install is the same mod's, from a copy put in by hand (it is
/// kept and put back on uninstall like any file replaced, but asks for no leave to replace). `gone` says why the earlier
/// install's files that are not in `src_files` are deleted ("the new version does not have").
fn plan_files(
    w: &mut Work,
    rec: &mut ModRec,
    src_files: &[(RelPath, Vec<u8>)],
    label: &str,
    dll_only: bool,
    earlier: &dyn Fn(&RelPath) -> bool,
    gone: &str,
) -> Result<()> {
    let ctx = w.ctx;
    let prev_files = rec.files.clone();
    let prev_found = std::mem::take(&mut rec.found);
    let mut files: Vec<FileRec> = Vec::new();
    // the files that were in the game already, the same as in the zip, when dllchain listed the mod
    let mut found: Vec<FileRec> = Vec::new();
    let mut dirs = rec.dirs.clone();
    let mut by_hand: Vec<RelPath> = Vec::new();
    let taken: HashSet<String> = w
        .all()
        .filter(|m| m.word != rec.word)
        .flat_map(|m| m.files.iter().chain(&m.found).map(|f| f.rel.key()).chain([m.dll_at.key()]))
        .chain([ctx.host().key()])
        .collect();
    let prev_found_keys: HashSet<String> = prev_found.iter().map(|f| f.rel.key()).collect();
    let was_found = |rel: &RelPath| prev_found_keys.contains(&rel.key());
    for (rel, bytes) in src_files {
        check_target(rel, ctx.game)?;
        if taken.contains(&rel.key()) {
            fail!("{label}: {rel} belongs to another mod on dllchain's list");
        }
        resolve(&ctx.dir, rel)?;
        let sha = sha256_hex(bytes);
        let now = ctx.sha(rel);
        if ctx.exists(rel) && now.is_none() {
            fail!("{label}: {rel} is a folder in the game folder, not a file");
        }
        let prev = prev_files.iter().find(|f| f.rel.same(rel));
        for d in rel.ancestors() {
            if !ctx.exists(&d) && !dirs.iter().any(|x| x.same(&d)) {
                dirs.push(d);
            }
        }
        match (now, prev) {
            (Some(n), p) if n == sha => match p {
                Some(p) => files.push(FileRec { rel: rel.clone(), sha, backup: p.backup.clone() }),
                None => found.push(FileRec { rel: rel.clone(), sha, backup: None }),
            },
            (None, p) => {
                w.plan.steps.push(Step::Write(rel.clone(), bytes.clone()));
                w.plan.expect_files.push((rel.clone(), Some(sha.clone())));
                files.push(FileRec { rel: rel.clone(), sha, backup: p.and_then(|p| p.backup.clone()) });
            }
            (Some(n), Some(p)) if n == p.sha => {
                w.plan.steps.push(Step::Write(rel.clone(), bytes.clone()));
                w.plan.expect_files.push((rel.clone(), Some(sha.clone())));
                files.push(FileRec { rel: rel.clone(), sha, backup: p.backup.clone() });
            }
            (Some(_), Some(p)) => {
                // the player changed a file the earlier version installed (a settings file): theirs stays
                let new = RelPath::parse(&format!("{rel}.new"))?;
                w.plan.steps.push(Step::Write(new.clone(), bytes.clone()));
                w.plan.expect_files.push((new.clone(), Some(sha.clone())));
                w.plan.notes.push(format!("You changed {rel}, so yours stays. The new version is saved as {new}"));
                files.push(p.clone());
                files.push(FileRec { rel: new, sha, backup: None });
            }
            (Some(n), None) => {
                let backup = backup_of(&rec.word, rel)?;
                let Some(old) = ctx.read(rel) else { fail!("could not read {rel}") };
                debug_assert_eq!(sha256_hex(&old), n);
                if earlier(rel) || was_found(rel) {
                    by_hand.push(rel.clone());
                } else {
                    w.plan.replaces.push(rel.clone());
                }
                w.plan.steps.push(Step::Write(backup.clone(), old));
                w.plan.expect_files.push((backup.clone(), Some(n)));
                w.plan.steps.push(Step::Write(rel.clone(), bytes.clone()));
                w.plan.expect_files.push((rel.clone(), Some(sha.clone())));
                files.push(FileRec { rel: rel.clone(), sha, backup: Some(backup) });
            }
        }
    }
    let new_count = files.len();
    let mut undone = Undone::default();
    // Only a version that ships the mod's files can leave one out. A new DLL on its own (a fix, often downloaded
    // without the rest) keeps the files the earlier version installed; so does a licence or read-me the earlier
    // version put next to the game, which dllchain no longer installs.
    let mut carried: Vec<&RelPath> = Vec::new();
    for p in &prev_files {
        if files.iter().any(|f| f.rel.same(&p.rel)) {
            continue;
        }
        if dll_only || release_extra(&p.rel) {
            files.push(p.clone());
            carried.push(&p.rel);
            continue;
        }
        undone.add(undo_file(w, p, &mut files), p);
    }
    if dll_only {
        // the earlier version's files that were there before it stay listed as such
        for p in &prev_found {
            if !files.iter().chain(&found).any(|f| f.rel.same(&p.rel)) {
                found.push(p.clone());
            }
        }
        match carried.len() {
            0 => {}
            1 => w.plan.summary.push(format!("Keeps {} from the earlier version.", carried[0])),
            k => {
                w.plan.summary.push(format!("Keeps the other {k} files from the earlier version{}.", most_in(&carried)))
            }
        }
    }
    let created: Vec<&RelPath> = files[..new_count.min(files.len())]
        .iter()
        .filter(|f| {
            !prev_files.iter().any(|p| p.rel.same(&f.rel))
                && !w.plan.replaces.iter().any(|r| r.same(&f.rel))
                && !by_hand.iter().any(|r| r.same(&f.rel))
        })
        .map(|f| &f.rel)
        .collect();
    match created.len() {
        0 => {}
        1 => w.plan.summary.push(format!("Adds 1 more file: {}.", created[0])),
        k => w.plan.summary.push(format!("Adds {k} more files{}.", most_in(&created))),
    }
    let newly_found: Vec<&RelPath> = found.iter().map(|f| &f.rel).filter(|r| !was_found(r)).collect();
    match newly_found[..] {
        [] => {}
        [f] => w.plan.summary.push(format!(
            "Finds {f} already in your game, the same as in the zip: dllchain lists it, and Uninstall leaves it."
        )),
        _ => w.plan.summary.push(format!(
            "Finds {} of its files already in your game, the same as in the zip{}: dllchain lists them, and Uninstall leaves them.",
            newly_found.len(),
            most_in(&newly_found)
        )),
    }
    match by_hand.len() {
        0 => {}
        1 => w
            .plan
            .summary
            .push(format!("Updates {}, which you put there by hand. dllchain keeps that copy.", by_hand[0])),
        k => w.plan.summary.push(format!(
            "Updates {k} files you put there by hand{}. dllchain keeps those copies.",
            most_in(&by_hand.iter().collect::<Vec<_>>())
        )),
    }
    let replaced: Vec<&RelPath> = w.plan.replaces.iter().collect();
    match replaced.len() {
        0 => {}
        1 => {
            w.plan.summary.push(format!("Replaces {}, which was already there.", replaced[0]));
            w.plan.summary.push("dllchain keeps the old one, and Uninstall puts it back.".into());
        }
        k => {
            w.plan.summary.push(format!("Replaces {k} files that were already there{}.", most_in(&replaced)));
            w.plan.summary.push("dllchain keeps the old ones, and Uninstall puts them back.".into());
        }
    }
    let deleted: Vec<&RelPath> = undone.deleted.iter().collect();
    match deleted.len() {
        0 => {}
        1 => w.plan.summary.push(format!("Deletes {}, which {gone}.", deleted[0])),
        k => w.plan.summary.push(format!("Deletes {k} files {gone}{}.", most_in(&deleted))),
    }
    undone.words(w);
    rec.files = files;
    rec.found = found;
    dirs.sort_by_key(|d| d.key());
    dirs.dedup_by(|a, b| a.same(b));
    // the folders dllchain made for the earlier version's files, which the new one has none in
    let (used, gone): (Vec<RelPath>, Vec<RelPath>) =
        dirs.into_iter().partition(|d| rec.files.iter().any(|f| f.rel.strip_prefix(d).is_some()));
    for d in gone.into_iter().rev() {
        w.plan.steps.push(Step::RemoveDirIfEmpty(d));
    }
    rec.dirs = used;
    Ok(())
}

/// What `undo_file` did with one file.
enum Undo {
    Deleted,
    Restored,
    /// The player changed it, so it stays; with whether dllchain still has the original.
    Kept(bool),
    /// Already gone.
    Gone,
}

/// The files a mod takes away, counted, so a mod with hundreds of files still gets a few lines.
#[derive(Default)]
struct Undone {
    deleted: Vec<RelPath>,
    restored: Vec<RelPath>,
    kept: Vec<RelPath>,
    kept_originals: bool,
}

impl Undone {
    fn add(&mut self, u: Undo, f: &FileRec) {
        match u {
            Undo::Deleted => self.deleted.push(f.rel.clone()),
            Undo::Restored => self.restored.push(f.rel.clone()),
            Undo::Kept(original) => {
                self.kept.push(f.rel.clone());
                self.kept_originals |= original;
            }
            Undo::Gone => {}
        }
    }

    /// The lines for the files put back and kept; the caller words the deleted ones.
    fn words(&self, w: &mut Work) {
        let restored: Vec<&RelPath> = self.restored.iter().collect();
        match restored.len() {
            0 => {}
            1 => w.plan.summary.push(format!("Puts back {}, which was there before.", restored[0])),
            k => w.plan.summary.push(format!("Puts back {k} files that were there before{}.", most_in(&restored))),
        }
        let originals =
            if self.kept_originals { ". dllchain keeps the originals in dllchain\\originals\\" } else { "" };
        match self.kept.len() {
            0 => {}
            1 => w.plan.notes.push(format!("You changed {}, so it stays{originals}", self.kept[0])),
            k => w.plan.notes.push(format!("You changed {k} of these files, so they stay{originals}")),
        }
    }
}

/// Takes one of a mod's files away again: deleted while it is as dllchain left it, or the original put back; a file
/// the player changed stays (and stays on the list when `keep_changed` is given).
fn undo_file(w: &mut Work, f: &FileRec, keep_changed: &mut Vec<FileRec>) -> Undo {
    let ctx = w.ctx;
    let now = ctx.sha(&f.rel);
    let original = f.backup.as_ref().and_then(|b| ctx.read(b).map(|bytes| (b.clone(), bytes)));
    match (now.as_deref() == Some(f.sha.as_str()) || now.is_none(), original) {
        (true, Some((b, bytes))) => {
            w.plan.expect_files.push((f.rel.clone(), Some(sha256_hex(&bytes))));
            w.plan.steps.push(Step::Write(f.rel.clone(), bytes));
            w.plan.steps.push(Step::Delete(b));
            Undo::Restored
        }
        (true, None) if now.is_some() => {
            w.plan.steps.push(Step::Delete(f.rel.clone()));
            w.plan.expect_files.push((f.rel.clone(), None));
            Undo::Deleted
        }
        (true, None) => Undo::Gone,
        (false, original) => {
            if f.backup.is_some() {
                keep_changed.push(f.clone());
            }
            Undo::Kept(original.is_some())
        }
    }
}

/// The folders files go into, the fullest first: `mods\auto-loot-plus\` (two levels at most), with how many.
/// Files next to the game count as the game folder itself, which has no name here.
fn places(rels: &[&RelPath]) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    for r in rels {
        let parts: Vec<&str> = r.as_str().split('\\').collect();
        let place = parts[..parts.len() - 1].iter().take(2).fold(String::new(), |s, p| format!("{s}{p}\\"));
        match out.iter_mut().find(|(p, _)| p.eq_ignore_ascii_case(&place)) {
            Some((_, n)) => *n += 1,
            None => out.push((place, 1)),
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.1));
    out
}

/// ", most in mods\auto-loot-plus\" for a list of files, ", all in AutoDoors\" when they share only their top
/// folder, or nothing when they are spread out.
fn most_in(rels: &[&RelPath]) -> String {
    let in_top = |t: &str| rels.iter().all(|r| !r.is_top_level() && r.first_part().eq_ignore_ascii_case(t));
    match places(rels).first() {
        Some((p, n)) if !p.is_empty() && *n == rels.len() => format!(", all in {p}"),
        Some((p, n)) if !p.is_empty() && *n * 2 > rels.len() => format!(", most in {p}"),
        _ => match rels.first().map(|r| r.first_part()) {
            Some(t) if in_top(t) => format!(", all in {t}\\"),
            _ => String::new(),
        },
    }
}

/// Puts the chain right again: another mod copied over `dinput8.dll`, a missing guest, a missing chain line, or a
/// DLL another tool renamed.
pub fn plan_repair(ctx: &Ctx) -> Result<Plan> {
    let scan = scan(ctx)?;
    let mut w = Work::new(ctx, &scan, "repair".into())?;
    w.plan.first_install = false;
    if scan.record.is_empty() {
        // dllchain has installed nothing here, so there is nothing of its own to put right; a mod already there
        // joins the chain when the first mod is installed
        return Ok(w.plan);
    }
    w.adopt_host(None);
    w.layout()?;
    if let Some(ph) = package_host(ctx, &scan) {
        let host = ctx.game.display(ph.word);
        for m in w.mods.iter().filter(|m| !m.dll_at.same(&ctx.host())) {
            w.plan.notes.push(format!(
                "{host} loads no DLL next to the game, so {} ({}) will not run. Install it again to move it into {}.",
                name_of(ctx.game, m),
                m.dll_at,
                package_dir(ph, &m.word)
            ));
        }
    }
    let mut notes = Vec::new();
    for m in w.all() {
        // the files it lists, and those that were there already, which it needs as much
        let missing: Vec<&RelPath> =
            m.files.iter().chain(&m.found).map(|f| &f.rel).filter(|r| !ctx.exists(r)).collect();
        let name = name_of(ctx.game, m);
        match missing.len() {
            0 => {}
            1 => notes.push(format!("{} of {name} is missing. Install it again to put it back", missing[0])),
            k => notes.push(format!("{k} files of {name} are missing. Install it again to put them back")),
        }
    }
    w.plan.notes.extend(notes);
    Ok(w.finish())
}

/// Gives a mod on dllchain's list the name players see (`None` or an empty name: back to its own name). Only the
/// record changes; files keep their names.
pub fn plan_rename(ctx: &Ctx, word: &str, name: Option<&str>) -> Result<Plan> {
    let scan = scan(ctx)?;
    let mut record = scan.record.clone();
    let Some(m) = record.mods.iter_mut().find(|m| m.word == word) else {
        fail!("{word} is not on dllchain's list; dllchain names only the mods it installed or chained");
    };
    let old = name_of(ctx.game, m);
    let new = name.and_then(clean_name);
    m.name = match new {
        Some(n) => Some(n),
        None => {
            let dll = load_dll(ctx, &scan, m).and_then(|b| inspect(&b, ctx.game).ok());
            dll.as_ref().and_then(default_name)
        }
    };
    let now = name_of(ctx.game, m);
    let mut plan = Plan { action: format!("rename {word}"), notes: scan.notes.clone(), ..Plan::default() };
    if now == old {
        plan.notes.push(format!("{old} already has that name"));
        return Ok(plan);
    }
    plan.summary.push(format!("{old} is shown as {now} from now on. No file is renamed."));
    let rel = RelPath::of(RECORD);
    let text = record.to_text().into_bytes();
    plan.expect_files.push((rel.clone(), Some(sha256_hex(&text))));
    plan.steps.push(Step::Write(rel, text));
    plan.record = record;
    Ok(plan)
}

/// A run's action without the SHA-256 of the zip: "install x.zip".
pub fn short_action(action: &str) -> &str {
    action.split(" (SHA-256").next().unwrap_or(action)
}

/// Takes back the last change dllchain made (an install, an update, a repair, an uninstall of one mod), from the copies
/// that run kept. Only while every file it changed is still as it left it; undoing an undo makes the change again.
pub fn plan_undo(ctx: &Ctx) -> Result<Plan> {
    let scan = scan(ctx)?;
    let Some(run) = crate::txn::last_run(&ctx.dir)? else {
        fail!("there is no change to undo: dllchain keeps the last one only, and none was made since this version");
    };
    if let Some(l) = run.unreadable.first() {
        fail!("dllchain's note of the last change cannot be read in full ({l}), so it cannot be undone");
    }
    let when = clock::readable(&run.at);
    let action = match run.action.strip_prefix("undo ") {
        Some(a) => a.to_string(),
        None => format!("undo {}", run.action),
    };
    let mut plan = Plan { action, notes: scan.notes.clone(), ..Plan::default() };
    let mut restored = 0;
    let mut deleted = 0;
    // the record as it was before: `Some(None)` when the run made it
    let mut record: Option<Option<Vec<u8>>> = None;
    for c in &run.changes {
        if c.rel.same(&RelPath::of(RECORD)) && c.before.is_none() {
            record = Some(None);
        }
        if ctx.sha(&c.rel) != c.after {
            fail!(
                "{} changed after the last change ({}, {when}), so dllchain cannot take that change back without losing yours",
                c.rel,
                short_action(&run.action)
            );
        }
        match &c.before {
            Some((copy, sha)) => {
                let Some(bytes) = ctx.read_checked(copy, sha) else {
                    fail!("dllchain's copy of {} from before the last change is gone, so it cannot be undone", c.rel);
                };
                if c.after.as_deref() == Some(sha.as_str()) {
                    continue;
                }
                if c.rel.same(&RelPath::of(RECORD)) {
                    record = Some(Some(bytes.clone()));
                }
                plan.expect_files.push((c.rel.clone(), Some(sha.clone())));
                plan.steps.push(Step::Write(c.rel.clone(), bytes));
                restored += 1;
            }
            None => {
                if c.after.is_none() {
                    continue;
                }
                plan.expect_files.push((c.rel.clone(), None));
                plan.steps.push(Step::Delete(c.rel.clone()));
                deleted += 1;
            }
        }
        if !is_own(&c.rel) {
            let how = if c.before.is_some() { "put back" } else { "deleted" };
            plan.files.push(format!("{} ({how})", c.rel));
        }
    }
    for d in run.made_dirs.iter().rev() {
        plan.steps.push(Step::RemoveDirIfEmpty(d.clone()));
    }
    plan.record = match record {
        Some(Some(b)) => {
            let (r, warn) = Record::parse(&String::from_utf8_lossy(&b));
            if !warn.is_empty() {
                fail!("dllchain's record from before the last change cannot be read in full, so it cannot be undone");
            }
            r
        }
        Some(None) => Record::default(),
        None => scan.record.clone(),
    };
    plan.files.sort();
    plan.summary.push(format!("Takes back the last change: {} ({when}).", short_action(&run.action)));
    let count = |k: usize, what: &str| match k {
        0 => None,
        1 => Some(format!("{what} 1 file")),
        k => Some(format!("{what} {k} files")),
    };
    let parts: Vec<String> = [count(restored, "puts back"), count(deleted, "deletes")].into_iter().flatten().collect();
    if !parts.is_empty() {
        plan.summary.push(format!("It {}, so they are as they were before.", parts.join(" and ")));
    }
    plan.summary.push("You can take this back too: Undo again makes the change again.".into());
    Ok(plan)
}

/// The name players see: the one the player gave, else the DLL's product name, else the known tool's name or the
/// file-name word.
pub fn name_of(game: &Game, m: &ModRec) -> String {
    m.name.clone().unwrap_or_else(|| game.display(&m.word))
}

/// A DLL's own name, for one that is not a known tool: its product name.
fn default_name(info: &DllInfo) -> Option<String> {
    match &info.word {
        Some((_, WordFrom::Known)) => None,
        // a DLL built from a template can keep Windows' own product name, which says nothing
        _ => info.product_name.as_deref().filter(|p| !p.contains("Microsoft")).and_then(clean_name),
    }
}

/// Uninstall one mod (by its word) or, with `None`, everything dllchain installed. What the mods made while they ran
/// stays, and is listed in `made`.
pub fn plan_uninstall(ctx: &Ctx, word: Option<&str>) -> Result<Plan> {
    plan_uninstall_with(ctx, word, false)
}

/// The same, and with `delete_made`, it deletes what the mods made while they ran too (their logs, backups, reports).
pub fn plan_uninstall_with(ctx: &Ctx, word: Option<&str>, delete_made: bool) -> Result<Plan> {
    let scan = scan(ctx)?;
    let mut w = Work::new(ctx, &scan, format!("uninstall {}", word.unwrap_or("everything")))?;
    w.plan.first_install = false;
    w.adopt_host(None);
    let targets: Vec<String> = match word {
        Some(x) if w.packages.iter().chain(&w.loaders).any(|m| m.word == x) => vec![x.to_string()],
        Some(x) => match w.mods.iter().find(|m| m.word == x) {
            None => fail!("{x} is not on dllchain's list"),
            Some(m) if m.kind == Kind::Adopted => fail!(
                "dllchain did not install {}; it was already {} (or copied over later). dllchain keeps it in the chain and never deletes it.",
                ctx.game.display(x),
                ctx.game.host_dll
            ),
            Some(m) => vec![m.word.clone()],
        },
        None => w.all().filter(|m| m.kind == Kind::Installed).map(|m| m.word.clone()).collect(),
    };
    w.allow_no_host = word.is_none();
    for t in &targets {
        let m = if let Some(i) = w.packages.iter().position(|m| &m.word == t) {
            w.packages.remove(i)
        } else if let Some(i) = w.loaders.iter().position(|m| &m.word == t) {
            w.loaders.remove(i)
        } else {
            w.mods.remove(w.mods.iter().position(|m| &m.word == t).expect("listed"))
        };
        let display = name_of(ctx.game, &m);
        if word.is_some() {
            w.plan.name = display.clone();
        }
        // a package's DLL is one of its files, and so is a loader the mod brought
        let at = if is_chained(ctx.game, &m) { locate(ctx, &scan, &m) } else { None };
        let mut m = m;
        let loader_stays = if is_own_loader(ctx.game, &m) { leave_shared_loader(&mut w, &mut m) } else { None };
        let m = m;
        let mut kept = Vec::new();
        let mut undone = Undone::default();
        for f in m.files.iter().rev() {
            undone.add(undo_file(&mut w, f, &mut kept), f);
        }
        w.plan.summary.push(format!("{display} will be removed from your game."));
        put_back_retired(&mut w, &m);
        let mut deleted: Vec<&RelPath> = at.iter().collect();
        deleted.extend(&undone.deleted);
        match deleted.len() {
            0 => {}
            1 => w.plan.summary.push(format!("Deletes {}.", deleted[0])),
            k => w.plan.summary.push(format!("Deletes {k} files{}.", most_in(&deleted))),
        }
        w.plan.summary.extend(loader_stays);
        match m.found.len() {
            0 => {}
            1 => w.plan.summary.push(format!("Leaves {}, which was there before dllchain listed it.", m.found[0].rel)),
            k => w.plan.summary.push(format!("Leaves {k} files that were there before dllchain listed it.")),
        }
        undone.words(&mut w);
        if let Some(at) = at {
            w.drop.push((at, m.dll_sha.clone(), m.word.clone()));
        }
        let made = made_by(ctx, &scan.record, &m, &w.all().collect::<Vec<_>>());
        if delete_made && !made.is_empty() {
            let files: usize = made.iter().map(|x| x.files.len()).sum();
            w.plan.summary.push(format!(
                "Deletes what {display} made while it ran ({}).",
                if files == 1 { "1 file".to_string() } else { format!("{files} files") }
            ));
            for x in &made {
                for f in &x.files {
                    w.plan.steps.push(Step::Delete(f.clone()));
                    w.plan.expect_files.push((f.clone(), None));
                }
                for d in &x.dirs {
                    w.plan.steps.push(Step::RemoveDirIfEmpty(d.clone()));
                }
            }
        }
        if !made.is_empty() {
            w.plan.made.push((display.clone(), made));
        }
        let mut dirs = m.dirs.clone();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.as_str().matches('\\').count()));
        for d in dirs {
            // a folder this mod made that another mod's files are in (`greeneye\`) goes with that mod instead
            let inside = |r: &RelPath| r.strip_prefix(&d).is_some();
            let heir =
                w.all_mut().find(|o| o.files.iter().chain(&o.found).any(|f| inside(&f.rel)) || inside(&o.dll_at));
            if let Some(o) = heir {
                if !o.dirs.iter().any(|x| x.same(&d)) {
                    o.dirs.push(d);
                }
                continue;
            }
            w.plan.steps.push(Step::RemoveDirIfEmpty(d));
        }
        if let Some(r) = &m.replaced_dll {
            // the copy that was there before this install comes back
            let bytes = ctx.read_checked(&store_rel(t, r), r);
            match bytes {
                Some(b) => {
                    w.plan.summary.push(format!("Your earlier copy of {display} is put back."));
                    w.dlls.insert(t.clone(), b);
                    w.mods.push(ModRec {
                        at: m.at.clone(),
                        source_sha: "-".into(),
                        label: format!("found as {}", ctx.game.host_dll),
                        name: m.name.clone(),
                        dll_sha: r.clone(),
                        ..ModRec::new(t.clone(), Kind::Adopted, m.order, ctx.host())
                    });
                }
                None => w
                    .plan
                    .notes
                    .push(format!("dllchain's copy of the earlier {display} is gone, so it cannot be put back")),
            }
        }
    }
    w.layout()?;
    w.forget_lone_host();
    Ok(w.finish())
}

/// Before a mod that brings its own loader goes: when another mod on the list uses the same loader, the mod's entry
/// for it moves to that mod, so the loader stays for it. When a DLL dllchain did not install is in the mod's folders
/// (another mod for the same loader, put in by hand), the loader stays too. The line that says so, when it stays.
fn leave_shared_loader(w: &mut Work, m: &mut ModRec) -> Option<String> {
    let ctx = w.ctx;
    let loader = m.dll_at.clone();
    if let Some(h) = w.loaders.iter().position(|o| o.dll_at.same(&loader)) {
        let heir = name_of(ctx.game, &w.loaders[h]);
        if let Some((f, found)) = take_listed(m, &loader) {
            let o = &mut w.loaders[h];
            o.dll_sha = f.sha.clone();
            if found { o.found.push(f) } else { o.files.push(f) }
        }
        return Some(format!("Leaves {loader}, which {heir} uses too."));
    }
    if !lists(m, &loader) {
        return None;
    }
    let others = foreign_dlls(ctx, m);
    let first = others.first()?.clone();
    take_listed(m, &loader);
    Some(match others.len() {
        1 => format!("Leaves {loader}: {first}, which dllchain did not install, may need it."),
        k => format!("Leaves {loader}: {k} DLLs dllchain did not install may need it, such as {first}."),
    })
}

/// The DLLs in a mod's folders that are not the mod's (each folder its files are in, from the top, and the folders in
/// it), at most a few thousand looked at.
fn foreign_dlls(ctx: &Ctx, m: &ModRec) -> Vec<RelPath> {
    let mut tops: Vec<RelPath> = Vec::new();
    for f in m.files.iter().chain(&m.found).filter(|f| !f.rel.is_top_level()) {
        let top = RelPath::of(f.rel.first_part());
        if !tops.iter().any(|t| t.same(&top)) {
            tops.push(top);
        }
    }
    let mut out = Vec::new();
    let mut seen = 0;
    while let Some(dir) = tops.pop() {
        let Ok(path) = resolve(&ctx.dir, &dir) else { continue };
        for name in names_in(&path) {
            seen += 1;
            if seen > 5000 {
                return out;
            }
            let Ok(rel) = dir.join(&name) else { continue };
            match dir_or_file(ctx, &rel) {
                Some(true) => tops.push(rel),
                Some(false) if rel.extension() == ".dll" && !lists(m, &rel) => out.push(rel),
                _ => {}
            }
        }
    }
    out
}

/// A file or folder a mod made while it ran.
#[derive(Clone, Debug, PartialEq)]
pub struct Made {
    /// The file or folder, as the list shows it.
    pub rel: RelPath,
    pub is_dir: bool,
    /// The files to delete: `rel` itself for a file, else every file in the folder that dllchain may delete.
    pub files: Vec<RelPath>,
    /// The folders to remove once empty, deepest first.
    pub dirs: Vec<RelPath>,
}

/// What a mod made while it ran: the files and folders in the game folder and in its DLL's folder whose names start
/// with the mod's word and a `-`, `_` or `.` (`enstash-quickstack.log`, `enstash-backups\`), that no mod on dllchain's
/// list installed. Never a DLL, a program, game data, a file the game, Steam or a loader owns, another mod's (whose
/// longer word starts the same, on dllchain's list or a DLL beside it, such as `enstash-extra.dll`'s
/// `enstash-extra.log`), nor anything behind a link.
///
/// A mod that brings its own loader keeps what it makes in its own folders, under any name (Greeneye's loader writes
/// `greeneye\loader.log`, Town Portals `greeneye\town-portal\town-portal.ini`): every file in a folder its files are in
/// that no mod installed is its, and so is a folder of its own that holds none of them. A folder shared with a mod
/// still on the list (`others`), or one that also holds another folder of someone else's, is left to them.
fn made_by(ctx: &Ctx, record: &Record, m: &ModRec, others: &[&ModRec]) -> Vec<Made> {
    let word = m.word.to_ascii_lowercase();
    let starts = |n: &str, w: &str| n.strip_prefix(w).is_some_and(|r| r.starts_with(['-', '_', '.']));
    let known: Vec<String> = record
        .mods
        .iter()
        .map(|o| o.word.as_str())
        .chain(ctx.game.tools.iter().map(|t| t.word))
        .map(str::to_ascii_lowercase)
        .collect();
    let installed = |r: &RelPath| {
        record.mods.iter().any(|o| {
            o.dll_at.same(r)
                || o.files.iter().chain(&o.retired).chain(&o.found).any(|f| f.rel.same(r))
                || o.dirs.iter().any(|d| d.same(r))
        })
    };
    let deletable = |r: &RelPath| {
        check_target(r, ctx.game).is_ok()
            && r.extension() != ".dll"
            && !r.as_str().eq_ignore_ascii_case(ctx.game.game_log)
            && !installed(r)
    };
    let mut out = Vec::new();
    for folder in std::iter::once(None).chain(m.dll_at.parent().map(Some)) {
        let Some(dir) = folder.as_ref().map_or(Some(ctx.dir.clone()), |f| resolve(&ctx.dir, f).ok()) else { continue };
        let names = names_in(&dir);
        // a mod's own word, and any DLL's here, claims what starts with it
        let lower: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
        let dlls = lower.iter().filter_map(|n| n.strip_suffix(".dll")).map(str::to_string);
        let claims: Vec<String> = known.iter().cloned().chain(dlls).filter(|o| o.len() > word.len()).collect();
        for (name, n) in names.into_iter().zip(&lower) {
            if !starts(n, &word) || claims.iter().any(|o| starts(n, o)) {
                continue;
            }
            let Ok(rel) = folder.as_ref().map_or_else(|| RelPath::parse(&name), |f| f.join(&name)) else { continue };
            let Some(is_dir) = dir_or_file(ctx, &rel) else { continue };
            if !deletable(&rel) {
                continue;
            }
            if is_dir {
                let (mut files, mut dirs) = (Vec::new(), Vec::new());
                walk_made(ctx, &rel, &deletable, &mut files, &mut dirs);
                if !files.is_empty() {
                    out.push(Made { rel, is_dir: true, files, dirs });
                }
            } else {
                out.push(Made { rel: rel.clone(), is_dir: false, files: vec![rel], dirs: Vec::new() });
            }
        }
    }
    if is_own_loader(ctx.game, m) {
        made_in_own_folders(ctx, m, others, &installed, &deletable, &mut out);
    }
    out
}

/// [`made_by`]'s part for a mod that brings its own loader: what is in its own folders that no mod installed.
fn made_in_own_folders(
    ctx: &Ctx,
    m: &ModRec,
    others: &[&ModRec],
    installed: &dyn Fn(&RelPath) -> bool,
    deletable: &dyn Fn(&RelPath) -> bool,
    out: &mut Vec<Made>,
) {
    let under = |r: &RelPath, d: &RelPath| r.strip_prefix(d).is_some();
    let mine: Vec<&RelPath> = m.files.iter().chain(&m.found).map(|f| &f.rel).filter(|r| !r.is_top_level()).collect();
    let mut folders: Vec<RelPath> = Vec::new();
    for a in mine.iter().flat_map(|r| r.ancestors()) {
        if !folders.iter().any(|f| f.same(&a)) {
            folders.push(a);
        }
    }
    let ours_in = |d: &RelPath| mine.iter().any(|r| under(r, d));
    for d in &folders {
        if others.iter().any(|o| o.files.iter().chain(&o.found).any(|f| under(&f.rel, d))) {
            continue;
        }
        let Ok(path) = resolve(&ctx.dir, d) else { continue };
        let entries: Vec<(RelPath, bool)> = names_in(&path)
            .into_iter()
            .filter_map(|n| {
                let rel = d.join(&n).ok()?;
                let is_dir = dir_or_file(ctx, &rel)?;
                Some((rel, is_dir))
            })
            .collect();
        // a folder of someone else's (a mod put in by hand beside this one) is never the mod's; a folder of logs is
        let foreign = |r: &RelPath| !ours_in(r) && !is_runtime_folder(r);
        // a folder the mod's files are only further down in, beside someone else's folder: theirs too
        let holds = mine.iter().any(|r| r.parent().is_some_and(|p| p.same(d)));
        if !holds && entries.iter().any(|(r, is_dir)| *is_dir && foreign(r)) {
            continue;
        }
        for (rel, is_dir) in entries {
            if out.iter().any(|x| x.rel.same(&rel)) {
                continue;
            }
            if !is_dir {
                if deletable(&rel) {
                    out.push(Made { rel: rel.clone(), is_dir: false, files: vec![rel], dirs: Vec::new() });
                }
            } else if !ours_in(&rel) && is_runtime_folder(&rel) && !installed(&rel) && deletable(&rel) {
                let (mut files, mut dirs) = (Vec::new(), Vec::new());
                walk_made(ctx, &rel, deletable, &mut files, &mut dirs);
                if !files.is_empty() {
                    out.push(Made { rel, is_dir: true, files, dirs });
                }
            }
        }
    }
}

/// A folder a mod writes into while it runs, by the words in its name: `logs\`, `cache\`, `crash_dumps\`, `backups\`
/// (not `catalog\` or `templates\`).
fn is_runtime_folder(rel: &RelPath) -> bool {
    let name = rel.file_name().to_ascii_lowercase();
    name.split(|c: char| !c.is_ascii_alphabetic()).any(|part| {
        let one =
            part.strip_suffix("es").filter(|p| p.ends_with('h')).or_else(|| part.strip_suffix('s')).unwrap_or(part);
        ["log", "cache", "crash", "dump", "backup", "temp", "tmp"].contains(&one)
    })
}

/// The files in `dir` (and its folders) that `deletable` allows, and its folders, deepest first, `dir` last. A link
/// and what is behind it are left out.
fn walk_made(
    ctx: &Ctx,
    dir: &RelPath,
    deletable: &dyn Fn(&RelPath) -> bool,
    files: &mut Vec<RelPath>,
    dirs: &mut Vec<RelPath>,
) {
    let Ok(path) = resolve(&ctx.dir, dir) else { return };
    for name in names_in(&path) {
        let Ok(rel) = dir.join(&name) else { continue };
        let Some(is_dir) = dir_or_file(ctx, &rel) else { continue };
        if is_dir {
            walk_made(ctx, &rel, deletable, files, dirs);
        } else if deletable(&rel) {
            files.push(rel);
        }
    }
    dirs.push(dir.clone());
}

/// Whether `rel` is a folder (`Some(true)`) or a file; `None` when it is gone, or is a link or behind one.
fn dir_or_file(ctx: &Ctx, rel: &RelPath) -> Option<bool> {
    let p = resolve(&ctx.dir, rel).ok()?;
    std::fs::symlink_metadata(p).ok().map(|m| m.is_dir())
}

/// The names in a folder, sorted (none when it cannot be read; a name Windows cannot spell in Unicode is left out).
fn names_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

/// Puts back the files an install took out of the mod's folder (an older version's leftovers, a package host's off
/// switch), unless something is in their place again.
fn put_back_retired(w: &mut Work, m: &ModRec) {
    let ctx = w.ctx;
    for f in &m.retired {
        let Some(backup) = &f.backup else { continue };
        let Some(bytes) = ctx.read_checked(backup, &f.sha) else {
            w.plan.notes.push(format!("dllchain's copy of {} is gone, so it cannot be put back", f.rel));
            continue;
        };
        // the copy goes either way: with the mod off the list, nothing would ever give it back or clear it
        if ctx.exists(&f.rel) {
            w.plan.notes.push(format!(
                "{} is there again, so dllchain leaves it as it is and drops the older copy it kept",
                f.rel
            ));
        } else {
            w.plan.summary.push(format!("Puts back {}, which was there before.", f.rel));
            w.plan.expect_files.push((f.rel.clone(), Some(f.sha.clone())));
            w.plan.steps.push(Step::Write(f.rel.clone(), bytes));
        }
        w.plan.steps.push(Step::Delete(backup.clone()));
    }
}

/// Makes the plan's changes, checks the folder, and undoes everything when a step or the check fails.
pub fn apply(ctx: &Ctx, plan: &Plan) -> Result<Vec<String>> {
    if plan.steps.is_empty() {
        return Ok(Vec::new());
    }
    let mut id = clock::now_stamp();
    let mut k = 2;
    while ctx.exists(&RelPath::parse(&format!("{OWN_DIR}\\backup\\{id}"))?) {
        id = format!("{}-{k}", clock::now_stamp());
        k += 1;
    }
    let mut t = Txn::begin(&ctx.dir, &id, &plan.action)?;
    let run = |t: &mut Txn| -> Result<()> {
        for (n, step) in plan.steps.iter().enumerate() {
            if plan.fail_after == Some(n) {
                fail!("stopped on purpose after {n} steps (test)");
            }
            match step {
                Step::Write(r, b) => t.write(r, b)?,
                Step::Rename(a, b) => t.rename(a, b)?,
                Step::Delete(r) => t.delete(r)?,
                Step::RemoveDirIfEmpty(r) => {
                    t.remove_dir_if_empty(r)?;
                }
            }
        }
        verify(ctx, plan)
    };
    if let Err(e) = run(&mut t) {
        let mut lines = t.log.clone();
        let notes = t.rollback();
        lines.push(format!("FAILED: {e}; everything was put back"));
        lines.extend(notes.iter().cloned());
        append_log(ctx, &plan.action, &lines);
        let mut msg = format!("{e}. Nothing was changed: every step was undone.");
        for n in notes {
            msg.push_str(&format!("\n{n}"));
        }
        return Err(Error(msg));
    }
    let log = t.commit()?;
    if plan.remove_own_dir {
        remove_own_dir(ctx)?;
    } else {
        append_log(ctx, &plan.action, &log);
    }
    Ok(log)
}

/// The check after a run: every file as planned, the chain lines as planned, and dllchain's record readable again.
fn verify(ctx: &Ctx, plan: &Plan) -> Result<()> {
    for (i, (rel, sha)) in plan.expect_files.iter().enumerate() {
        if plan.expect_files[i + 1..].iter().any(|(r, _)| r.same(rel)) {
            continue; // a later step changes it again
        }
        let now = ctx.sha(rel);
        if now != *sha {
            fail!("the check after the change found {rel} not as planned");
        }
    }
    let list = plan.list.clone().unwrap_or_else(|| ctx.chain_rel());
    let format = ctx.format_of(&list);
    let parsed = ctx.read(&list).map(|b| ChainFile::decode_as(&b, format).parse(ctx.game.host_dll)).unwrap_or_default();
    for (name, on) in &plan.expect_chain {
        match parsed.find(name) {
            Some(e) if e.on == *on => {}
            _ => fail!("the check after the change found the line for {name} in {list} not as planned"),
        }
    }
    let host = ctx.host();
    let hosts = plan.record.mods.iter().filter(|m| m.dll_at.same(&host)).count();
    if hosts > 1 {
        fail!("the check after the change found two mods planned as {host}");
    }
    if !plan.remove_own_dir && !plan.record.is_empty() {
        let text = ctx.read(&RelPath::of(RECORD)).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        let (back, warn) = Record::parse(&text);
        if back != plan.record || !warn.is_empty() {
            fail!("the check after the change could not read dllchain's record back");
        }
    }
    Ok(())
}

/// Removes dllchain's folder once nothing is left to undo, except originals of files the player changed.
fn remove_own_dir(ctx: &Ctx) -> Result<()> {
    let own = resolve(&ctx.dir, &RelPath::of(OWN_DIR))?;
    let originals = own.join("originals");
    let keep = prune_empty(&originals);
    for e in std::fs::read_dir(&own)? {
        let e = e?;
        if keep && e.file_name().eq_ignore_ascii_case("originals") {
            continue;
        }
        let p = e.path();
        if e.file_type()?.is_dir() { std::fs::remove_dir_all(&p)? } else { std::fs::remove_file(&p)? }
    }
    if !keep {
        std::fs::remove_dir(&own)?;
    }
    Ok(())
}

/// Removes the empty folders under `dir` (and `dir` when it ends up empty); true when a file is left in it.
fn prune_empty(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut files = false;
    for e in rd.flatten() {
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        files |= !is_dir || prune_empty(&e.path());
    }
    if !files {
        let _ = std::fs::remove_dir(dir);
    }
    files
}

fn append_log(ctx: &Ctx, action: &str, lines: &[String]) {
    use std::io::Write;
    let Ok(p) = resolve(&ctx.dir, &RelPath::of(LOG)) else {
        return;
    };
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{} {action}\r", clock::now_text());
        for l in lines {
            let _ = writeln!(f, "  {l}\r");
        }
    }
}

/// One row of the status list.
#[derive(Clone, Debug)]
pub struct Row {
    pub file: RelPath,
    /// `None` for a DLL that is not on dllchain's list.
    pub word: Option<String>,
    pub name: String,
    pub role: String,
    /// When dllchain installed or chained it (local time), empty when it is not on dllchain's list.
    pub at: String,
    pub source: String,
    pub sha: Option<String>,
    pub installed: bool,
    /// The web page it was downloaded from, when Windows noted one.
    pub page: Option<String>,
    /// Everything dllchain knows about it, for the details pane: (label, value) lines; an empty label carries on the
    /// line above.
    pub details: Vec<(String, String)>,
    /// Its version: from the DLL, else from the zip's name.
    pub version: Option<String>,
    /// Whether it loaded at the game's last start, for a mod whose log dllchain knows.
    pub load: Option<(Load, String)>,
}

/// What a mod's log says about the game's last start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    Loaded,
    /// It loaded, and says the player switched its main part off.
    Off,
    NotLoaded,
    /// The game has not started since the mod was installed.
    NotYet,
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub rows: Vec<Row>,
    pub notes: Vec<String>,
    /// What a repair would change (empty: nothing to repair).
    pub repair: Vec<String>,
    /// The last change, which Undo takes back: what it was and when (local time).
    pub undo: Option<(String, String)>,
    /// The loader that runs mods from folders of their own, when the game has one: its name and that folder (`mods`).
    pub package_host: Option<(String, String)>,
    /// The loader that only loads mods, when it is the host: its name and the file it lists them in.
    pub loader: Option<(String, String)>,
    /// Who loads the mods, in one line: "XHL-Core loads the chain (dinput8-chain.ini)."
    pub host: Option<String>,
}

/// The numbers of a version, without a tag: `0.12.2-beta` and `v0.12.2` are both `0.12.2`.
fn version_numbers(v: &str) -> &str {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let end = v.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(v.len());
    v[..end].trim_end_matches('.')
}

/// Whether the mod whose DLL is `dll` loaded at the game's last start, from its own log next to it. The game writes
/// its log at every start, so a game log newer than the DLL with no newer mod log means it did not load.
pub fn load_check(ctx: &Ctx, word: &str, dll: &RelPath, version: Option<&str>) -> Option<(Load, String)> {
    let log = ctx.game.tool(word)?.log.as_ref()?;
    let log_rel = match dll.parent() {
        Some(dir) => dir.join(log.file).ok()?,
        None => RelPath::of(log.file),
    };
    let installed = ctx.modified(dll)?;
    let game_started = ctx.modified(&RelPath::of(ctx.game.game_log)).is_some_and(|t| t > installed);
    let fresh = ctx.modified(&log_rel).is_some_and(|t| t >= installed);
    if !fresh {
        return Some(if game_started {
            (Load::NotLoaded, format!("did not load at the game's last start: {log_rel} has nothing from it"))
        } else {
            (Load::NotYet, "not started yet: start the game once, and dllchain can tell whether it loaded".into())
        });
    }
    let text = ctx.read(&log_rel).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let logged = lines.next().and_then(|l| l.find(log.first).map(|i| l[i + log.first.len()..].trim().to_string()));
    let Some(logged) = logged else {
        return Some((
            Load::NotLoaded,
            format!("did not load at the game's last start: {log_rel} does not start as it should"),
        ));
    };
    if let Some(v) = version.filter(|v| version_numbers(v) != version_numbers(&logged)) {
        return Some((
            Load::NotLoaded,
            format!(
                "an older copy ran at the game's last start: {log_rel} says {logged}, not {v}. Another copy may be in the game folder"
            ),
        ));
    }
    let rest: Vec<&str> = lines.collect();
    if rest.iter().any(|l| l.contains(log.ok)) {
        Some((Load::Loaded, format!("loaded at the game's last start ({logged})")))
    } else if rest.iter().any(|l| l.contains(log.off)) {
        Some((Load::Off, format!("loaded at the game's last start ({logged}), but its log says it is switched off")))
    } else {
        Some((
            Load::NotLoaded,
            format!("started at the game's last start ({logged}) but did not finish loading: see {log_rel}"),
        ))
    }
}

/// Every proxy DLL next to the game and every chain entry, with its SHA-256 and where it came from.
pub fn status(ctx: &Ctx) -> Result<Status> {
    let scan = scan(ctx)?;
    let mut st = Status { notes: scan.notes.clone(), ..Status::default() };
    let emf_moved: Vec<String> = ctx
        .read(&RelPath::of("emf\\moved.txt"))
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .filter_map(|l| l.strip_prefix("moved\t").map(|s| s.trim().to_lowercase()))
                .collect()
        })
        .unwrap_or_default();
    let host = ctx.host();
    // `listed`: the mod the row is for, when the file alone does not say (a loader several mods share)
    let row = |rel: &RelPath, role: String, listed: Option<&ModRec>| -> Row {
        let sha = ctx.sha(rel);
        let pair = |label: &str, value: String| (label.to_string(), value);
        let mut details = vec![pair("File", format!("{rel} ({role})"))];
        let rebuilt =
            |m: &ModRec| m.dll_at.same(rel) && scan.rebuilt(&m.word).is_some_and(|r| Some(&r.2.sha256) == sha.as_ref());
        let by_sha = || {
            let mut mods = scan.record.mods.iter().filter(|m| !is_own_loader(ctx.game, m));
            mods.find(|m| Some(&m.dll_sha) == sha.as_ref() || rebuilt(m))
        };
        if let Some(m) = listed.or_else(by_sha) {
            let name = name_of(ctx.game, m);
            let folder = m.label.strip_suffix('\\');
            let source = match m.kind {
                Kind::Installed => m.label.clone(),
                Kind::Adopted => "there before dllchain".into(),
            };
            match m.kind {
                Kind::Installed => {
                    details.push(pair("Installed", clock::readable(&m.at)));
                    details.push(pair(
                        "From",
                        match folder {
                            Some(f) => format!("the folder \"{f}\""),
                            None => m.label.clone(),
                        },
                    ));
                }
                Kind::Adopted => {
                    details.push(pair("Found", clock::readable(&m.at)));
                    details.push(pair("From", "there before dllchain, or copied in by hand".into()));
                    details.push(pair("", "dllchain keeps it in the chain and never deletes it".into()));
                }
            }
            details.push(pair("Web page", m.from_url.clone().unwrap_or_else(|| "not known".into())));
            let rels: Vec<&RelPath> = m.files.iter().map(|f| &f.rel).collect();
            if !rels.is_empty() {
                let mut where_ =
                    places(&rels).into_iter().map(|(p, _)| p).filter(|p| !p.is_empty()).collect::<Vec<_>>();
                let in_ = match where_.len() {
                    0 => String::new(),
                    1..=3 => {
                        let last = where_.pop().unwrap_or_default();
                        if where_.is_empty() {
                            format!(", in {last}")
                        } else {
                            format!(", in {} and {last}", where_.join(", "))
                        }
                    }
                    _ => ", in several folders".into(),
                };
                details.push(pair("Files added", format!("{}{in_}", rels.len())));
            }
            for f in &m.files {
                let state = match ctx.sha(&f.rel) {
                    None => "missing",
                    Some(s) if s != f.sha => "changed since it was installed",
                    _ if f.backup.is_some() => "replaced yours, which is kept",
                    _ => continue,
                };
                details.push(pair("", format!("{} ({state})", f.rel)));
            }
            match m.found.len() {
                0 => {}
                1 => details.push(pair("Already there", format!("{} (Uninstall leaves it)", m.found[0].rel))),
                k => details.push(pair("Already there", format!("{k} files (Uninstall leaves them)"))),
            }
            let own_loader = is_own_loader(ctx.game, m);
            if own_loader {
                details.push(pair(
                    "Loader",
                    format!("{rel}, which the game loads by itself, not through {}", ctx.game.chain_file),
                ));
                let shared: Vec<String> = scan
                    .record
                    .mods
                    .iter()
                    .filter(|o| o.word != m.word && o.dll_at.same(rel))
                    .map(|o| name_of(ctx.game, o))
                    .collect();
                if !shared.is_empty() {
                    details.push(pair("", format!("shared with {}", shared.join(", "))));
                }
            }
            for r in missing(ctx, &scan, &rels) {
                details.push(pair("Note", r.line()));
                details.push(pair("", format!("{}'s page: {}", r.short, r.page)));
            }
            if rebuilt(m) {
                details.push(pair("Changed", "replaced by hand with another build since dllchain kept a copy".into()));
                details.push(pair("", "a repair keeps yours and updates dllchain's copy".into()));
            }
            details.push(pair("SHA-256", sha.clone().unwrap_or_else(|| m.dll_sha.clone())));
            if m.source_sha != "-" && m.source_sha != m.dll_sha {
                details.push(pair("Zip SHA-256", m.source_sha.clone()));
            }
            if let Some(r) = &m.replaced_dll {
                details
                    .push(pair("Replaced", format!("an earlier copy (SHA-256 {}); Uninstall puts it back", short(r))));
            }
            if m.word != name {
                details.push(pair("Short name", m.word.clone()));
            }
            // only the DLL's own version says which copy should have run; one from a zip's name is a guess. It is read
            // only when needed: for a mod with a known log, or one with no version on record
            let has_log = ctx.game.tool(&m.word).is_some_and(|t| t.log.is_some());
            // (a loader's own version is the loader's, not the mod's)
            let dll_version = (!own_loader && (has_log || m.version.is_none()))
                .then(|| ctx.read(rel).and_then(|b| inspect(&b, ctx.game).ok()).and_then(|i| i.version))
                .flatten();
            let version = m.version.clone().or_else(|| dll_version.clone());
            let load = load_check(ctx, &m.word, rel, dll_version.as_deref());
            if let Some(v) = &version {
                details.insert(1, pair("Version", v.clone()));
            }
            if let Some((_, l)) = &load {
                details.insert(1 + usize::from(version.is_some()), pair("Last start", l.clone()));
            }
            return Row {
                file: rel.clone(),
                word: Some(m.word.clone()),
                name,
                role,
                at: m.at.clone(),
                source,
                sha,
                installed: m.kind == Kind::Installed,
                page: m.from_url.clone(),
                details,
                version,
                load,
            };
        }
        let info = ctx.read(rel).and_then(|b| inspect(&b, ctx.game).ok());
        let name = info
            .as_ref()
            .and_then(|i| default_name(i).or_else(|| i.word.as_ref().map(|w| ctx.game.display(&w.0))))
            .unwrap_or_else(|| "unknown".into());
        let source = if emf_moved.contains(&rel.key()) {
            "moved by EMF".to_string()
        } else if rel.same(&host) {
            "put there by hand".to_string()
        } else {
            "not installed by dllchain".to_string()
        };
        let from = if emf_moved.contains(&rel.key()) {
            "moved there by EMF (emf\\moved.txt says so)"
        } else if rel.same(&host) {
            "put there by hand or by another tool"
        } else {
            "not installed by dllchain"
        };
        details.push(pair("From", format!("{from}; not on dllchain's list")));
        let package_host = info.as_ref().and_then(|i| i.word.as_ref()).and_then(|w| ctx.game.package_host(&w.0));
        if rel.same(&host) && (scan.record.is_empty() || scan.loader().is_some()) {
            details.push(pair("", "dllchain leaves it as it is.".into()));
            match package_host {
                Some(ph) => details
                    .push(pair("", format!("A mod you install goes into {}\\<mod>\\, where {name} loads it.", ph.dir))),
                None if scan.loader().is_some() => details
                    .push(pair("", format!("A mod you install is listed in {}, where {name} loads it.", scan.list))),
                None => {
                    details.push(pair("", "When you install another mod, it joins the chain".into()));
                    details.push(pair("", "and dllchain keeps a copy of it.".into()));
                }
            }
        }
        let version = info.as_ref().and_then(|i| i.version.clone());
        if let Some(v) = &version {
            details.push(pair("Version", v.clone()));
        }
        if let Some(p) = info.as_ref().and_then(|i| i.product_name.clone()) {
            details.push(pair("Product name", p));
        }
        details.push(pair("SHA-256", sha.clone().unwrap_or_else(|| "the file is missing".into())));
        Row {
            file: rel.clone(),
            word: None,
            name,
            role,
            at: String::new(),
            source,
            sha,
            installed: false,
            page: None,
            details,
            version,
            load: None,
        }
    };
    let list = &scan.list;
    let can = scan.host.as_ref().is_some_and(|(_, i)| i.can_host);
    if ctx.exists(&host) {
        let role = match scan.loader() {
            Some(_) => format!("game loads it; loads the mods in {list}"),
            None if can => "game loads it; starts others".into(),
            None => "game loads it; starts no others".into(),
        };
        st.rows.push(row(&host, role, None));
    }
    let starter = scan.loader().map_or(ctx.game.host_dll, |l| l.name);
    for e in &scan.parsed.entries {
        let role = match (e.on, ctx.exists(&e.name)) {
            (true, true) => format!("started by {starter}"),
            (true, false) => "listed, but missing".to_string(),
            // a switched-off line for a file that is not there and not dllchain's (such as an example in the file as
            // its loader ships)
            (false, false) if !scan.record.mods.iter().any(|m| m.dll_at.same(&e.name)) => continue,
            (false, _) => "switched off".to_string(),
        };
        st.rows.push(row(&e.name, role, None));
    }
    if let Some(l) = scan.loader() {
        st.loader = Some((l.name.to_string(), list.to_string()));
    }
    if let Some(target) = scan.chain.as_ref().and_then(|c| c.forward().map(|v| (v, c.format.forward))) {
        let (value, (section, key)) = target;
        // a path outside the game folder is the loader's business: only a missing file in it is noted
        if RelPath::parse(&value).is_ok_and(|r| !ctx.exists(&r)) {
            st.notes.push(format!(
                "The {key}= line in [{section}] of {list} names {value}, which is not in the game folder: change or remove that line."
            ));
        }
    }
    // Multi DLL Installer renames each mod to dinput9.dll, dinput10.dll and so on; whatever is dinput8.dll loads them
    // (an older dllchain could name a mod so: those are its own)
    let mut renamed: Vec<(u32, String)> = std::fs::read_dir(&ctx.dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stem =
                name.get(..name.len().checked_sub(4)?).filter(|_| name.to_ascii_lowercase().ends_with(".dll"))?;
            let n = ctx.game.renamed_by_others(stem).then(|| stem.trim_start_matches(|c: char| !c.is_ascii_digit()))?;
            let ours = scan.record.mods.iter().any(|m| m.dll_at.same(&RelPath::of(&name)));
            (!ours && e.path().is_file()).then(|| (n.parse().unwrap_or(u32::MAX), name.clone()))
        })
        .collect();
    renamed.sort();
    let renamed: Vec<String> = renamed.into_iter().map(|r| r.1).collect();
    if !renamed.is_empty() {
        st.notes.push(format!(
            "{} look like mods Multi DLL Installer renamed. dllchain leaves them as they are and never gives a mod such a name.",
            renamed.join(", ")
        ));
    }
    for p in ctx.game.proxy_names {
        let rel = RelPath::of(&format!("{p}.dll"));
        if rel.same(&host) {
            continue;
        }
        // the mods that brought it as their own loader, one row each
        let mut mods = scan.record.mods.iter().filter(|m| is_own_loader(ctx.game, m) && m.dll_at.same(&rel)).peekable();
        if mods.peek().is_none() {
            if ctx.exists(&rel) {
                let mut r = row(&rel, "another loader's DLL, left alone".into(), None);
                r.source = "not installed by dllchain".into();
                st.rows.push(r);
            }
            continue;
        }
        let role = if ctx.exists(&rel) { "its own loader; the game loads it" } else { "its own loader, but missing" };
        for m in mods {
            st.rows.push(row(&rel, role.into(), Some(m)));
        }
    }
    let ph = package_host(ctx, &scan);
    st.package_host = ph.map(|p| (ctx.game.display(p.word), p.dir.to_string()));
    if let Some(ph) = ph {
        // an older dllchain chained mods with the package host, which runs none of them
        let host = ctx.game.display(ph.word);
        for m in scan.record.mods.iter().filter(|m| is_chained(ctx.game, m) && m.word != ph.word) {
            st.notes.push(format!(
                "{} does not run: {host} loads no DLL next to the game. Install it again, and dllchain moves it into {}, where {host} loads it.",
                name_of(ctx.game, m),
                package_dir(ph, &m.word)
            ));
        }
    }
    let host_row = st.rows.iter().find(|r| r.file.same(&host)).map(|r| r.name.clone());
    st.host = match (&st.package_host, scan.loader(), host_row) {
        (Some((name, dir)), _, _) => Some(format!("{name} loads the mods from {dir}\\<mod>\\.")),
        (None, Some(l), _) => Some(format!("{} loads the chain ({list}).", l.name)),
        (None, None, Some(name)) if can && scan.parsed.on().next().is_some() => {
            Some(format!("{name} loads the chain ({list})."))
        }
        _ => None,
    };
    let loader = st.package_host.as_ref().map_or("its loader", |p| p.0.as_str()).to_string();
    for m in scan.record.mods.iter().filter(|m| is_package(m)) {
        st.rows.push(row(&m.dll_at, format!("loaded by {loader} from its own folder"), Some(m)));
    }
    if let Ok(Some(run)) = crate::txn::last_run(&ctx.dir).map(|r| r.filter(|r| r.unreadable.is_empty())) {
        st.undo = Some((short_action(&run.action).to_string(), clock::readable(&run.at)));
    }
    match plan_repair(ctx) {
        Ok(p) if !p.is_empty() => st.repair = p.summary,
        Ok(_) => {}
        Err(e) => st.repair = vec![format!("cannot be repaired automatically: {e}")],
    }
    Ok(st)
}

/// The mods dllchain installed, for the uninstall list: (word, name players see, where its DLL is).
pub fn installed(ctx: &Ctx) -> Result<Vec<(String, String, RelPath)>> {
    let scan = scan(ctx)?;
    Ok(scan
        .record
        .mods
        .iter()
        .filter(|m| m.kind == Kind::Installed)
        .map(|m| (m.word.clone(), name_of(ctx.game, m), m.dll_at.clone()))
        .collect())
}
