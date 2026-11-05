//! What is specific to a game. One entry today, Enshrouded; another game is another entry.

use crate::paths::RelPath;

/// A loader or DLL mod dllchain recognises by a text inside its DLL (plain or UTF-16, case ignored).
#[derive(Debug)]
pub struct Tool {
    /// Its file-name word: a renamed copy becomes `<word>.dll`.
    pub word: &'static str,
    /// The name players see.
    pub name: &'static str,
    pub texts: &'static [&'static str],
    /// Its log, which says whether it loaded; `None` when dllchain does not know it.
    pub log: Option<LoadLog>,
    /// Files an older version left that a newer one replaced: dllchain takes them out (with a copy) on install.
    pub leftovers: &'static [Leftover],
}

impl Tool {
    /// A tool dllchain knows only by name.
    pub const fn named(word: &'static str, name: &'static str, texts: &'static [&'static str]) -> Tool {
        Tool { word, name, texts, log: None, leftovers: &[] }
    }
}

/// How a mod's own log, next to its DLL, says it loaded: its first line (after the time) is `<first><version>`, and a
/// later line has `ok`, or `off` when the player switched the mod's main part off.
#[derive(Debug)]
pub struct LoadLog {
    pub file: &'static str,
    pub first: &'static str,
    pub ok: &'static str,
    pub off: &'static str,
}

/// `old`, next to the mod's DLL, is what an older version had instead of `new`.
#[derive(Debug)]
pub struct Leftover {
    pub old: &'static str,
    pub new: &'static str,
}

/// A loader that refuses DLLs next to the game and runs each DLL mod from a folder of its own, `<dir>\<mod>\`, when
/// the mod declares itself there with `declaration`. With `off_switch` in that folder too, the loader takes it for one
/// of its script packages, which stays off until the player enables it.
#[derive(Debug)]
pub struct PackageHost {
    pub word: &'static str,
    pub dir: &'static str,
    pub declaration: &'static str,
    pub off_switch: &'static str,
}

/// Something a mod needs that dllchain does not install. dllchain only says so; it never blocks or downloads.
#[derive(Debug)]
pub struct Requirement {
    /// As players call it: "This mod also needs <name>".
    pub name: &'static str,
    pub short: &'static str,
    /// Where players get it.
    pub page: &'static str,
    /// A mod needs it when it ships `<dir>\<any folder>\<file>`.
    pub dir: &'static str,
    pub file: &'static str,
    /// It is installed when a DLL the game loads (the host, a chain entry, another proxy name) contains one of these.
    pub texts: &'static [&'static str],
}

impl Requirement {
    /// Whether a mod with these files needs it.
    pub fn needed_by<'a>(&self, files: impl IntoIterator<Item = &'a RelPath>) -> bool {
        files.into_iter().any(|r| {
            let parts: Vec<&str> = r.as_str().split('\\').collect();
            parts.len() == 3 && parts[0].eq_ignore_ascii_case(self.dir) && parts[2].eq_ignore_ascii_case(self.file)
        })
    }

    /// The warning, one line.
    pub fn line(&self) -> String {
        format!(
            "This mod also needs {}, which isn't installed. Parts of it won't work until you install {}.",
            self.name, self.short
        )
    }
}

#[derive(Debug)]
pub struct Game {
    pub name: &'static str,
    pub steam_app_id: u32,
    /// One of these must be in the game folder.
    pub exes: &'static [&'static str],
    /// The name the host DLL has; the game loads it.
    pub host_dll: &'static str,
    /// The community chain file the host reads.
    pub chain_file: &'static str,
    /// A DLL that contains one of these texts can start the others when it is the host.
    pub host_texts: &'static [&'static str],
    /// EMF's loader carries this text (its `loader_id.h`); EMF's own installer looks after it.
    pub emf_texts: &'static [&'static str],
    /// The first tool whose text a DLL contains names it; the order matters (ShroudForge's DLLs contain EML's text).
    pub tools: &'static [Tool],
    /// Other proxy names: other loaders own them, and a renamed mod must never get one.
    pub proxy_names: &'static [&'static str],
    /// Never written, replaced or deleted, wherever a zip puts them.
    pub protected_extensions: &'static [&'static str],
    pub protected_names: &'static [&'static str],
    /// Other tools' folders in the game folder that dllchain never writes into.
    pub other_tool_dirs: &'static [&'static str],
    /// dllchain changes nothing while one of these runs.
    pub running: &'static [&'static str],
    /// Where the saves live: `<steam>\userdata\<id>\<app id>\remote` and `<Saved Games>\<this>`.
    pub saved_games_dir: &'static str,
    /// What mods can need besides dllchain; checked on install and shown with the mod.
    pub requirements: &'static [Requirement],
    /// Loaders that run DLL mods from folders of their own instead of a chain.
    pub package_hosts: &'static [PackageHost],
    /// The game's own log in its folder, written at every start.
    pub game_log: &'static str,
}

pub static ENSHROUDED: Game = Game {
    name: "Enshrouded",
    steam_app_id: 1203620,
    exes: &["enshrouded.exe", "enshrouded_server.exe"],
    host_dll: "dinput8.dll",
    chain_file: "dinput8-chain.ini",
    host_texts: &["dinput8-chain.ini"],
    emf_texts: &["EMF-LOADER-7d3c9f", "emf_probe"],
    // The lines after auto-loot-plus are EMF's known_tools.txt (MIT, Copyright (c) 2026 EMF contributors; see
    // THIRD-PARTY-NOTICES.txt), so both tools give a renamed DLL the same name.
    tools: &[
        // enstash's first log line is `enstash Quick Stack placeholders <MOD_VERSION>`, and a later one says
        // `placeholders ON` once Quick Stack is hooked (mod/quickstack/enstash_quickstack.c in AKJama/enshrouded-chests).
        // From 0.12 its item data is enstash-data.dat; it still reads an old enstash-data.txt, which would be stale.
        Tool {
            word: "enstash",
            name: "enstash",
            texts: &["enstash Quick Stack"],
            log: Some(LoadLog {
                file: "enstash-quickstack.log",
                first: "enstash Quick Stack placeholders ",
                ok: "placeholders ON",
                off: "placeholders OFF",
            }),
            leftovers: &[Leftover { old: "enstash-data.txt", new: "enstash-data.dat" }],
        },
        Tool::named("auto-loot-plus", "Auto Loot Plus", &["auto-loot-plus"]),
        Tool::named("shroudforge", "ShroudForge", &["shroudforge"]),
        Tool::named("eml", "EML (Enshrouded Mod Loader)", &["EML_LOG_FILE_PATH"]),
        Tool::named("modloader-proxy", "ModLoader Proxy", &["modloader-proxy.dll", "chainloader.ini"]),
        Tool::named("shroudtopia", "Shroudtopia (or its Shroudweave fork)", &["shroudtopia"]),
        Tool::named("greeneye", "Greeneye Loader", &["greeneye"]),
        Tool::named("aldrich-modmenu", "Aldrich Mod Menu", &["aldrich"]),
    ],
    proxy_names: &[
        "dinput8",
        "dbghelp",
        "dbgcore",
        "winmm",
        "powrprof",
        "version",
        "xinput1_4",
        "dxgi",
        "umpdc",
        "cfgmgr32",
    ],
    protected_extensions: &[".exe", ".kfc"],
    protected_names: &["steam_api64.dll", "steam_appid.txt", "dinput8-chain.ini", "chainloader.ini"],
    other_tool_dirs: &["emf", "shroudforge"],
    running: &["enshrouded.exe", "enshrouded_server.exe", "steam.exe", "emf.exe"],
    saved_games_dir: "Enshrouded",
    // An EML script package is `mods\<id>\mod.json` (EML's docs/src/eml/develop/setup.md). EML runs as `dbghelp.dll` or
    // `dinput8.dll`, and both carry EML_LOG_FILE_PATH (crates/*-proxy/src/logging.rs); ShroudForge, which also runs
    // EML packages, carries it too.
    requirements: &[Requirement {
        name: "the Enshrouded Mod Loader (EML)",
        short: "EML",
        page: "https://brabb3l.github.io/kfc-parser/eml/usage.html",
        dir: "mods",
        file: "mod.json",
        texts: &["EML_LOG_FILE_PATH"],
    }],
    // ShroudForge 1.7.8's DLL Manager guide (built into shroudforge.exe): a DLL loads only from a folder directly under
    // mods\, declared by its native-plugin.ini (`[Plugin]`, `Enabled=1`, `Dll=<file>`); DLLs next to the game are
    // refused. With mod.json there the folder is one of its packages, off until enabled (enstash's rig, S8, S8d, S8e).
    package_hosts: &[PackageHost {
        word: "shroudforge",
        dir: "mods",
        declaration: "native-plugin.ini",
        off_switch: "mod.json",
    }],
    game_log: "enshrouded.log",
};

impl Game {
    pub fn tool(&self, word: &str) -> Option<&'static Tool> {
        self.tools.iter().find(|t| t.word == word)
    }

    pub fn package_host(&self, word: &str) -> Option<&'static PackageHost> {
        self.package_hosts.iter().find(|p| p.word == word)
    }

    /// The name players see for a word: the tool's name, or the word itself.
    pub fn display(&self, word: &str) -> String {
        self.tools.iter().find(|t| t.word == word).map(|t| t.name.to_string()).unwrap_or_else(|| word.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_eml_package_is_mods_id_mod_json() {
        let eml = &ENSHROUDED.requirements[0];
        let needs =
            |files: &[&str]| eml.needed_by(files.iter().map(|f| RelPath::parse(f).unwrap()).collect::<Vec<_>>().iter());
        assert!(needs(&[r"mods\auto-loot-plus\src\mod.lua", r"mods\auto-loot-plus\mod.json"]));
        assert!(needs(&[r"MODS\x\Mod.JSON"]));
        assert!(!needs(&[]));
        assert!(!needs(&[r"mods\mod.json", r"mods\x\src\mod.json", r"other\x\mod.json", r"mods\x\mod.json.bak"]));
    }
}
