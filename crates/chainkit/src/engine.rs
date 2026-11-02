//! Install, repair and uninstall, each in two halves: a [`Plan`] that says what would change (and changes nothing),
//! and [`apply`], which makes the changes in one undoable run and checks the folder afterwards.
//!
//! Who hosts: only the DLL called `dinput8.dll` runs, and it starts the others from `dinput8-chain.ini`. With EMF's
//! loader as `dinput8.dll`, every mod is a guest. Otherwise the host is the mod that is `dinput8.dll` now when it can
//! host, else the newest mod that can; a mod that cannot read the chain file is only ever the host when it is alone.
//! So the result is the same whatever order mods come in.

use crate::chainini::{AddResult, ChainFile, Parsed};
use crate::dllinfo::{DllInfo, WordFrom, inspect};
use crate::game::Game;
use crate::hash::{sha256_hex, short};
use crate::paths::{RelPath, is_word, resolve};
use crate::record::{FileRec, Kind, ModRec, RECORD, Record, clean_name, store_rel};
use crate::source::{ModSource, check_target};
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
    Ok(Scan { record, notes, chain, parsed, host })
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
        if !dll_ok {
            notes.push(format!("record: {} would have its DLL at {at}, which dllchain never uses; left out", m.word));
            return false;
        }
        m.files.retain(|f| {
            let ok =
                check_target(&f.rel, game).is_ok() && f.backup.as_ref().is_none_or(|b| b.key().starts_with(&originals));
            if !ok {
                notes.push(format!("record: {}'s file {} is not one dllchain installs; left out", m.word, f.rel));
            }
            ok
        });
        true
    });
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
    mods: Vec<ModRec>,
    dlls: HashMap<String, Vec<u8>>,
    plan: Plan,
    /// DLL files to delete before anything moves: (where, its SHA-256).
    drop: Vec<(RelPath, String)>,
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
        match self.mods.iter().find(|m| m.word == word) {
            Some(m) => name_of(self.ctx.game, m),
            None => self.ctx.game.display(word),
        }
    }

    fn unique_word(&self, word: &str) -> String {
        let mut w = word.to_string();
        let mut n = 2;
        while self.mods.iter().any(|m| m.word == w) {
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
        if word.is_some() && word.as_deref() == installing {
            self.plan.summary.push(format!(
                "{host} is an earlier copy of {}: the new one takes its place (dllchain keeps the old one, and Uninstall puts it back).",
                self.display(installing.unwrap_or_default())
            ));
            self.drop.push((host, info.sha256.clone()));
            self.write_store(installing.unwrap_or_default(), bytes);
            return Some(info.sha256.clone());
        }
        if let Some(i) = word.as_ref().and_then(|w| self.mods.iter().position(|m| &m.word == w)) {
            let old = self.mods[i].clone();
            if let Some(at) = locate(self.ctx, self.scan, &old) {
                if !at.same(&host) {
                    self.drop.push((at.clone(), old.dll_sha.clone()));
                }
            }
            self.plan.summary.push(format!(
                "{host} is a newer copy of {}: dllchain uses it from now on, in place of the one it had (SHA-256 {}, was {}).",
                self.display(&old.word),
                short(&info.sha256),
                short(&old.dll_sha)
            ));
            let m = &mut self.mods[i];
            m.dll_sha = info.sha256.clone();
            m.dll_at = host;
            self.dlls.insert(old.word.clone(), bytes.clone());
            return None;
        }
        let w = self.unique_word(&word.unwrap_or_else(|| "dinput8-previous".into()));
        let name = default_name(info).unwrap_or_else(|| self.display(&w));
        self.plan.summary.push(format!("{name} is already {host}: dllchain keeps a copy of it and never deletes it."));
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
            files: Vec::new(),
            dirs: Vec::new(),
        });
        self.dlls.insert(w, bytes.clone());
        None
    }

    fn next_order(&self) -> u32 {
        self.mods.iter().map(|m| m.order).max().unwrap_or(0).max(self.scan.record.next_order().saturating_sub(1)) + 1
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
        let dropping = |r: &RelPath, drop: &[(RelPath, String)]| drop.iter().any(|(d, _)| d.same(r));
        let holder =
            (0..n).find(|&i| located[i].as_ref().is_some_and(|l| l.same(&host)) && !dropping(&host, &self.drop));

        let host_i = if emf || n == 0 {
            None
        } else if n == 1 {
            Some(0)
        } else if let Some(h) = holder.filter(|&h| can_host[h]) {
            Some(h)
        } else if let Some(h) = (0..n).filter(|&i| can_host[i]).max_by_key(|&i| self.mods[i].order) {
            Some(h)
        } else if self.allow_no_host {
            let h = (0..n).max_by_key(|&i| self.mods[i].order).unwrap_or(0);
            self.plan.notes.push(format!(
                "None of {} can start the others, so only {} runs; the others stay renamed (not deleted).",
                self.mods.iter().map(|m| self.display(&m.word)).collect::<Vec<_>>().join(", "),
                self.display(&self.mods[h].word)
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
        // who starts the guests, and whether the host has any to start
        let starter = match host_i {
            Some(h) => self.display(&self.mods[h].word),
            None => "EMF".to_string(),
        };
        let starts = if n > 1 && host_i.is_some_and(|h| can_host[h]) { ", and it starts your other mods" } else { "" };
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
                        "{} cannot read {}, so the DLLs listed there by others do not start: {}",
                        self.display(&self.mods[h].word),
                        ctx.game.chain_file,
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
                        || self
                            .scan
                            .parsed
                            .entries
                            .iter()
                            .any(|e| e.name.same(r) && ctx.sha(r).as_deref() != Some(sha.as_str()))
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
        for (r, sha) in self.drop.clone() {
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
                    self.plan
                        .summary
                        .push(format!("{w} is renamed to {} and keeps working, started by {starter}.", targets[h]));
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
                self.plan.summary.push(if t.same(&host) {
                    format!("{w} becomes {host} (it was {l}). The game loads it{starts}.")
                } else {
                    format!("{w} is renamed from {l} to {t} and keeps working, started by {starter}.")
                });
            } else {
                self.plan.steps.push(Step::Write(t.clone(), self.dlls[&self.mods[i].word].clone()));
                let was_here = self.scan.record.mods.iter().any(|m| m.word == self.mods[i].word && m.dll_sha == sha);
                self.plan.summary.push(match (t.same(&host), was_here) {
                    (true, false) => format!("{w} becomes {host}. The game loads it{starts}."),
                    (false, false) => format!("{w} goes in as {t}, started by {starter}."),
                    (_, true) => format!("{w} comes back as {t}, from dllchain's copy."),
                });
            }
            self.plan.expect_files.push((t.clone(), Some(sha)));
        }

        // the chain file
        let mut chain = self.scan.chain.clone().unwrap_or_default();
        let before = chain.clone();
        let mut ours: Vec<RelPath> = self.scan.record.mods.iter().map(|m| m.dll_at.clone()).collect();
        ours.extend(located.iter().flatten().cloned());
        ours.extend(self.drop.iter().map(|d| d.0.clone()));
        let guests: Vec<usize> = (0..n).filter(|&i| !targets[i].same(&host)).collect();
        let mut taken_out = Vec::new();
        for r in &ours {
            if !r.same(&host) && !guests.iter().any(|&g| targets[g].same(r)) && chain.remove(r) {
                taken_out.push(self.plan.summary.len());
                self.plan.summary.push(format!("{r} is taken out of {}.", ctx.game.chain_file));
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
                    "{} ({}) is switched off in {} ('; ' in front of its line), so it stays off; take the '; ' away to switch it on",
                    self.display(word),
                    targets[g],
                    ctx.game.chain_file
                ));
            }
            if r == AddResult::SwitchedOn {
                self.plan.summary.push(format!("{} is switched on again in {}.", targets[g], ctx.game.chain_file));
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
                    // the whole file goes, so its lines need no words of their own
                    for i in taken_out.iter().rev() {
                        self.plan.summary.remove(*i);
                    }
                    self.plan.summary.push(format!(
                        "{} is deleted: dllchain made it, and no DLL is left in it.",
                        ctx.game.chain_file
                    ));
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
            mods: self.mods.clone(),
            chain_created: created,
            chain: chain.parse(ctx.game.host_dll).entries.iter().map(|e| (e.name.clone(), e.on)).collect(),
        };
        Ok(())
    }

    /// Writes the record last (or plans to remove dllchain's folder when nothing is left).
    fn finish(mut self) -> Plan {
        let rel = RelPath::of(RECORD);
        if self.plan.record.is_empty() {
            if self.ctx.exists(&RelPath::of(OWN_DIR)) {
                self.plan.remove_own_dir = true;
                if !self.plan.steps.is_empty() {
                    self.plan.summary.push("dllchain's own folder is removed: nothing is left to undo.".into());
                }
            }
        } else {
            let text = self.plan.record.to_text().into_bytes();
            if self.ctx.read(&rel).as_deref() != Some(text.as_slice()) {
                if self.plan.steps.is_empty() {
                    self.plan.summary.push("dllchain's list is brought up to date: another tool moved a DLL.".into());
                }
                self.plan.expect_files.push((rel.clone(), Some(sha256_hex(&text))));
                self.plan.steps.push(Step::Write(rel, text));
            }
        }
        // renames first, then the files next to the game, then the mods' folders
        let own = |r: &RelPath| r.first_part().eq_ignore_ascii_case(OWN_DIR);
        let mut gone: Vec<&RelPath> = Vec::new();
        let mut lines: Vec<(u8, String)> = Vec::new();
        for s in &self.plan.steps {
            let line = match s {
                Step::Write(r, _) if !own(r) => {
                    let there = self.ctx.exists(r) && !gone.iter().any(|g| g.same(r));
                    (1, format!("{r} ({})", if there { "replaced" } else { "new" }))
                }
                Step::Rename(a, b) => {
                    gone.push(a);
                    (0, format!("{a} renamed to {b}"))
                }
                Step::Delete(r) if !own(r) => {
                    gone.push(r);
                    (1, format!("{r} (deleted)"))
                }
                _ => continue,
            };
            let top = !line.1.split(' ').next().unwrap_or_default().contains('\\');
            lines.push(if line.0 == 1 && !top { (2, line.1) } else { line });
        }
        lines.sort_by_key(|(k, _)| *k);
        self.plan.files = lines.into_iter().map(|(_, l)| l).collect();
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
    let mut w = Work::new(ctx, &scan, format!("install {} (SHA-256 {})", src.label, src.sha256))?;
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
                w.plan.notes.push(format!("{display} with this DLL is already installed; its files are checked again"));
            } else {
                w.plan.summary.push(format!(
                    "{display} is updated to the new version (DLL SHA-256 {}, was {}).",
                    short(&info.sha256),
                    short(&old.dll_sha)
                ));
                if let Some(at) = locate(ctx, &scan, &old) {
                    w.drop.push((at, old.dll_sha.clone()));
                }
            }
            let mut r = old.clone();
            if old.kind == Kind::Adopted {
                // installed by hand before; it comes back on uninstall
                r.replaced_dll = Some(old.dll_sha.clone());
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
            files: Vec::new(),
            dirs: Vec::new(),
        },
    };
    if replaced_now.is_some() {
        rec.replaced_dll = replaced_now;
    }
    rec.at = clock::now_text();
    rec.source_sha = src.sha256.clone();
    rec.label = src.label.clone();
    rec.from_url = src.from_url.clone();
    if rec.name.is_none() {
        rec.name = default_name(&info);
    }
    rec.dll_sha = info.sha256.clone();

    let before = w.plan.summary.len();
    plan_files(&mut w, &mut rec, src)?;
    if let Some(i) = prev {
        w.mods[i] = rec;
    } else {
        w.mods.push(rec);
    }
    w.dlls.insert(word, src.dll.clone());
    // the DLLs first, then the mod's other files
    let file_lines = w.plan.summary.split_off(before);
    w.layout()?;
    w.plan.summary.extend(file_lines);
    for i in &src.ignored {
        w.plan.notes.push(format!("{i} in the zip is outside the mod's folder and is not installed"));
    }
    Ok(w.finish())
}

/// The mod's other files: created, updated, kept when the player changed them, and the old version's leftovers.
fn plan_files(w: &mut Work, rec: &mut ModRec, src: &ModSource) -> Result<()> {
    let ctx = w.ctx;
    let prev_files = rec.files.clone();
    let mut files: Vec<FileRec> = Vec::new();
    let mut dirs = rec.dirs.clone();
    let others: Vec<&ModRec> = w.mods.iter().filter(|m| m.word != rec.word).collect();
    for (rel, bytes) in &src.files {
        check_target(rel, ctx.game)?;
        if others.iter().any(|m| m.files.iter().any(|f| f.rel.same(rel)) || m.dll_at.same(rel)) || rel.same(&ctx.host())
        {
            fail!("{}: {rel} belongs to another mod on dllchain's list", src.label);
        }
        resolve(&ctx.dir, rel)?;
        let sha = sha256_hex(bytes);
        let now = ctx.sha(rel);
        if ctx.exists(rel) && now.is_none() {
            fail!("{}: {rel} is a folder in the game folder, not a file", src.label);
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
                w.plan.notes.push(format!(
                    "{rel} was changed since it was installed, so yours stays; the new version is {new}"
                ));
                files.push(p.clone());
                files.push(FileRec { rel: new, sha, backup: None });
            }
            (Some(n), None) => {
                let backup = RelPath::parse(&format!("{OWN_DIR}\\originals\\{}\\{rel}", rec.word))?;
                let Some(old) = ctx.read(rel) else { fail!("could not read {rel}") };
                debug_assert_eq!(sha256_hex(&old), n);
                w.plan.replaces.push(rel.clone());
                w.plan.steps.push(Step::Write(backup.clone(), old));
                w.plan.steps.push(Step::Write(rel.clone(), bytes.clone()));
                w.plan.expect_files.push((rel.clone(), Some(sha.clone())));
                files.push(FileRec { rel: rel.clone(), sha, backup: Some(backup) });
            }
        }
    }
    let new_count = files.len();
    let mut gone = 0;
    for p in &prev_files {
        if files.iter().any(|f| f.rel.same(&p.rel)) {
            continue;
        }
        gone += undo_file(w, p, &mut files) as usize;
    }
    match gone {
        0 => {}
        1 => w.plan.summary.push("1 file of the old version, which the new one does not have, is deleted.".into()),
        k => {
            w.plan.summary.push(format!("{k} files of the old version, which the new one does not have, are deleted."))
        }
    }
    let created: Vec<&RelPath> = files[..new_count.min(files.len())]
        .iter()
        .filter(|f| !prev_files.iter().any(|p| p.rel.same(&f.rel)))
        .map(|f| &f.rel)
        .collect();
    match created.len() {
        0 => {}
        1 => w.plan.summary.push(format!("1 more file goes into the game folder: {}.", created[0])),
        k => w.plan.summary.push(format!("{k} more files go into the game folder{}.", most_in(&created))),
    }
    match w.plan.replaces.len() {
        0 => {}
        1 => w.plan.summary.push(format!(
            "{} is replaced: dllchain did not install it, so it keeps the original, and Uninstall puts it back.",
            w.plan.replaces[0]
        )),
        k => w.plan.summary.push(format!(
            "{k} files dllchain did not install are replaced; dllchain keeps the originals, and Uninstall puts them back: {}.",
            w.plan.replaces.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
        )),
    }
    rec.files = files;
    dirs.sort_by_key(|d| d.key());
    dirs.dedup_by(|a, b| a.same(b));
    rec.dirs = dirs;
    Ok(())
}

/// Takes one of a mod's files away again: deleted while it is as dllchain left it, or the original put back; a file
/// the player changed stays (and stays on the list when `keep_changed` is given). True when it is deleted, for the
/// caller to count.
fn undo_file(w: &mut Work, f: &FileRec, keep_changed: &mut Vec<FileRec>) -> bool {
    let ctx = w.ctx;
    let now = ctx.sha(&f.rel);
    let original = f.backup.as_ref().and_then(|b| ctx.read(b).map(|bytes| (b.clone(), bytes)));
    match (now.as_deref() == Some(f.sha.as_str()) || now.is_none(), original) {
        (true, Some((b, bytes))) => {
            w.plan.expect_files.push((f.rel.clone(), Some(sha256_hex(&bytes))));
            w.plan.steps.push(Step::Write(f.rel.clone(), bytes));
            w.plan.steps.push(Step::Delete(b));
            w.plan.summary.push(format!("Your original {} is put back.", f.rel));
        }
        (true, None) => {
            if now.is_some() {
                w.plan.steps.push(Step::Delete(f.rel.clone()));
                w.plan.expect_files.push((f.rel.clone(), None));
                return true;
            }
        }
        (false, original) => {
            let extra = original.map_or(String::new(), |(b, _)| format!("; the original is kept in {b}"));
            w.plan.notes.push(format!("{} was changed since dllchain installed it, so it stays{extra}", f.rel));
            if f.backup.is_some() {
                keep_changed.push(f.clone());
            }
        }
    }
    false
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

/// ", most of them in mods\auto-loot-plus\" for a list of files, or nothing when they are spread out.
fn most_in(rels: &[&RelPath]) -> String {
    match places(rels).first() {
        Some((p, n)) if !p.is_empty() && *n == rels.len() => format!(", all in {p}"),
        Some((p, n)) if !p.is_empty() && *n * 2 > rels.len() => format!(", most of them in {p}"),
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
    for m in &w.mods {
        for f in &m.files {
            if !ctx.exists(&f.rel) {
                w.plan.notes.push(format!(
                    "{} of {} is missing; install the mod again to put it back",
                    f.rel,
                    ctx.game.display(&m.word)
                ));
            }
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
    plan.summary.push(format!("{old} is now called {now} in dllchain's list (no file is renamed)."));
    let rel = RelPath::of(RECORD);
    let text = record.to_text().into_bytes();
    plan.expect_files.push((rel.clone(), Some(sha256_hex(&text))));
    plan.steps.push(Step::Write(rel, text));
    plan.record = record;
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
        Some(x) => match w.mods.iter().find(|m| m.word == x) {
            None => fail!("{x} is not on dllchain's list"),
            Some(m) if m.kind == Kind::Adopted => fail!(
                "dllchain did not install {}; it was already {} (or copied over later). dllchain keeps it in the chain and never deletes it.",
                ctx.game.display(x),
                ctx.game.host_dll
            ),
            Some(m) => vec![m.word.clone()],
        },
        None => w.mods.iter().filter(|m| m.kind == Kind::Installed).map(|m| m.word.clone()).collect(),
    };
    w.allow_no_host = word.is_none();
    for t in &targets {
        let i = w.mods.iter().position(|m| &m.word == t).expect("listed");
        let m = w.mods.remove(i);
        let display = name_of(ctx.game, &m);
        if word.is_some() {
            w.plan.name = display.clone();
        }
        let at = locate(ctx, &scan, &m);
        let line = w.plan.summary.len();
        let mut kept = Vec::new();
        let mut gone = 0;
        for f in m.files.iter().rev() {
            gone += undo_file(&mut w, f, &mut kept) as usize;
        }
        let what = match (&at, gone) {
            (Some(a), 0) => format!("its {a} is deleted"),
            (Some(a), 1) => format!("its {a} and 1 more file are deleted"),
            (Some(a), k) => format!("its {a} and {k} more files are deleted"),
            (None, 0) => "its DLL is already gone".to_string(),
            (None, 1) => "1 file of it is deleted".to_string(),
            (None, k) => format!("{k} files of it are deleted"),
        };
        w.plan.summary.insert(line, format!("{display} is uninstalled: {what}."));
        if let Some(at) = at {
            w.drop.push((at, m.dll_sha.clone()));
        }
        if !kept.is_empty() {
            w.plan.notes.push(format!(
                "{display}: {} changed file(s) stay, with their originals in dllchain's folder",
                kept.len()
            ));
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
                    w.plan.summary.push(format!("The {display} that was {} before comes back.", ctx.game.host_dll));
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
    // a single mod dllchain did not install, back as dinput8.dll and unlisted: nothing left to keep track of
    if w.plan.record.mods.len() == 1
        && w.plan.record.mods[0].kind == Kind::Adopted
        && w.plan.record.mods[0].dll_at.same(&ctx.host())
    {
        let m = w.plan.record.mods.remove(0);
        let r = store_rel(&m.word, &m.dll_sha);
        if ctx.exists(&r) || w.plan.steps.iter().any(|s| matches!(s, Step::Write(x, _) if x.same(&r))) {
            w.plan.steps.retain(|s| !matches!(s, Step::Write(x, _) if x.same(&r)));
            if ctx.exists(&r) {
                w.plan.steps.push(Step::Delete(r));
            }
        }
        w.plan.record.chain.clear();
    }
    Ok(w.finish())
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
    let mut t = Txn::begin(&ctx.dir, &id)?;
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
}

#[derive(Clone, Debug, Default)]
pub struct Status {
    pub rows: Vec<Row>,
    pub notes: Vec<String>,
    /// What a repair would change (empty: nothing to repair).
    pub repair: Vec<String>,
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
        if let Some(m) = scan.record.mods.iter().find(|m| Some(&m.dll_sha) == sha.as_ref()) {
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
            details.push(pair("SHA-256", m.dll_sha.clone()));
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
        if rel.same(&host) && scan.record.is_empty() {
            details.push(pair("", "dllchain leaves it as it is. When you install another mod,".into()));
            details.push(pair("", "it joins the chain and dllchain keeps a copy of it.".into()));
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
