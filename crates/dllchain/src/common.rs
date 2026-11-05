//! What the window and the command line share: finding the game, the checks before a change, and the words for a plan.

use chainkit::engine::{Ctx, Plan, apply};
use chainkit::game::{ENSHROUDED, Game};
use chainkit::saves::{SaveRoots, has_starter, starter_backup};
use chainkit::{Error, Result, fail, procs, steam};
use std::path::PathBuf;

pub const GAME: &Game = &ENSHROUDED;

#[derive(Clone, Debug, Default)]
pub struct Opts {
    pub game: Option<PathBuf>,
    pub steam_root: Option<PathBuf>,
    /// For tests: where the saves are and where their copies go, instead of the player's own folders.
    pub saved_games: Option<PathBuf>,
    pub save_store: Option<PathBuf>,
    /// For tests on a PC where Steam is open: fake game folders only.
    pub skip_running_check: bool,
}

impl Opts {
    pub fn steam_root(&self) -> Option<PathBuf> {
        self.steam_root.clone().or_else(steam::steam_root_from_registry)
    }

    /// The game folder: the one given, else the one Steam has.
    pub fn game_dir(&self) -> Result<PathBuf> {
        if let Some(g) = &self.game {
            return Ok(g.clone());
        }
        let Some(root) = self.steam_root() else {
            fail!("Steam was not found on this PC. Pick {}'s folder yourself.", GAME.name);
        };
        let found = steam::find_game(&root, GAME);
        match found.len() {
            0 => fail!("{} was not found in Steam's libraries. Pick its folder yourself.", GAME.name),
            1 => Ok(found[0].clone()),
            _ => fail!(
                "{} is in more than one Steam library ({}). Pick the folder you play from.",
                GAME.name,
                found.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// The game folder, ready for a plan: the game and Steam are closed, and an unfinished run is undone first.
    pub fn open(&self) -> Result<(Ctx, Vec<String>)> {
        let ctx = Ctx::new(&self.game_dir()?, GAME)?;
        self.check_closed()?;
        let notes = ctx.recover()?.unwrap_or_default();
        Ok((ctx, notes))
    }

    pub fn check_closed(&self) -> Result<()> {
        if self.skip_running_check {
            return Ok(());
        }
        match procs::blocking(GAME, &procs::running_exes()) {
            Some(why) => Err(Error(why)),
            None => Ok(()),
        }
    }

    fn save_roots(&self) -> Option<SaveRoots> {
        let mut r = SaveRoots::for_user(self.steam_root())?;
        if let Some(s) = &self.saved_games {
            r.saved_games = Some(s.clone());
        }
        if let Some(s) = &self.save_store {
            r.store = s.clone();
        }
        Some(r)
    }

    /// Makes the plan's changes; before the first install ever, the saves are copied first, as EMF does.
    pub fn apply(&self, ctx: &Ctx, plan: &Plan) -> Result<Vec<String>> {
        self.check_closed()?;
        let mut out = Vec::new();
        if plan.first_install && !plan.is_empty() {
            if let Some(roots) = self.save_roots() {
                if !has_starter(&roots) {
                    out.push(starter_backup(&roots, GAME)?);
                }
            }
        }
        apply(ctx, plan)?;
        Ok(out)
    }
}

/// The plan in words, for the confirmation.
pub fn describe(plan: &Plan) -> String {
    let mut s = String::new();
    if plan.is_empty() {
        s.push_str("Nothing needs to change.\n");
    } else {
        s.push_str("What changes in your game folder:\n");
        for l in &plan.summary {
            s.push_str(&format!("  - {l}\n"));
        }
    }
    for r in &plan.missing {
        s.push_str(&format!("\n{}\n{}'s page: {}\n", r.line(), r.short, r.page));
    }
    if !plan.notes.is_empty() {
        s.push_str("\nGood to know:\n");
        for n in &plan.notes {
            s.push_str(&format!("  - {n}\n"));
        }
    }
    s
}

/// Every file a run takes out, under the folder it is in, for the question before uninstalling everything.
pub fn removed_text(plan: &Plan) -> String {
    let mut s = String::new();
    for (folder, names) in plan.removed_by_folder() {
        match folder.as_str() {
            "" => s.push_str("From the game folder:\n"),
            f => s.push_str(&format!("From {f}:\n")),
        }
        for n in names {
            s.push_str(&format!("    {n}\n"));
        }
    }
    s
}

/// A mod's details as aligned lines: the labels in one column, the values in the next. With `tab`, a tab goes
/// between them instead, for a text box with a tab stop.
pub fn details_text(details: &[(String, String)], tab: bool) -> String {
    let width = details.iter().map(|(l, _)| l.chars().count()).max().unwrap_or(0) + 4;
    let mut s = String::new();
    for (label, value) in details {
        if tab {
            s.push_str(&format!("{label}\t{value}\n"));
        } else {
            s.push_str(&format!("{label:width$}{value}\n"));
        }
    }
    s
}

/// A folder as Explorer shows it: with its real capitals (Steam keeps its path in lower case), without the `\\?\`
/// prefix. The path as given when it cannot be read.
pub fn shown(p: &std::path::Path) -> String {
    let real = std::fs::canonicalize(p).map(|c| c.display().to_string()).unwrap_or_else(|_| p.display().to_string());
    without_verbatim(&real)
}

fn without_verbatim(p: &str) -> String {
    match p.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => p.strip_prefix(r"\\?\").unwrap_or(p).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_shown_as_explorer_shows_them() {
        assert_eq!(without_verbatim(r"\\?\C:\Program Files (x86)\Steam"), r"C:\Program Files (x86)\Steam");
        assert_eq!(without_verbatim(r"\\?\UNC\server\share\Games"), r"\\server\share\Games");
        assert_eq!(without_verbatim(r"D:\Games"), r"D:\Games");
        let dir = std::env::temp_dir().join("dllchain-Shown-Case");
        std::fs::create_dir_all(&dir).unwrap();
        let lower = std::path::PathBuf::from(dir.display().to_string().to_lowercase());
        let s = shown(&lower);
        assert!(s.ends_with("dllchain-Shown-Case") && !s.starts_with(r"\\?\"), "{s}");
    }
}
