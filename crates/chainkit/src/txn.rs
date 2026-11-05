//! A run's changes, undoable as one: before a file changes, a copy of it (or the fact that it was not there) and what
//! dllchain is about to write are saved in `dllchain\journal.txt`. A failed run is rolled back from it at once; a run
//! that was cut off (power loss, a crash) is rolled back the next time dllchain starts.
//!
//! A file is only put back while it still holds what dllchain wrote (or nothing): a change someone else made in the
//! meantime is left alone and reported.

use crate::OWN_DIR;
use crate::hash::{file_sha256, is_sha256, sha256_hex};
use crate::paths::{RelPath, resolve};
use crate::{Error, Result, fail};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const JOURNAL: &str = "dllchain\\journal.txt";
/// The journal of the last run that finished, kept with its copies so the run can be undone.
pub const UNDO: &str = "dllchain\\undo.txt";
const HEADER: &str = "dllchain-journal\t1";
const TMP: &str = ".dllchain-tmp";

#[derive(Clone, Debug)]
struct Touched {
    rel: RelPath,
    /// A copy of what was there and its SHA-256, or `None` when nothing was.
    before: Option<(RelPath, String)>,
    /// What dllchain meant to leave there, each time it changed the file (`None`: deleted).
    intended: Vec<Option<String>>,
}

pub struct Txn {
    game: PathBuf,
    id: String,
    /// What the run is, in words ("install x.zip"), and when it began.
    action: String,
    at: String,
    touched: Vec<Touched>,
    made_dirs: Vec<RelPath>,
    removed_dirs: Vec<RelPath>,
    /// One line per change, for the log.
    pub log: Vec<String>,
}

impl Txn {
    pub fn begin(game: &Path, id: &str, action: &str) -> Result<Txn> {
        if resolve(game, &RelPath::of(JOURNAL))?.exists() {
            fail!("an earlier dllchain run was not finished; it must be undone first");
        }
        let t = Txn {
            game: game.to_path_buf(),
            id: id.to_string(),
            action: crate::record::clean(action),
            at: crate::clock::now_text(),
            touched: Vec::new(),
            made_dirs: Vec::new(),
            removed_dirs: Vec::new(),
            log: Vec::new(),
        };
        t.make_dirs(&RelPath::of(&format!("{OWN_DIR}\\backup\\{id}\\x")))?;
        t.persist()?;
        Ok(t)
    }

    pub fn backup_dir(&self) -> RelPath {
        backup_dir(&self.id)
    }

    fn path(&self, rel: &RelPath) -> Result<PathBuf> {
        resolve(&self.game, rel)
    }

    fn persist(&self) -> Result<()> {
        let mut lines = vec![format!("{HEADER}\t{}", self.id), format!("action\t{}\t{}", self.at, self.action)];
        for t in &self.touched {
            let b = t.before.as_ref().map_or("-\t-".to_string(), |(b, s)| format!("{b}\t{s}"));
            lines.push(format!("touch\t{}\t{b}", t.rel));
            for i in &t.intended {
                lines.push(format!("intend\t{}\t{}", t.rel, i.as_deref().unwrap_or("-")));
            }
        }
        for d in &self.made_dirs {
            lines.push(format!("made-dir\t{d}"));
        }
        for d in &self.removed_dirs {
            lines.push(format!("removed-dir\t{d}"));
        }
        let bytes = (lines.join("\r\n") + "\r\n").into_bytes();
        write_atomic(&self.path(&RelPath::of(JOURNAL))?, &bytes)
    }

    /// Creates the folders above `rel` that are missing, and returns them.
    fn make_dirs(&self, rel: &RelPath) -> Result<Vec<RelPath>> {
        let mut made = Vec::new();
        for d in rel.ancestors() {
            let p = self.path(&d)?;
            if !p.exists() {
                std::fs::create_dir(&p).map_err(|e| Error(format!("could not create the folder {d}: {e}")))?;
                made.push(d);
            } else if !p.is_dir() {
                fail!("{d} is a file, so dllchain cannot make a folder there");
            }
        }
        Ok(made)
    }

    fn touch(&mut self, rel: &RelPath) -> Result<usize> {
        if let Some(i) = self.touched.iter().position(|t| t.rel.same(rel)) {
            return Ok(i);
        }
        let p = self.path(rel)?;
        let before = if p.is_file() {
            let bytes = std::fs::read(&p).map_err(|e| Error(format!("could not read {rel}: {e}")))?;
            let b = self.backup_dir().join(rel.as_str())?;
            let bp = self.path(&b)?;
            self.make_dirs(&b)?;
            write_atomic(&bp, &bytes)?;
            Some((b, sha256_hex(&bytes)))
        } else if p.exists() {
            fail!("{rel} is a folder, not a file");
        } else {
            None
        };
        self.touched.push(Touched { rel: rel.clone(), before, intended: Vec::new() });
        Ok(self.touched.len() - 1)
    }

    fn intend(&mut self, i: usize, sha: Option<String>) -> Result<()> {
        self.touched[i].intended.push(sha);
        self.persist()
    }

    fn track_dirs(&mut self, rel: &RelPath) -> Result<()> {
        let made = self.make_dirs(rel)?;
        if !made.is_empty() {
            self.made_dirs.extend(made);
            self.persist()?;
        }
        Ok(())
    }

    pub fn write(&mut self, rel: &RelPath, bytes: &[u8]) -> Result<()> {
        let i = self.touch(rel)?;
        let sha = sha256_hex(bytes);
        self.intend(i, Some(sha.clone()))?;
        self.track_dirs(rel)?;
        write_atomic(&self.path(rel)?, bytes)?;
        self.log.push(format!("wrote {rel} (SHA-256 {sha})"));
        Ok(())
    }

    pub fn delete(&mut self, rel: &RelPath) -> Result<()> {
        let i = self.touch(rel)?;
        let was = self.touched[i].before.as_ref().map(|b| b.1.clone()).unwrap_or_default();
        self.intend(i, None)?;
        let p = self.path(rel)?;
        if p.exists() {
            std::fs::remove_file(&p).map_err(|e| Error(format!("could not delete {rel}: {e}")))?;
        }
        self.log.push(format!("deleted {rel} (was SHA-256 {was})"));
        Ok(())
    }

    pub fn rename(&mut self, from: &RelPath, to: &RelPath) -> Result<()> {
        let pf = self.path(from)?;
        let Some(sha) = file_sha256(&pf) else { fail!("{from} is not there to rename") };
        let a = self.touch(from)?;
        let b = self.touch(to)?;
        if self.path(to)?.exists() {
            fail!("{to} is already there; dllchain does not overwrite it");
        }
        self.intend(a, None)?;
        self.intend(b, Some(sha.clone()))?;
        self.track_dirs(to)?;
        std::fs::rename(&pf, self.path(to)?).map_err(|e| Error(format!("could not rename {from} to {to}: {e}")))?;
        self.log.push(format!("renamed {from} to {to} (SHA-256 {sha})"));
        Ok(())
    }

    /// Removes the folder when it is empty; a folder with anything in it stays.
    pub fn remove_dir_if_empty(&mut self, rel: &RelPath) -> Result<bool> {
        let p = self.path(rel)?;
        if !p.is_dir() || std::fs::read_dir(&p)?.next().is_some() {
            return Ok(false);
        }
        self.removed_dirs.push(rel.clone());
        self.persist()?;
        std::fs::remove_dir(&p).map_err(|e| Error(format!("could not remove the folder {rel}: {e}")))?;
        self.log.push(format!("removed the empty folder {rel}"));
        Ok(true)
    }

    /// Ends the run. Its journal and copies are kept as the one run that can be undone; the run before loses its.
    pub fn commit(self) -> Result<Vec<String>> {
        let undo = self.path(&RelPath::of(UNDO))?;
        let older = std::fs::read_to_string(&undo).ok().and_then(|t| journal_id(&t)).filter(|id| *id != self.id);
        // the journal stops being one first (a rename replaces the old undo file in one step)
        std::fs::rename(self.path(&RelPath::of(JOURNAL))?, &undo)?;
        if let Some(id) = older {
            drop_backups(&self.game, &id);
        }
        Ok(self.log)
    }

    /// Puts every touched file back as it was. Returns a line for each file it had to leave alone.
    pub fn rollback(self) -> Vec<String> {
        let mut notes = Vec::new();
        for d in self.removed_dirs.iter().rev() {
            if let Ok(p) = self.path(d) {
                let _ = std::fs::create_dir_all(p);
            }
        }
        for t in self.touched.iter().rev() {
            let Ok(p) = self.path(&t.rel) else {
                notes.push(format!("{}: left as it is (the path is not safe)", t.rel));
                continue;
            };
            let _ = std::fs::remove_file(tmp_path(&p));
            let now = file_sha256(&p);
            let before_sha = t.before.as_ref().map(|b| b.1.clone());
            if now == before_sha {
                continue;
            }
            if !t.intended.contains(&now) {
                notes.push(format!(
                    "{}: changed by another program during the run, so dllchain left it as it is",
                    t.rel
                ));
                continue;
            }
            let r = match &t.before {
                None => std::fs::remove_file(&p).map_err(Error::from),
                Some((b, sha)) => match self.path(b).and_then(|bp| std::fs::read(bp).map_err(Error::from)) {
                    Ok(bytes) if sha256_hex(&bytes) == *sha => write_atomic(&p, &bytes),
                    Ok(_) => Err(Error("its backup copy was changed".into())),
                    Err(e) => Err(e),
                },
            };
            if let Err(e) = r {
                notes.push(format!("{}: could not be put back ({e})", t.rel));
            }
        }
        for d in self.made_dirs.iter().rev() {
            if let Ok(p) = self.path(d) {
                let _ = std::fs::remove_dir(p); // only when empty
            }
        }
        if let Ok(p) = self.path(&RelPath::of(JOURNAL)) {
            let _ = std::fs::remove_file(p);
        }
        // a file that could not be put back keeps its copy
        if notes.iter().all(|n| !n.contains("could not be put back")) {
            drop_backups(&self.game, &self.id);
        }
        notes
    }
}

/// Rolls back a run that was cut off, from its journal. `None` when there was none.
pub fn recover(game: &Path) -> Result<Option<Vec<String>>> {
    let jp = resolve(game, &RelPath::of(JOURNAL))?;
    let Ok(text) = std::fs::read_to_string(&jp) else {
        return Ok(None);
    };
    let Some((t, notes)) = parse(game, &text) else {
        std::fs::remove_file(&jp)?;
        return Ok(Some(vec![
            "dllchain's journal of an unfinished run could not be read; it was removed and nothing was undone".into(),
        ]));
    };
    let mut out = vec!["An earlier dllchain run was cut off; its changes were undone.".to_string()];
    out.extend(notes);
    out.extend(t.rollback());
    Ok(Some(out))
}

/// One file the last finished run changed.
#[derive(Clone, Debug)]
pub struct Change {
    pub rel: RelPath,
    /// The copy of what was there and its SHA-256 (`None`: nothing was).
    pub before: Option<(RelPath, String)>,
    /// What the run left there (`None`: nothing).
    pub after: Option<String>,
}

/// The last run that finished, which can be undone.
#[derive(Clone, Debug, Default)]
pub struct LastRun {
    pub action: String,
    /// When it ran (local time, as [`crate::clock::now_text`] writes it).
    pub at: String,
    pub changes: Vec<Change>,
    /// Folders it created, deepest last.
    pub made_dirs: Vec<RelPath>,
    /// Journal lines that could not be read; a run with any cannot be undone.
    pub unreadable: Vec<String>,
}

/// The last finished run, from `dllchain\undo.txt`; `None` when there is none (or it cannot be read).
pub fn last_run(game: &Path) -> Result<Option<LastRun>> {
    let Ok(text) = std::fs::read_to_string(resolve(game, &RelPath::of(UNDO))?) else {
        return Ok(None);
    };
    let Some((t, unreadable)) = parse(game, &text) else {
        // there is one, but it cannot be read: say so rather than that there is none
        let first = text.lines().next().unwrap_or("").to_string();
        return Ok(Some(LastRun { unreadable: vec![first], ..LastRun::default() }));
    };
    let changes = t
        .touched
        .iter()
        .map(|x| Change { rel: x.rel.clone(), before: x.before.clone(), after: x.intended.last().cloned().flatten() })
        .collect();
    Ok(Some(LastRun { action: t.action, at: t.at, changes, made_dirs: t.made_dirs, unreadable }))
}

/// Where a run keeps its copies of what it changed.
fn backup_dir(id: &str) -> RelPath {
    RelPath::of(&format!("{OWN_DIR}\\backup\\{id}"))
}

/// The id in a journal's first line.
fn journal_id(text: &str) -> Option<String> {
    let head: Vec<&str> = text.lines().next()?.split('\t').collect();
    match head.as_slice() {
        ["dllchain-journal", "1", id] if crate::paths::is_word(id) => Some(id.to_string()),
        _ => None,
    }
}

/// Removes a run's copies (and the backup folder when no other run's copies are in it).
fn drop_backups(game: &Path, id: &str) {
    if let Ok(p) = resolve(game, &backup_dir(id)) {
        let _ = std::fs::remove_dir_all(&p);
        if let Some(parent) = p.parent() {
            let _ = std::fs::remove_dir(parent); // only when empty
        }
    }
}

/// A journal as a run to roll back, with a note for each line it skipped; `None` when its first line is not one.
fn parse(game: &Path, text: &str) -> Option<(Txn, Vec<String>)> {
    let id = journal_id(text)?;
    let mut t = Txn {
        game: game.to_path_buf(),
        id,
        action: String::new(),
        at: String::new(),
        touched: Vec::new(),
        made_dirs: Vec::new(),
        removed_dirs: Vec::new(),
        log: Vec::new(),
    };
    let backups = t.backup_dir();
    let mut notes = Vec::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        let ok = match f.as_slice() {
            ["touch", rel, b, s] => match RelPath::parse(rel) {
                Ok(rel) => {
                    let before = if *b == "-" {
                        Some(None)
                    } else {
                        RelPath::parse(b)
                            .ok()
                            .filter(|b| b.strip_prefix(&backups).is_some() && is_sha256(s))
                            .map(|b| Some((b, s.to_string())))
                    };
                    before.map(|before| t.touched.push(Touched { rel, before, intended: Vec::new() })).is_some()
                }
                Err(_) => false,
            },
            ["intend", rel, s] => {
                let sha = if *s == "-" {
                    Some(None)
                } else if is_sha256(s) {
                    Some(Some(s.to_string()))
                } else {
                    None
                };
                match (RelPath::parse(rel), sha) {
                    (Ok(rel), Some(sha)) => {
                        t.touched.iter_mut().find(|x| x.rel.same(&rel)).map(|x| x.intended.push(sha)).is_some()
                    }
                    _ => false,
                }
            }
            ["made-dir", d] => RelPath::parse(d).map(|d| t.made_dirs.push(d)).is_ok(),
            ["removed-dir", d] => RelPath::parse(d).map(|d| t.removed_dirs.push(d)).is_ok(),
            ["action", at, action] => {
                (t.at, t.action) = (at.to_string(), action.to_string());
                true
            }
            [""] => true,
            _ => false,
        };
        if !ok {
            notes.push(format!("journal line skipped: {line}"));
        }
    }
    Some((t, notes))
}

fn tmp_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(TMP);
    PathBuf::from(s)
}

/// Writes through a temporary file and a rename, so a file is never half-written.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = tmp_path(path);
    let r = (|| -> std::io::Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = r {
        let _ = std::fs::remove_file(&tmp);
        fail!("could not write {}: {e}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("chainkit-txn-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rollback_puts_everything_back() {
        let g = dir("rollback");
        std::fs::write(g.join("a.txt"), b"old a").unwrap();
        std::fs::write(g.join("b.dll"), b"bbb").unwrap();
        let mut t = Txn::begin(&g, "t1", "test").unwrap();
        t.write(&RelPath::of("a.txt"), b"new a").unwrap();
        t.write(&RelPath::of("new\\deep\\c.txt"), b"c").unwrap();
        t.rename(&RelPath::of("b.dll"), &RelPath::of("moved.dll")).unwrap();
        t.delete(&RelPath::of("a.txt")).unwrap();
        assert!(!g.join("a.txt").exists());
        let notes = t.rollback();
        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(std::fs::read(g.join("a.txt")).unwrap(), b"old a");
        assert_eq!(std::fs::read(g.join("b.dll")).unwrap(), b"bbb");
        assert!(!g.join("moved.dll").exists());
        assert!(!g.join("new").exists());
        assert!(!g.join(JOURNAL).exists());
        let _ = std::fs::remove_dir_all(&g);
    }

    #[test]
    fn a_cut_off_run_is_undone_and_others_changes_are_kept() {
        let g = dir("recover");
        std::fs::write(g.join("a.txt"), b"old").unwrap();
        let mut t = Txn::begin(&g, "t2", "test").unwrap();
        t.write(&RelPath::of("a.txt"), b"mine").unwrap();
        t.write(&RelPath::of("b.txt"), b"mine too").unwrap();
        std::mem::forget(t); // as if the process stopped here
        std::fs::write(g.join("b.txt"), b"someone else").unwrap();
        let notes = recover(&g).unwrap().unwrap();
        assert_eq!(std::fs::read(g.join("a.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(g.join("b.txt")).unwrap(), b"someone else");
        assert!(notes.iter().any(|n| n.contains("b.txt") && n.contains("another program")), "{notes:?}");
        assert_eq!(recover(&g).unwrap(), None);
        let _ = std::fs::remove_dir_all(&g);
    }

    #[test]
    fn a_hostile_journal_cannot_reach_outside() {
        let g = dir("hostile");
        let outside = g.join("..").join(format!("chainkit-txn-outside-{}", std::process::id()));
        std::fs::write(&outside, b"keep me").unwrap();
        std::fs::create_dir_all(g.join(OWN_DIR)).unwrap();
        let journal = format!(
            "{HEADER}\tt3\ntouch\t..\\{name}\t-\t-\nintend\t..\\{name}\t-\ntouch\tx.txt\tC:\\Windows\\win.ini\t{s}\n",
            name = outside.file_name().unwrap().to_string_lossy(),
            s = "a".repeat(64)
        );
        std::fs::write(g.join(JOURNAL), journal).unwrap();
        let notes = recover(&g).unwrap().unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep me");
        assert!(notes.len() >= 3, "{notes:?}");
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&g);
    }
}
