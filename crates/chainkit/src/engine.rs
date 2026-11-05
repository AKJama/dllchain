//! Install, repair and uninstall, each in two halves: a [`Plan`] that says what would change (and changes nothing),
//! and [`apply`], which makes the changes in one undoable run and checks the folder afterwards.
//!
//! Who hosts: only the DLL called `dinput8.dll` runs, and it starts the others from `dinput8-chain.ini`. With EMF's
//! loader as `dinput8.dll`, every mod is a guest. Otherwise the host is the mod that is `dinput8.dll` now when it can
//! host, else the newest mod that can; a mod that cannot read the chain file is only ever the host when it is alone.
//! So the result is the same whatever order mods come in.

use crate::chainini::{AddResult, ChainFile, Parsed};
use crate::dllinfo::{DllInfo, WordFrom, contains_text, inspect};
use crate::game::{Game, PackageHost, Requirement};
use crate::hash::{sha256_hex, short};
use crate::paths::{RelPath, is_word, resolve};
use crate::record::{FileRec, Kind, ModRec, RECORD, Record, clean_name, store_rel};
use crate::source::{ModSource, check_target, release_extra};
use crate::steam::is_game_folder;
use crate::txn::{Txn, recover};
use crate::{Error, OWN_DIR, Result, clock, fail};
use std::collections::HashMap;
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

    fn modified(&self, rel: &RelPath) -> Option<std::time::SystemTime> {
        resolve(&self.dir, rel).ok().and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok())
    }

    fn host(&self) -> RelPath {
        RelPath::of(self.game.host_dll)
    }

    fn chain_rel(&self) -> RelPath {
        RelPath::of(self.game.chain_file)
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
    steps: Vec<Step>,
    record: Record,
    /// Files as they must be after the run (`None`: not there).
    expect_files: Vec<(RelPath, Option<String>)>,
    /// Chain lines as they must be after the run.
    expect_chain: Vec<(RelPath, bool)>,
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
}

/// What is in the game folder now.
struct Scan {
    record: Record,
    notes: Vec<String>,
    chain: Option<ChainFile>,
    parsed: Parsed,
    /// `dinput8.dll` now, with what it is.
    host: Option<(Vec<u8>, DllInfo)>,
    /// Guests replaced by hand with another build of the same mod: (word, the file's bytes, what it is).
    rebuilt: Vec<(String, Vec<u8>, DllInfo)>,
}

impl Scan {
    fn rebuilt(&self, word: &str) -> Option<&(String, Vec<u8>, DllInfo)> {
        self.rebuilt.iter().find(|r| r.0 == word)
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
    let chain = ctx.read(&ctx.chain_rel()).map(|b| ChainFile::decode(&b));
    let parsed = chain.as_ref().map(|c| c.parse(ctx.game.host_dll)).unwrap_or_default();
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
    if !record.mods.is_empty() {
        for (name, on) in &record.chain {
            match parsed.find(name) {
                None => notes.push(format!(
                    "{}: the line for {name} was taken out by someone else since dllchain last ran",
                    ctx.game.chain_file
                )),
                Some(e) if e.on != *on => notes.push(format!(
                    "{}: {name} was switched {} by someone else since dllchain last ran",
                    ctx.game.chain_file,
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
                notes.push(format!(
                    "{}: {} was added by someone else since dllchain last ran ({what})",
                    ctx.game.chain_file, e.name
                ));
            }
        }
    }
    for r in &parsed.refused {
        notes.push(format!(
            "{} line {} ignored, as every chain loader does: {} ({})",
            ctx.game.chain_file,
            r.line + 1,
            r.text,
            r.why
        ));
    }
    let mut scan = Scan { record, notes, chain, parsed, host, rebuilt: Vec::new() };
    scan.rebuilt = scan.record.mods.iter().filter_map(|m| rebuilt(ctx, &scan, m)).collect();
    Ok(scan)
}

/// A guest's DLL that someone replaced by hand where dllchain left it, with a newer build of the same mod: dllchain
/// takes it as that mod's DLL from now on instead of putting its own copy back. It must still be a 64-bit DLL, not
/// EMF and not another mod on the list, and if it says which mod it is, that is this one. A missing file, or anything
/// else in its place, is still repaired. (`dinput8.dll` itself is [`Work::adopt_host`]'s.)
fn rebuilt(ctx: &Ctx, scan: &Scan, m: &ModRec) -> Option<(String, Vec<u8>, DllInfo)> {
    if is_package(m) || m.dll_at.same(&ctx.host()) || locate(ctx, scan, m).is_some() {
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
    (same_mod && !info.is_emf && !theirs).then(|| (m.word.clone(), bytes, info))
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
/// game, Steam or another loader does not own; its files are ones a zip could install; its backups are in
/// dllchain's `originals` folder.
fn keep_safe(record: &mut Record, game: &Game, notes: &mut Vec<String>) {
    let originals = format!("{OWN_DIR}\\originals\\");
    let host_stem = game.host_dll.trim_end_matches(".dll");
    record.mods.retain_mut(|m| {
        let at = &m.dll_at;
        let name = at.file_name().to_lowercase();
        let dll_ok = at.is_top_level()
            && at.extension() == ".dll"
            && !game.protected_names.contains(&name.as_str())
            && game.proxy_names.iter().all(|p| *p == host_stem || format!("{p}.dll") != name);
        if !dll_ok && !game.package_hosts.iter().any(|h| at.same(&package_dll(game, h, &m.word))) {
            notes.push(format!("record: {} would have its DLL at {at}, which dllchain never uses; left out", m.word));
            return false;
        }
        let word = m.word.clone();
        for (what, list) in [("file", &mut m.files), ("old file", &mut m.retired)] {
            list.retain(|f| {
                let ok = check_target(&f.rel, game).is_ok()
                    && f.backup.as_ref().is_none_or(|b| b.key().starts_with(&originals));
                if !ok {
                    notes.push(format!("record: {word}'s {what} {} is not one dllchain installs; left out", f.rel));
                }
                ok
            });
        }
        true
    });
}

/// The folder a package host runs a mod from: `mods\<word>`.
fn package_dir(host: &PackageHost, word: &str) -> RelPath {
    RelPath::of(&format!("{}\\{word}", host.dir))
}

/// The mod's DLL there, under the name it shipped with: `mods\<word>\dinput8.dll`.
fn package_dll(game: &Game, host: &PackageHost, word: &str) -> RelPath {
    RelPath::of(&format!("{}\\{}", package_dir(host, word), game.host_dll))
}

/// A mod dllchain put into a package host's folder rather than the chain.
fn is_package(m: &ModRec) -> bool {
    !m.dll_at.is_top_level()
}

/// The loader in the game that runs DLL mods from folders of their own (ShroudForge): the game's `dinput8.dll`, or a
/// mod on dllchain's list (an older dllchain chained ShroudForge like any other mod).
fn package_host(ctx: &Ctx, scan: &Scan) -> Option<&'static PackageHost> {
    let host = scan.host.as_ref().and_then(|(_, i)| i.word.as_ref()).map(|w| w.0.as_str());
    let listed = scan.record.mods.iter().filter(|m| !is_package(m)).map(|m| m.word.as_str());
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
    let stored = ctx.read(&store_rel(&m.word, &m.dll_sha)).filter(|b| sha256_hex(b) == m.dll_sha);
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

    fn display(&self, word: &str) -> String {
        match self.mods.iter().chain(&self.packages).find(|m| m.word == word) {
            Some(m) => name_of(self.ctx.game, m),
            None => self.ctx.game.display(word),
        }
    }

    fn unique_word(&self, word: &str) -> String {
        let mut w = word.to_string();
        let mut n = 2;
        while self.mods.iter().chain(&self.packages).any(|m| m.word == w) {
            w = format!("{word}-{n}");
            n += 1;
        }
        w
    }

    /// A `dinput8.dll` that is neither EMF's nor one of dllchain's mods: another mod copied over, or there before
    /// dllchain. It joins the chain like any mod (a newer copy of a mod on the list takes that mod's place); dllchain
    /// keeps a copy and never deletes it. With `installing`, the word of the mod being installed: a copy of that same
    /// tool is replaced by the install and comes back on uninstall; its SHA-256 is returned.
    fn adopt_host(&mut self, installing: Option<&str>) -> Option<String> {
        let (bytes, info) = self.scan.host.as_ref()?;
        if info.is_emf || self.mods.iter().any(|m| m.dll_sha == info.sha256) {
            return None;
        }
        let host = self.ctx.host();
        let word = info.word.as_ref().map(|w| w.0.clone());
        // a package host runs its mods without a chain: it joins one only when there is one already
        if self.mods.is_empty() && word.as_deref().is_some_and(|w| self.ctx.game.package_host(w).is_some()) {
            return None;
        }
        if word.is_some() && word.as_deref() == installing {
            self.plan.summary.push(format!(
                "The new {} takes the place of the older copy in your game.",
                self.display(installing.unwrap_or_default())
            ));
            self.plan.summary.push("dllchain keeps the older copy, and Uninstall puts it back.".into());
            self.drop.push((host, info.sha256.clone(), installing.unwrap_or_default().to_string()));
            self.write_store(installing.unwrap_or_default(), bytes);
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
            word: w.clone(),
            kind: Kind::Adopted,
            order,
            at: clock::now_text(),
            source_sha: "-".into(),
            label: format!("found as {}", self.ctx.game.host_dll),
            name: default_name(info),
            from_url: None,
            dll_sha: info.sha256.clone(),
            dll_at: host,
            replaced_dll: None,
            version: info.version.as_deref().and_then(clean_name),
            retired: Vec::new(),
            files: Vec::new(),
            dirs: Vec::new(),
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
        self.mods
            .iter()
            .chain(&self.packages)
            .map(|m| m.order)
            .max()
            .unwrap_or(0)
            .max(self.scan.record.next_order().saturating_sub(1))
            + 1
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
        let emf = self.scan.host.as_ref().is_some_and(|(_, i)| i.is_emf);
        let n = self.mods.len();
        let can_host: Vec<bool> = self
            .mods
            .iter()
            .map(|m| inspect(&self.dlls[&m.word], ctx.game).map(|i| i.can_host).unwrap_or(false))
            .collect();
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
        let host_i = if emf || n == 0 {
            None
        } else if n == 1 {
            Some(0)
        } else if package_host.is_some() {
            package_host
        } else if let Some(h) = current.filter(|&h| can_host[h]) {
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
                        if ctx.game.proxy_names.contains(&base.as_str()) || (ctx.in_system32)(&base) {
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
            let in_place = ctx.sha(t).as_deref() == Some(sha.as_str())
                && !(t.same(&host) && moved_from.iter().any(|m| m.same(&host)));
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

        // the chain file
        let mut chain = self.scan.chain.clone().unwrap_or_default();
        let before = chain.clone();
        let mut ours: Vec<RelPath> =
            self.scan.record.mods.iter().filter(|m| !is_package(m)).map(|m| m.dll_at.clone()).collect();
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
            let r = chain.add(&targets[g], ctx.game.host_dll, on);
            if r == AddResult::KeptOff {
                self.plan.notes.push(format!(
                    "{} is switched off, so it stays off. To switch it on, take the '; ' away from the start of its line in {}",
                    self.display(word),
                    ctx.game.chain_file
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
        let chain_rel = ctx.chain_rel();
        let mut created = self.scan.record.chain_created;
        if chain != before {
            if !chain.names_any_dll(ctx.game.host_dll) && (created || self.scan.chain.is_none()) {
                if self.scan.chain.is_some() {
                    self.plan.steps.push(Step::Delete(chain_rel.clone()));
                }
                self.plan.expect_files.push((chain_rel, None));
                created = false;
                chain = ChainFile::new();
            } else {
                if self.scan.chain.is_none() {
                    created = true;
                }
                let bytes = chain.encode();
                self.plan.expect_files.push((chain_rel.clone(), Some(sha256_hex(&bytes))));
                self.plan.steps.push(Step::Write(chain_rel, bytes));
            }
        }
        self.plan.expect_chain = expect_chain;

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
        for m in &self.scan.record.mods {
            for sha in std::iter::once(&m.dll_sha).chain(m.replaced_dll.iter()) {
                let r = store_rel(&m.word, sha);
                if !keep.iter().any(|k| k.same(&r)) && ctx.exists(&r) {
                    self.plan.steps.push(Step::Delete(r));
                }
            }
        }
        self.plan.record = Record {
            mods: self.mods.iter().chain(&self.packages).cloned().collect(),
            chain_created: created,
            chain: chain.parse(ctx.game.host_dll).entries.iter().map(|e| (e.name.clone(), e.on)).collect(),
        };
        Ok(())
    }

    /// A single mod dllchain did not install, back as dinput8.dll with no chain: nothing left to keep track of, so it
    /// leaves dllchain's list (the mods a package host runs from their own folders stay).
    fn forget_lone_host(&mut self) {
        let ctx = self.ctx;
        let chained: Vec<usize> =
            (0..self.plan.record.mods.len()).filter(|&i| !is_package(&self.plan.record.mods[i])).collect();
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

    /// Writes the record last (or plans to remove dllchain's folder when nothing is left).
    fn finish(mut self) -> Plan {
        let rel = RelPath::of(RECORD);
        if self.plan.record.is_empty() {
            if self.ctx.exists(&RelPath::of(OWN_DIR)) {
                self.plan.remove_own_dir = true;
                if !self.plan.steps.is_empty() {
                    self.plan.summary.push("dllchain's own folder is removed too: nothing is left to undo.".into());
                }
            }
        } else {
            let text = self.plan.record.to_text().into_bytes();
            if self.ctx.read(&rel).as_deref() != Some(text.as_slice()) {
                if self.plan.steps.is_empty() {
                    self.plan.summary.push("dllchain notes that another tool moved a mod.".into());
                }
                self.plan.expect_files.push((rel.clone(), Some(sha256_hex(&text))));
                self.plan.steps.push(Step::Write(rel, text));
            }
        }
        // renames first, then the files next to the game, then the mods' folders, each in name order
        let own = |r: &RelPath| r.first_part().eq_ignore_ascii_case(OWN_DIR);
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
                Step::Delete(r)
                    if !own(r)
                        && !steps[i + 1..]
                            .iter()
                            .any(|s| matches!(s, Step::Rename(_, b) | Step::Write(b, _) if b.same(r))) =>
                {
                    (1, r, format!("{r} (deleted)"))
                }
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
    if info.is_emf {
        fail!("{} is EMF; install EMF with its own install.cmd", src.label);
    }
    let word = match &info.word {
        Some((w, WordFrom::Known | WordFrom::VersionInfo)) => w.clone(),
        None => src.word_hint.clone(),
    };
    if !is_word(&word) {
        fail!("{}: could not make a file name from it", src.label);
    }
    let action = format!("install {} (SHA-256 {})", src.label, src.sha256);
    if let Some(ph) = package_host(ctx, &scan) {
        return plan_install_package(ctx, &scan, src, &info, &word, ph, action);
    }
    let mut w = Work::new(ctx, &scan, action)?;
    let replaced_now = w.adopt_host(Some(&word));
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
        None => ModRec {
            word: word.clone(),
            kind: Kind::Installed,
            order: w.next_order(),
            at: String::new(),
            source_sha: String::new(),
            label: String::new(),
            name: None,
            from_url: None,
            dll_sha: String::new(),
            dll_at: ctx.host(),
            replaced_dll: None,
            version: None,
            files: Vec::new(),
            retired: Vec::new(),
            dirs: Vec::new(),
        },
    };
    if replaced_now.is_some() {
        rec.replaced_dll = replaced_now;
    }
    fill_source(&mut rec, src, &info);

    let before = w.plan.summary.len();
    retire(&mut w, &mut rec, None, &src.files, None)?;
    plan_files(&mut w, &mut rec, &src.files, &src.label, src.files.is_empty(), &|_| false)?;
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
    }
    w.plan.summary.extend(file_lines);
    source_notes(&mut w, src);
    let files: Vec<&RelPath> = src.files.iter().map(|(r, _)| r).collect();
    w.plan.missing = missing(ctx, &scan, &files);
    Ok(w.finish())
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
        let end = (i..b.len()).find(|&j| !(b[j].is_ascii_digit() || b[j] == b'.')).unwrap_or(b.len());
        let num = stem[i..end].trim_end_matches('.');
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

/// The notes about the zip's files that stay out of the game folder.
fn source_notes(w: &mut Work, src: &ModSource) {
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
    let dir = package_dir(ph, word);
    let dll_rel = package_dll(ctx.game, ph, word);
    let (decl, off) = (RelPath::of(ph.declaration), RelPath::of(ph.off_switch));
    let prev = w.packages.iter().position(|m| m.word == word);
    let name = match prev {
        Some(i) => name_of(ctx.game, &w.packages[i]),
        None => default_name(info).unwrap_or_else(|| w.display(word)),
    };
    let dll_only = src.files.is_empty();
    match src.files.iter().find(|(r, _)| r.same(&decl)) {
        Some((_, b)) => check_declaration(b, ctx.game.host_dll).map_err(|e| Error(format!("{}: {e}", src.label)))?,
        // a new DLL alone, for a mod that is in its folder already
        None if dll_only && prev.is_some() => {}
        None => fail!(
            "{host} runs DLL mods only from a folder of their own in {}\\, and only when the mod says how, in a {decl} file. {} has none, so dllchain cannot install it with {host}. Add it with {host}'s DLL Manager instead.",
            ph.dir,
            src.label
        ),
    }
    // a loader that stops the game at a folder in mods\ without mod.json (EML), other than the package host's own
    for p in ctx.game.proxy_names {
        let rel = RelPath::of(&format!("{p}.dll"));
        let Some(b) = ctx.read(&rel) else { continue };
        if inspect(&b, ctx.game).ok().and_then(|i| i.word).is_some_and(|(w, _)| w == ph.word) {
            continue;
        }
        let strict = ctx.game.requirements.iter().find(|r| {
            r.dir.eq_ignore_ascii_case(ph.dir)
                && r.file.eq_ignore_ascii_case(ph.off_switch)
                && r.texts.iter().any(|t| contains_text(&b, t))
        });
        if let Some(r) = strict {
            fail!(
                "{rel} is {}, not {host}'s own. {} stops the game at a folder in {}\\ without {off}, and {host} keeps a folder with {off} in it switched off, so {name} cannot run with both. Put {host}'s own {rel} back (install {host} again), then install {name}.",
                r.short,
                r.short,
                ph.dir
            );
        }
    }

    let chained = w.mods.iter().position(|m| m.word == word);
    let mut rec = match (prev, chained) {
        (Some(i), _) => w.packages.remove(i),
        (None, Some(i)) => {
            let old = w.mods.remove(i);
            if old.kind == Kind::Adopted {
                fail!(
                    "{name} is in the game folder from before dllchain (as {}), where {host} does not load it. Take it out by hand, then install it again.",
                    old.dll_at
                );
            }
            if let Some(at) = locate(ctx, scan, &old) {
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
            ModRec { replaced_dll: None, ..old }
        }
        (None, None) => ModRec {
            word: word.to_string(),
            kind: Kind::Installed,
            order: w.next_order(),
            at: String::new(),
            source_sha: String::new(),
            label: String::new(),
            name: None,
            from_url: None,
            dll_sha: String::new(),
            dll_at: dll_rel.clone(),
            replaced_dll: None,
            version: None,
            files: Vec::new(),
            retired: Vec::new(),
            dirs: Vec::new(),
        },
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

    let mut files = vec![(dll_rel, src.dll.clone())];
    for (r, b) in &src.files {
        if !r.same(&off) {
            files.push((dir.join(r.as_str())?, b.clone()));
        }
    }
    retire(&mut w, &mut rec, Some(&dir), &files, Some((&dir.join(ph.off_switch)?, &host)))?;
    plan_files(&mut w, &mut rec, &files, &src.label, dll_only, &|r| r.strip_prefix(&dir).is_some())?;
    if chained.is_some() {
        for l in w.plan.summary.iter_mut().filter(|l| l.starts_with("Deletes ")) {
            *l = l.replace(", which the new version does not have", ", which it had next to the game");
            *l = l.replace(" the new version does not have", " it had next to the game");
        }
    }
    w.packages.push(rec);
    w.layout()?;
    w.forget_lone_host();
    if src.files.iter().any(|(r, _)| r.same(&off)) {
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
        let backup = RelPath::parse(&format!("{OWN_DIR}\\originals\\{}\\{rel}", rec.word))?;
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

/// The mod's other files: created, updated, kept when the player changed them, and the old version's leftovers.
/// `earlier(rel)`: a file there now that dllchain did not install is the same mod's, from a copy put in by hand (it is
/// kept and put back on uninstall like any file replaced, but asks for no leave to replace).
fn plan_files(
    w: &mut Work,
    rec: &mut ModRec,
    src_files: &[(RelPath, Vec<u8>)],
    label: &str,
    dll_only: bool,
    earlier: &dyn Fn(&RelPath) -> bool,
) -> Result<()> {
    let ctx = w.ctx;
    let prev_files = rec.files.clone();
    let mut files: Vec<FileRec> = Vec::new();
    let mut dirs = rec.dirs.clone();
    let mut by_hand: Vec<RelPath> = Vec::new();
    let others: Vec<&ModRec> = w.mods.iter().chain(&w.packages).filter(|m| m.word != rec.word).collect();
    for (rel, bytes) in src_files {
        check_target(rel, ctx.game)?;
        if others.iter().any(|m| m.files.iter().any(|f| f.rel.same(rel)) || m.dll_at.same(rel)) || rel.same(&ctx.host())
        {
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
            (Some(n), p) if n == sha => {
                if let Some(p) = p {
                    files.push(FileRec { rel: rel.clone(), sha, backup: p.backup.clone() });
                }
            }
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
                let backup = RelPath::parse(&format!("{OWN_DIR}\\originals\\{}\\{rel}", rec.word))?;
                let Some(old) = ctx.read(rel) else { fail!("could not read {rel}") };
                debug_assert_eq!(sha256_hex(&old), n);
                if earlier(rel) {
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
        1 => w.plan.summary.push(format!("Deletes {}, which the new version does not have.", deleted[0])),
        k => w.plan.summary.push(format!("Deletes {k} files the new version does not have{}.", most_in(&deleted))),
    }
    undone.words(w);
    rec.files = files;
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

/// ", most in mods\auto-loot-plus\" for a list of files, or nothing when they are spread out.
fn most_in(rels: &[&RelPath]) -> String {
    match places(rels).first() {
        Some((p, n)) if !p.is_empty() && *n == rels.len() => format!(", all in {p}"),
        Some((p, n)) if !p.is_empty() && *n * 2 > rels.len() => format!(", most in {p}"),
        _ => String::new(),
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
    for m in w.mods.iter().chain(&w.packages) {
        let missing: Vec<&RelPath> = m.files.iter().map(|f| &f.rel).filter(|r| !ctx.exists(r)).collect();
        let name = name_of(ctx.game, m);
        match missing.len() {
            0 => {}
            1 => w.plan.notes.push(format!("{} of {name} is missing. Install it again to put it back", missing[0])),
            k => w.plan.notes.push(format!("{k} files of {name} are missing. Install it again to put them back")),
        }
    }
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
                let Some(bytes) = ctx.read(copy).filter(|b| sha256_hex(b) == *sha) else {
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
        if !c.rel.first_part().eq_ignore_ascii_case(OWN_DIR) {
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

/// Uninstall one mod (by its word) or, with `None`, everything dllchain installed.
pub fn plan_uninstall(ctx: &Ctx, word: Option<&str>) -> Result<Plan> {
    let scan = scan(ctx)?;
    let mut w = Work::new(ctx, &scan, format!("uninstall {}", word.unwrap_or("everything")))?;
    w.plan.first_install = false;
    w.adopt_host(None);
    let targets: Vec<String> = match word {
        Some(x) if w.packages.iter().any(|m| m.word == x) => vec![x.to_string()],
        Some(x) => match w.mods.iter().find(|m| m.word == x) {
            None => fail!("{x} is not on dllchain's list"),
            Some(m) if m.kind == Kind::Adopted => fail!(
                "dllchain did not install {}; it was already {} (or copied over later). dllchain keeps it in the chain and never deletes it.",
                ctx.game.display(x),
                ctx.game.host_dll
            ),
            Some(m) => vec![m.word.clone()],
        },
        None => {
            w.mods.iter().chain(&w.packages).filter(|m| m.kind == Kind::Installed).map(|m| m.word.clone()).collect()
        }
    };
    w.allow_no_host = word.is_none();
    for t in &targets {
        let m = match w.packages.iter().position(|m| &m.word == t) {
            Some(i) => w.packages.remove(i),
            None => w.mods.remove(w.mods.iter().position(|m| &m.word == t).expect("listed")),
        };
        let display = name_of(ctx.game, &m);
        if word.is_some() {
            w.plan.name = display.clone();
        }
        // a package's DLL is one of its files
        let at = if is_package(&m) { None } else { locate(ctx, &scan, &m) };
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
        undone.words(&mut w);
        if let Some(at) = at {
            w.drop.push((at, m.dll_sha.clone(), m.word.clone()));
        }
        let mut dirs = m.dirs.clone();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.as_str().matches('\\').count()));
        for d in dirs {
            w.plan.steps.push(Step::RemoveDirIfEmpty(d));
        }
        if let Some(r) = &m.replaced_dll {
            // the copy that was there before this install comes back
            let bytes = ctx.read(&store_rel(t, r)).filter(|b| sha256_hex(b) == *r);
            match bytes {
                Some(b) => {
                    w.plan.summary.push(format!("Your earlier copy of {display} is put back."));
                    w.dlls.insert(t.clone(), b);
                    w.mods.push(ModRec {
                        word: t.clone(),
                        kind: Kind::Adopted,
                        order: m.order,
                        at: m.at.clone(),
                        source_sha: "-".into(),
                        label: format!("found as {}", ctx.game.host_dll),
                        name: m.name.clone(),
                        from_url: None,
                        dll_sha: r.clone(),
                        dll_at: ctx.host(),
                        replaced_dll: None,
                        version: None,
                        retired: Vec::new(),
                        files: Vec::new(),
                        dirs: Vec::new(),
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

/// Puts back the files an install took out of the mod's folder (an older version's leftovers, a package host's off
/// switch), unless something is in their place again.
fn put_back_retired(w: &mut Work, m: &ModRec) {
    let ctx = w.ctx;
    for f in &m.retired {
        let Some(backup) = &f.backup else { continue };
        let Some(bytes) = ctx.read(backup).filter(|b| sha256_hex(b) == f.sha) else {
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
    let chain = ctx.read(&ctx.chain_rel()).map(|b| ChainFile::decode(&b)).unwrap_or_default();
    let parsed = chain.parse(ctx.game.host_dll);
    for (name, on) in &plan.expect_chain {
        match parsed.find(name) {
            Some(e) if e.on == *on => {}
            _ => {
                fail!("the check after the change found the line for {name} in {} not as planned", ctx.game.chain_file)
            }
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
    let log_rel = match dll.as_str().rsplit_once('\\') {
        Some((dir, _)) => RelPath::parse(&format!("{dir}\\{}", log.file)).ok()?,
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
    let row = |rel: &RelPath, role: String| -> Row {
        let sha = ctx.sha(rel);
        let pair = |label: &str, value: String| (label.to_string(), value);
        let mut details = vec![pair("File", format!("{rel} ({role})"))];
        let rebuilt =
            |m: &ModRec| m.dll_at.same(rel) && scan.rebuilt(&m.word).is_some_and(|r| Some(&r.2.sha256) == sha.as_ref());
        if let Some(m) = scan.record.mods.iter().find(|m| Some(&m.dll_sha) == sha.as_ref() || rebuilt(m)) {
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
            let dll_version = ctx.read(rel).and_then(|b| inspect(&b, ctx.game).ok()).and_then(|i| i.version);
            let version = m.version.clone().or_else(|| dll_version.clone());
            // only the DLL's own version says which copy should have run; one from a zip's name is a guess
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
        if rel.same(&host) && scan.record.is_empty() {
            details.push(pair("", "dllchain leaves it as it is.".into()));
            match package_host {
                Some(ph) => details
                    .push(pair("", format!("A mod you install goes into {}\\<mod>\\, where {name} loads it.", ph.dir))),
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
    if ctx.exists(&host) {
        let can = scan.host.as_ref().map(|(_, i)| i.can_host).unwrap_or(false);
        let role = if can { "game loads it; starts others" } else { "game loads it; starts no others" };
        st.rows.push(row(&host, role.into()));
    }
    for e in &scan.parsed.entries {
        let role = match (e.on, ctx.exists(&e.name)) {
            (true, true) => format!("started by {}", ctx.game.host_dll),
            (false, _) => "switched off".to_string(),
            (true, false) => "listed, but missing".to_string(),
        };
        st.rows.push(row(&e.name, role));
    }
    for p in ctx.game.proxy_names {
        let rel = RelPath::of(&format!("{p}.dll"));
        if rel.same(&host) || !ctx.exists(&rel) {
            continue;
        }
        let mut r = row(&rel, "another loader's DLL, left alone".into());
        r.source = "not installed by dllchain".into();
        st.rows.push(r);
    }
    let ph = package_host(ctx, &scan);
    st.package_host = ph.map(|p| (ctx.game.display(p.word), p.dir.to_string()));
    if let Some(ph) = ph {
        // an older dllchain chained mods with the package host, which runs none of them
        let host = ctx.game.display(ph.word);
        for m in scan.record.mods.iter().filter(|m| !is_package(m) && m.word != ph.word) {
            st.notes.push(format!(
                "{} does not run: {host} loads no DLL next to the game. Install it again, and dllchain moves it into {}, where {host} loads it.",
                name_of(ctx.game, m),
                package_dir(ph, &m.word)
            ));
        }
    }
    let loader = st.package_host.as_ref().map_or("its loader", |p| p.0.as_str()).to_string();
    for m in scan.record.mods.iter().filter(|m| is_package(m)) {
        st.rows.push(row(&m.dll_at, format!("loaded by {loader} from its own folder")));
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
