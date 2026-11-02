//! A copy of the player's saves before the first install, as EMF makes one. The saves are only read.

use crate::game::Game;
use crate::hash::sha256_hex;
use crate::{Error, Result, clock};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct SaveRoots {
    /// Steam's folder: saves in `userdata\<id>\<app id>\remote`.
    pub steam: Option<PathBuf>,
    /// The Saved Games folder: saves in `<it>\<game>`.
    pub saved_games: Option<PathBuf>,
    /// Where the copies go: `<it>\<stamp>\...` and `<it>\starter.txt`.
    pub store: PathBuf,
}

impl SaveRoots {
    /// `%USERPROFILE%\Saved Games` and `%LOCALAPPDATA%\dllchain\save-backups`.
    pub fn for_user(steam: Option<PathBuf>) -> Option<SaveRoots> {
        let local = std::env::var_os("LOCALAPPDATA")?;
        let saved_games = std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Saved Games"));
        Some(SaveRoots { steam, saved_games, store: PathBuf::from(local).join("dllchain").join("save-backups") })
    }
}

pub fn has_starter(roots: &SaveRoots) -> bool {
    roots.store.join("starter.txt").is_file()
}

/// Every save file, with the folder it goes to in the copy.
fn find(roots: &SaveRoots, game: &Game) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    if let Some(steam) = &roots.steam {
        if let Ok(users) = std::fs::read_dir(steam.join("userdata")) {
            for u in users.flatten() {
                let id = u.file_name().to_string_lossy().into_owned();
                if !id.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                let remote = u.path().join(game.steam_app_id.to_string()).join("remote");
                walk(&remote, &PathBuf::from(format!("steam-{id}")), &mut out);
            }
        }
    }
    if let Some(sg) = &roots.saved_games {
        walk(&sg.join(game.saved_games_dir), &PathBuf::from("saved-games"), &mut out);
    }
    out
}

fn walk(dir: &Path, to: &Path, out: &mut Vec<(PathBuf, PathBuf)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let name = e.file_name();
        if ft.is_dir() {
            walk(&e.path(), &to.join(&name), out);
        } else if ft.is_file() {
            out.push((e.path(), to.join(&name)));
        }
    }
}

/// Copies every save once, checks each copy, and returns what to tell the player.
pub fn starter_backup(roots: &SaveRoots, game: &Game) -> Result<String> {
    let files = find(roots, game);
    if files.is_empty() {
        return Ok(format!("No {} saves on this PC yet, so there was nothing to back up.", game.name));
    }
    let folder = roots.store.join(format!("{}-before-dllchain", clock::now_stamp()));
    let mut bytes = 0u64;
    for (from, to) in &files {
        let data =
            std::fs::read(from).map_err(|e| Error(format!("could not read the save {}: {e}", from.display())))?;
        let dst = folder.join(to);
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::write(&dst, &data)?;
        let back = std::fs::read(&dst)?;
        if sha256_hex(&back) != sha256_hex(&data) {
            return Err(Error(format!("the copy of {} does not match the save", from.display())));
        }
        bytes += data.len() as u64;
    }
    std::fs::write(roots.store.join("starter.txt"), format!("{}\r\n{} files\r\n", folder.display(), files.len()))?;
    Ok(format!(
        "Backed up your saves first: {} files ({} KB) in {}.",
        files.len(),
        bytes.div_ceil(1024),
        folder.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::ENSHROUDED;

    #[test]
    fn copies_steam_and_local_saves_once() {
        let root = std::env::temp_dir().join(format!("chainkit-saves-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let remote = root.join("steam\\userdata\\123\\1203620\\remote");
        std::fs::create_dir_all(&remote).unwrap();
        std::fs::write(remote.join("3ad85aea"), b"world").unwrap();
        std::fs::create_dir_all(root.join("sg\\Enshrouded\\sub")).unwrap();
        std::fs::write(root.join("sg\\Enshrouded\\sub\\x"), b"local").unwrap();
        let roots = SaveRoots {
            steam: Some(root.join("steam")),
            saved_games: Some(root.join("sg")),
            store: root.join("store"),
        };
        assert!(!has_starter(&roots));
        let msg = starter_backup(&roots, &ENSHROUDED).unwrap();
        assert!(msg.contains("2 files"), "{msg}");
        assert!(has_starter(&roots));
        assert_eq!(std::fs::read(remote.join("3ad85aea")).unwrap(), b"world");
        let _ = std::fs::remove_dir_all(&root);
    }
}
