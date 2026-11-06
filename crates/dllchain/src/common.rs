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

/// The question before uninstalling everything: every file the run takes out, under the folder it is in, and that
/// Undo cannot take it back. Past `max` files the list stops, and the rest comes second (its folder named again), for
/// Show details. Both are empty when the run takes nothing out.
pub fn removed_text(plan: &Plan, max: usize) -> (String, String) {
    split_removed(plan.removed_by_folder(), max)
}

fn split_removed(by_folder: Vec<(String, Vec<String>)>, max: usize) -> (String, String) {
    let lines = by_folder
        .into_iter()
        .flat_map(|(folder, names)| names.into_iter().map(move |n| (vec![folder_head(&folder)], format!("    {n}\n"))));
    let [mut shown, rest] = split_list(lines, max);
    if !shown.is_empty() {
        shown = format!("These files are removed:\n{shown}Undo cannot take this back.\n");
    }
    (shown, rest)
}

/// Lines under their headings (outermost first, each with its line end): the first `max` lines in the first part, the
/// rest in the second. Each part gives a line's headings again where they differ from the line before it there.
fn split_list(lines: impl IntoIterator<Item = (Vec<String>, String)>, max: usize) -> [String; 2] {
    let mut out = [String::new(), String::new()];
    let mut last: [Vec<String>; 2] = Default::default();
    for (count, (heads, line)) in lines.into_iter().enumerate() {
        let k = usize::from(count >= max);
        let same = last[k].iter().zip(&heads).take_while(|(a, b)| a == b).count();
        for h in &heads[same..] {
            out[k].push_str(h);
        }
        out[k].push_str(&line);
        last[k] = heads;
    }
    out
}

/// "From the game folder:" or "From mods\x\:", with its line end.
fn folder_head(folder: &str) -> String {
    match folder {
        "" => "From the game folder:\n".to_string(),
        f => format!("From {f}:\n"),
    }
}

/// What the mods made while they ran (logs, backups, reports), for the uninstall question: the tick box's words, the
/// list by mod and folder with what Undo can do, and, past `max` entries, the rest of the list (its mod and folder
/// named again), for Show details. `None` when they made nothing dllchain could find.
pub fn made_choice(plan: &Plan, max: usize) -> Option<(String, String, String)> {
    let label = match plan.made.as_slice() {
        [] => return None,
        [(name, _)] => format!("Also delete {name}'s log and the other files it made"),
        _ => "Also delete the mods' logs and the other files they made".to_string(),
    };
    let lines = plan.made.iter().flat_map(|(name, items)| {
        items.iter().map(move |m| {
            let folder = m.rel.parent().map_or(String::new(), |p| format!("{p}\\"));
            let line = match (m.is_dir, m.files.len()) {
                (false, _) => format!("    {}\n", m.rel.file_name()),
                (true, 1) => format!("    {}\\ (a folder, 1 file)\n", m.rel.file_name()),
                (true, n) => format!("    {}\\ (a folder, {n} files)\n", m.rel.file_name()),
            };
            (vec![format!("{name} made these while it ran:\n"), folder_head(&folder)], line)
        })
    });
    let [mut shown, rest] = split_list(lines, max);
    shown.push_str(if plan.undoable() {
        "If you delete them, Undo can bring them back.\n"
    } else {
        "If you delete them, they are gone for good: Undo cannot bring them back.\n"
    });
    Some((label, shown, rest))
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
    fn a_long_uninstall_list_stops_and_the_rest_names_its_folder_again() {
        let list = || {
            vec![
                (String::new(), vec!["dinput8.dll".to_string(), "enstash.dll".to_string()]),
                (r"mods\alp\".to_string(), vec!["a.lua".to_string(), "b.lua".to_string()]),
            ]
        };
        let (shown, rest) = split_removed(list(), 3);
        assert_eq!(
            shown,
            "These files are removed:\nFrom the game folder:\n    dinput8.dll\n    enstash.dll\nFrom mods\\alp\\:\n    a.lua\nUndo cannot take this back.\n"
        );
        assert_eq!(rest, "From mods\\alp\\:\n    b.lua\n");
        let (shown, rest) = split_removed(list(), usize::MAX);
        assert!(shown.contains("    b.lua\n") && rest.is_empty(), "{shown}");
        assert_eq!(split_removed(Vec::new(), 3), (String::new(), String::new()));
    }

    #[test]
    fn a_list_gives_its_headings_again_where_they_change_and_after_the_cut() {
        let line = |heads: &[&str], l: &str| (heads.iter().map(|h| h.to_string()).collect(), l.to_string());
        let lines = [
            line(&["a:\n", "game:\n"], "  1\n"),
            line(&["a:\n", "game:\n"], "  2\n"),
            line(&["b:\n", "game:\n"], "  3\n"),
            line(&["b:\n", "mods:\n"], "  4\n"),
        ];
        let [shown, rest] = split_list(lines.clone(), 3);
        assert_eq!(shown, "a:\ngame:\n  1\n  2\nb:\ngame:\n  3\n");
        assert_eq!(rest, "b:\nmods:\n  4\n");
        let [shown, rest] = split_list(lines, usize::MAX);
        assert!(shown.ends_with("  3\nmods:\n  4\n") && rest.is_empty(), "{shown}");
    }

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
