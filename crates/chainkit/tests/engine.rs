//! Whole runs against fake game folders: every install order, updates, EMF, repairs, uninstalls, a failed run and a
//! hostile record.

use chainkit::chainini::ChainFile;
use chainkit::dllinfo::fake_dll;
use chainkit::engine::{Ctx, Plan, apply, plan_install, plan_rename, plan_repair, plan_uninstall, status};
use chainkit::game::ENSHROUDED;
use chainkit::hash::sha256_hex;
use chainkit::paths::RelPath;
use chainkit::source::ModSource;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn game(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("chainkit-engine-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("enshrouded.exe"), b"the game").unwrap();
    std::fs::write(d.join("steam_api64.dll"), b"steam").unwrap();
    d
}

fn ctx(dir: &Path) -> Ctx {
    let mut c = Ctx::new(dir, &ENSHROUDED).unwrap();
    c.in_system32 = |b| b == "uxtheme";
    c
}

/// Every file under `dir`, by lowercase path.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
                out.insert(format!("{}\\", p.strip_prefix(root).unwrap().display()).to_lowercase(), Vec::new());
            } else {
                out.insert(
                    p.strip_prefix(root).unwrap().display().to_string().to_lowercase(),
                    std::fs::read(&p).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Without dllchain's log, which a failed run adds to.
fn kept(s: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    s.iter().filter(|(k, _)| !k.starts_with("dllchain\\installer.log")).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn without_own(s: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    s.iter().filter(|(k, _)| !k.starts_with("dllchain")).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn alp() -> Vec<u8> {
    fake_dll(&["auto-loot-plus", "dinput8-chain.ini"], None)
}
fn alp2() -> Vec<u8> {
    fake_dll(&["auto-loot-plus", "dinput8-chain.ini", "version two"], None)
}
fn xhl() -> Vec<u8> {
    fake_dll(&[], Some("XHL Core"))
}
fn other() -> Vec<u8> {
    fake_dll(&[], Some("Other Thing"))
}
fn shiny() -> Vec<u8> {
    fake_dll(&["dinput8-chain.ini"], Some("Shiny Mod"))
}
fn emf() -> Vec<u8> {
    fake_dll(&["EMF-LOADER-7d3c9f", "dinput8-chain.ini"], None)
}

fn src(label: &str, dll: Vec<u8>, files: &[(&str, &[u8])]) -> ModSource {
    let mut all = dll.clone();
    for (n, b) in files {
        all.extend_from_slice(n.as_bytes());
        all.extend_from_slice(b);
    }
    ModSource {
        label: label.into(),
        sha256: sha256_hex(&all),
        word_hint: chainkit::source::word_hint(label.trim_end_matches(".zip")),
        dll,
        files: files.iter().map(|(n, b)| (RelPath::parse(n).unwrap(), b.to_vec())).collect(),
        ignored: Vec::new(),
        from_url: None,
    }
}

fn run(c: &Ctx, plan: chainkit::Result<Plan>) -> Plan {
    let p = plan.unwrap();
    apply(c, &p).unwrap();
    p
}

fn install(c: &Ctx, s: &ModSource) -> Plan {
    run(c, plan_install(c, s))
}

fn read(dir: &Path, rel: &str) -> Option<Vec<u8>> {
    std::fs::read(dir.join(rel)).ok()
}

/// The chain file's DLL lines: (name, on).
fn chain(dir: &Path) -> Vec<(String, bool)> {
    let Some(b) = read(dir, "dinput8-chain.ini") else {
        return Vec::new();
    };
    ChainFile::decode(&b).parse("dinput8.dll").entries.iter().map(|e| (e.name.to_string(), e.on)).collect()
}

/// Nothing a plan or apply leaves behind: no journal, no tmp files.
fn clean(dir: &Path) {
    assert!(!dir.join("dllchain\\journal.txt").exists());
    assert!(
        !snapshot(dir).keys().any(|k| k.contains(".dllchain-tmp")),
        "{:?}",
        snapshot(dir).keys().collect::<Vec<_>>()
    );
}

#[test]
fn a_host_and_a_guest_in_either_order() {
    let mut results = Vec::new();
    for (i, order) in [["alp", "xhl"], ["xhl", "alp"]].iter().enumerate() {
        let dir = game(&format!("order{i}"));
        let c = ctx(&dir);
        for m in order {
            let s = if *m == "alp" {
                src("AutoLootPlus-1-1-8.zip", alp(), &[("mods\\alp\\config.txt", b"cfg")])
            } else {
                src("xhl.zip", xhl(), &[])
            };
            install(&c, &s);
            clean(&dir);
        }
        assert_eq!(read(&dir, "dinput8.dll"), Some(alp()), "order {order:?}");
        assert_eq!(read(&dir, "xhl-core.dll"), Some(xhl()));
        assert_eq!(chain(&dir), vec![("xhl-core.dll".to_string(), true)]);
        assert_eq!(read(&dir, "mods\\alp\\config.txt"), Some(b"cfg".to_vec()));
        // nothing to repair right after an install
        assert!(plan_repair(&c).unwrap().is_empty());
        results.push(without_own(&snapshot(&dir)));
    }
    assert_eq!(results[0], results[1]);
}

#[test]
fn three_mods_newest_host_wins_only_when_the_current_one_cannot() {
    let dir = game("three");
    let c = ctx(&dir);
    install(&c, &src("xhl.zip", xhl(), &[]));
    install(&c, &src("alp.zip", alp(), &[]));
    install(&c, &src("shiny.zip", shiny(), &[]));
    // ALP could host when Shiny came, so it stays the host
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp()));
    let names: Vec<String> = chain(&dir).into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec!["xhl-core.dll", "shiny-mod.dll"]);
}

#[test]
fn two_mods_that_cannot_host_are_refused_and_nothing_changes() {
    let dir = game("nohost");
    let c = ctx(&dir);
    install(&c, &src("xhl.zip", xhl(), &[]));
    let before = snapshot(&dir);
    let e = plan_install(&c, &src("other.zip", other(), &[])).unwrap_err();
    assert!(e.0.contains("cannot run together"), "{e}");
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn a_proxy_or_system_name_gets_mod_added() {
    let dir = game("names");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    // a DLL named after a proxy says nothing about itself, so it gets a plain name
    install(&c, &src("version.zip", fake_dll(&[], Some("Version")), &[]));
    assert!(read(&dir, "dinput8-mod.dll").is_some());
    assert!(!dir.join("version.dll").exists());
    install(&c, &src("uxtheme.zip", fake_dll(&[], Some("UxTheme")), &[]));
    assert!(read(&dir, "uxtheme-mod.dll").is_some());
}

#[test]
fn update_keeps_the_players_changed_settings() {
    let dir = game("update");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[("mods\\alp\\config.txt", b"v1"), ("mods\\alp\\old.txt", b"old")]));
    std::fs::write(dir.join("mods\\alp\\config.txt"), b"mine").unwrap();
    let p = install(&c, &src("alp.zip", alp2(), &[("mods\\alp\\config.txt", b"v2")]));
    assert!(p.notes.iter().any(|n| n.contains("config.txt.new")), "{:?}", p.notes);
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp2()));
    assert_eq!(read(&dir, "mods\\alp\\config.txt"), Some(b"mine".to_vec()));
    assert_eq!(read(&dir, "mods\\alp\\config.txt.new"), Some(b"v2".to_vec()));
    assert!(!dir.join("mods\\alp\\old.txt").exists());
    let store: Vec<String> =
        snapshot(&dir).into_keys().filter(|k| k.starts_with("dllchain\\mods\\") && k.ends_with(".dll")).collect();
    assert_eq!(store, vec![format!("dllchain\\mods\\auto-loot-plus\\{}.dll", sha256_hex(&alp2()))]);
    // uninstalling keeps the changed file and takes the rest
    run(&c, plan_uninstall(&c, Some("auto-loot-plus")));
    assert_eq!(read(&dir, "mods\\alp\\config.txt"), Some(b"mine".to_vec()));
    assert!(!dir.join("mods\\alp\\config.txt.new").exists());
    assert!(!dir.join("dinput8.dll").exists());
    assert!(!dir.join("dllchain").exists());
}

#[test]
fn installing_the_same_mod_twice_changes_nothing() {
    let dir = game("twice");
    let c = ctx(&dir);
    let s = src("alp.zip", alp(), &[("mods\\alp\\config.txt", b"v1")]);
    install(&c, &s);
    let before = snapshot(&dir);
    let p = plan_install(&c, &s).unwrap();
    // only the record's date changes
    assert!(p.summary.iter().all(|l| !l.contains("install")), "{:?}", p.summary);
    apply(&c, &p).unwrap();
    assert_eq!(without_own(&snapshot(&dir)), without_own(&before));
}

#[test]
fn with_emf_every_mod_is_a_guest_and_emf_is_never_touched() {
    let dir = game("emf");
    std::fs::write(dir.join("dinput8.dll"), emf()).unwrap();
    std::fs::create_dir_all(dir.join("emf")).unwrap();
    std::fs::write(dir.join("emf\\moved.txt"), b"x").unwrap();
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    install(&c, &src("xhl.zip", xhl(), &[]));
    assert_eq!(read(&dir, "dinput8.dll"), Some(emf()));
    assert_eq!(read(&dir, "emf\\moved.txt"), Some(b"x".to_vec()));
    assert_eq!(chain(&dir), vec![("auto-loot-plus.dll".to_string(), true), ("xhl-core.dll".to_string(), true)]);
    run(&c, plan_uninstall(&c, None));
    assert_eq!(read(&dir, "dinput8.dll"), Some(emf()));
    assert!(chain(&dir).is_empty());
    assert!(!dir.join("dllchain").exists());
}

#[test]
fn emf_installed_afterwards_is_followed() {
    let dir = game("emf-after");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    install(&c, &src("xhl.zip", xhl(), &[]));
    // what EMF's installer does: the old dinput8.dll moves to a tool name and is listed
    std::fs::rename(dir.join("dinput8.dll"), dir.join("auto-loot-plus.dll")).unwrap();
    std::fs::write(dir.join("dinput8.dll"), emf()).unwrap();
    let mut ini = std::fs::read_to_string(dir.join("dinput8-chain.ini")).unwrap();
    ini.push_str("auto-loot-plus.dll\r\n");
    std::fs::write(dir.join("dinput8-chain.ini"), ini).unwrap();
    std::fs::create_dir_all(dir.join("emf")).unwrap();
    std::fs::write(dir.join("emf\\moved.txt"), b"moved\tauto-loot-plus.dll\r\n").unwrap();

    let st = status(&c).unwrap();
    assert!(st.notes.iter().any(|n| n.contains("auto-loot-plus.dll was added by someone else")), "{:?}", st.notes);
    let before = without_own(&snapshot(&dir));
    let p = run(&c, plan_repair(&c));
    // only dllchain's record follows the move
    assert_eq!(without_own(&snapshot(&dir)), before, "{:?}", p.summary);
    assert!(plan_repair(&c).unwrap().is_empty());
    run(&c, plan_uninstall(&c, None));
    assert_eq!(read(&dir, "dinput8.dll"), Some(emf()));
    assert!(!dir.join("auto-loot-plus.dll").exists() && !dir.join("xhl-core.dll").exists());
}

#[test]
fn repair_after_another_mod_was_copied_over() {
    let dir = game("repair");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    install(&c, &src("xhl.zip", xhl(), &[]));
    // a player copies another mod's dinput8.dll over by hand
    std::fs::write(dir.join("dinput8.dll"), shiny()).unwrap();
    let st = status(&c).unwrap();
    assert!(!st.repair.is_empty());
    run(&c, plan_repair(&c));
    clean(&dir);
    assert_eq!(read(&dir, "dinput8.dll"), Some(shiny()));
    assert_eq!(read(&dir, "auto-loot-plus.dll"), Some(alp()));
    assert_eq!(chain(&dir), vec![("xhl-core.dll".to_string(), true), ("auto-loot-plus.dll".to_string(), true)]);
    assert!(plan_repair(&c).unwrap().is_empty());
    // the copied-over mod is not dllchain's to uninstall, and stays after uninstalling everything
    assert!(plan_uninstall(&c, Some("shiny-mod")).is_err());
    run(&c, plan_uninstall(&c, None));
    assert_eq!(read(&dir, "dinput8.dll"), Some(shiny()));
    assert!(read(&dir, "dinput8-chain.ini").is_none());
    assert!(!dir.join("dllchain").exists());
}

#[test]
fn a_mod_that_cannot_host_copied_over_the_host_is_repaired() {
    let dir = game("repair2");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    std::fs::write(dir.join("dinput8.dll"), xhl()).unwrap();
    run(&c, plan_repair(&c));
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp()));
    assert_eq!(read(&dir, "xhl-core.dll"), Some(xhl()));
    assert_eq!(chain(&dir), vec![("xhl-core.dll".to_string(), true)]);
}

#[test]
fn uninstall_everything_gives_back_the_folder_byte_for_byte() {
    let dir = game("identical");
    // someone else's setup that was there first: a chain file in UTF-16 with their DLL, and a file the mod replaces
    let mut ini = vec![0xFF, 0xFE];
    for u in "; my setup\r\n[Chain]\r\ntheirs.dll\r\n\r\n[Other]\r\nx=1\r\n".encode_utf16() {
        ini.extend_from_slice(&u.to_le_bytes());
    }
    std::fs::write(dir.join("dinput8-chain.ini"), &ini).unwrap();
    std::fs::write(dir.join("theirs.dll"), fake_dll(&[], Some("Theirs"))).unwrap();
    std::fs::create_dir_all(dir.join("mods\\alp")).unwrap();
    std::fs::write(dir.join("mods\\alp\\config.txt"), b"there before").unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);

    let p = install(&c, &src("alp.zip", alp(), &[("mods\\alp\\config.txt", b"new"), ("mods\\alp\\deep\\x.txt", b"x")]));
    assert_eq!(p.replaces, vec![RelPath::of("mods\\alp\\config.txt")]);
    install(&c, &src("xhl.zip", xhl(), &[]));
    let names: Vec<String> = chain(&dir).into_iter().map(|e| e.0).collect();
    assert_eq!(names, vec!["theirs.dll", "xhl-core.dll"]);
    // the UTF-16 encoding stays
    assert_eq!(&read(&dir, "dinput8-chain.ini").unwrap()[..2], &[0xFF, 0xFE]);

    run(&c, plan_uninstall(&c, None));
    clean(&dir);
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn uninstall_one_lets_the_guest_become_host() {
    let dir = game("one");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    install(&c, &src("xhl.zip", xhl(), &[]));
    run(&c, plan_uninstall(&c, Some("auto-loot-plus")));
    assert_eq!(read(&dir, "dinput8.dll"), Some(xhl()));
    assert!(!dir.join("xhl-core.dll").exists());
    assert!(read(&dir, "dinput8-chain.ini").is_none());
    run(&c, plan_uninstall(&c, Some("xhl-core")));
    assert_eq!(snapshot(&dir).len(), 2);
}

#[test]
fn an_earlier_copy_of_the_same_mod_comes_back_on_uninstall() {
    let dir = game("earlier");
    std::fs::write(dir.join("dinput8.dll"), alp()).unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp2(), &[]));
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp2()));
    run(&c, plan_uninstall(&c, None));
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn a_mod_that_was_there_first_joins_and_stays() {
    let dir = game("first");
    std::fs::write(dir.join("dinput8.dll"), xhl()).unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp()));
    assert_eq!(read(&dir, "xhl-core.dll"), Some(xhl()));
    run(&c, plan_uninstall(&c, None));
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn a_switched_off_guest_stays_off_until_reinstalled() {
    let dir = game("off");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    install(&c, &src("xhl.zip", xhl(), &[]));
    let ini = std::fs::read_to_string(dir.join("dinput8-chain.ini")).unwrap().replace("xhl-core.dll", "; xhl-core.dll");
    std::fs::write(dir.join("dinput8-chain.ini"), ini).unwrap();
    assert!(plan_repair(&c).unwrap().summary.iter().all(|l| !l.contains("switch")));
    run(&c, plan_repair(&c));
    assert_eq!(chain(&dir), vec![("xhl-core.dll".to_string(), false)]);
    install(&c, &src("xhl.zip", xhl(), &[]));
    assert_eq!(chain(&dir), vec![("xhl-core.dll".to_string(), true)]);
}

#[test]
fn a_failed_run_is_undone_completely() {
    let dir = game("fail");
    let c = ctx(&dir);
    install(&c, &src("xhl.zip", xhl(), &[]));
    let before = snapshot(&dir);
    let mut p = plan_install(&c, &src("alp.zip", alp(), &[("mods\\alp\\a\\b.txt", b"x")])).unwrap();
    for n in 0..20 {
        p.fail_after = Some(n);
        if apply(&c, &p).is_ok() {
            // ran past the last step: that is the whole plan done; undo it for the next case
            run(&c, plan_uninstall(&c, Some("auto-loot-plus")));
            break;
        }
        clean(&dir);
        assert_eq!(kept(&snapshot(&dir)), kept(&before), "failing after {n} steps");
    }
}

#[test]
fn a_hostile_record_cannot_make_dllchain_delete_the_game() {
    let dir = game("hostile");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    let exe = sha256_hex(b"the game");
    let steam = sha256_hex(b"steam");
    let mut rec = std::fs::read_to_string(dir.join("dllchain\\record.txt")).unwrap();
    rec.push_str(&format!(
        "mod\tevil\tinstalled\t9\tnow\t-\tevil.zip\r\ndll\tevil\tenshrouded.exe\t{exe}\r\nfile\tevil\tsteam_api64.dll\t{steam}\t-\r\n\
         file\tauto-loot-plus\tenshrouded.exe\t{exe}\t-\r\nfile\tauto-loot-plus\t..\\outside.txt\t{exe}\t-\r\n"
    ));
    std::fs::write(dir.join("dllchain\\record.txt"), rec).unwrap();
    std::fs::write(dir.join("dinput8-chain.ini"), "[Chain]\r\n..\\..\\Windows\\System32\\evil.dll\r\nC:\\evil.dll\r\n")
        .unwrap();
    let p = plan_uninstall(&c, None).unwrap();
    assert!(!p.notes.is_empty());
    apply(&c, &p).unwrap();
    assert_eq!(read(&dir, "enshrouded.exe"), Some(b"the game".to_vec()));
    assert_eq!(read(&dir, "steam_api64.dll"), Some(b"steam".to_vec()));
    assert!(!dir.join("dinput8.dll").exists());
}

#[test]
fn an_unfinished_run_blocks_until_it_is_undone() {
    let dir = game("journal");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    let t = chainkit::txn::Txn::begin(&dir, "cut").unwrap();
    drop(t);
    assert!(plan_install(&c, &src("xhl.zip", xhl(), &[])).is_err());
    assert!(c.recover().unwrap().is_some());
    install(&c, &src("xhl.zip", xhl(), &[]));
}

#[test]
fn a_mod_put_there_by_hand_needs_no_repair() {
    let dir = game("byhand");
    std::fs::write(dir.join("dinput8.dll"), xhl()).unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);
    assert!(plan_repair(&c).unwrap().is_empty());
    let st = status(&c).unwrap();
    assert!(st.repair.is_empty(), "{:?}", st.repair);
    assert_eq!(st.rows.len(), 1);
    let r = &st.rows[0];
    assert_eq!(r.name, "XHL Core");
    assert_eq!(r.role, "game loads it; starts no others");
    assert_eq!(r.source, "put there by hand");
    assert!(r.details.iter().any(|(_, v)| v.contains("joins the chain")), "{:?}", r.details);
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn rename_changes_only_the_name() {
    let dir = game("rename");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    let mut s = src("xhl.zip", xhl(), &[]);
    s.from_url = Some("https://www.nexusmods.com/enshrouded/mods/1".into());
    install(&c, &s);
    let files = without_own(&snapshot(&dir));
    let name = |c: &Ctx, file: &str| status(c).unwrap().rows.into_iter().find(|r| r.file.as_str() == file).unwrap();
    // a DLL dllchain does not know shows its own product name
    assert_eq!(name(&c, "xhl-core.dll").name, "XHL Core");
    run(&c, plan_rename(&c, "xhl-core", Some("  Extra HUD Lines ")));
    let r = name(&c, "xhl-core.dll");
    assert_eq!(r.name, "Extra HUD Lines");
    assert!(!r.at.is_empty());
    assert_eq!(r.page.as_deref(), Some("https://www.nexusmods.com/enshrouded/mods/1"));
    assert!(
        r.details.iter().any(|(k, v)| k == "Web page" && v == "https://www.nexusmods.com/enshrouded/mods/1"),
        "{:?}",
        r.details
    );
    assert!(r.details.iter().any(|(k, v)| k == "Short name" && v == "xhl-core"), "{:?}", r.details);
    assert_eq!(without_own(&snapshot(&dir)), files);
    // the same name again changes nothing; an empty one gives the DLL's own name back
    assert!(plan_rename(&c, "xhl-core", Some("Extra HUD Lines")).unwrap().is_empty());
    run(&c, plan_rename(&c, "xhl-core", Some("   ")));
    assert_eq!(name(&c, "xhl-core.dll").name, "XHL Core");
    // a known tool keeps its known name until renamed, and an update keeps the player's name
    run(&c, plan_rename(&c, "auto-loot-plus", Some("ALP")));
    install(&c, &src("alp.zip", alp2(), &[]));
    assert_eq!(name(&c, "dinput8.dll").name, "ALP");
    assert!(plan_rename(&c, "nobody", Some("x")).is_err());
    clean(&dir);
}

#[test]
fn the_plan_and_the_details_in_plain_words() {
    let dir = game("words");
    let c = ctx(&dir);
    install(&c, &src("xhl.zip", xhl(), &[]));
    let files: [(&str, &[u8]); 4] = [
        (r"mods\auto-loot-plus\mod.lua", b"m"),
        (r"mods\auto-loot-plus\config.txt", b"c"),
        (r"mods\auto-loot-plus\items.txt", b"i"),
        (r"licenses\LICENSE.txt", b"l"),
    ];
    let p = plan_install(&c, &src("AutoLootPlus 1.1.8\\", alp(), &files)).unwrap();
    assert_eq!(p.name, "Auto Loot Plus");
    assert_eq!(
        p.summary,
        [
            "Auto Loot Plus will be added to your game.",
            "XHL Core keeps working.",
            r"Adds 4 more files, most in mods\auto-loot-plus\."
        ],
        "{:?}",
        p.summary
    );
    assert!(p.files.contains(&"dinput8.dll renamed to xhl-core.dll".to_string()), "{:?}", p.files);
    assert!(p.files.contains(&r"mods\auto-loot-plus\mod.lua (new)".to_string()), "{:?}", p.files);
    assert!(p.files.iter().all(|f| !f.starts_with("dllchain")), "{:?}", p.files);
    apply(&c, &p).unwrap();
    let st = status(&c).unwrap();
    let r = st.rows.iter().find(|r| r.file.as_str() == "dinput8.dll").unwrap();
    assert_eq!(r.source, "AutoLootPlus 1.1.8\\");
    let get = |k: &str| r.details.iter().find(|(l, _)| l == k).map(|(_, v)| v.clone());
    assert_eq!(get("File").as_deref(), Some("dinput8.dll (game loads it; starts others)"));
    assert_eq!(get("From").as_deref(), Some("the folder \"AutoLootPlus 1.1.8\""));
    assert_eq!(get("Web page").as_deref(), Some("not known"));
    assert_eq!(get("Files added").as_deref(), Some(r"4, in mods\auto-loot-plus\ and licenses\"));
    assert_eq!(get("SHA-256"), Some(sha256_hex(&alp())));
    assert!(get("Installed").is_some_and(|t| t.contains(", ") && !t.contains('-')), "{:?}", r.details);

    let p = plan_uninstall(&c, Some("auto-loot-plus")).unwrap();
    assert_eq!(p.name, "Auto Loot Plus");
    assert_eq!(
        p.summary,
        [
            "Auto Loot Plus will be removed from your game.",
            r"Deletes 5 files, most in mods\auto-loot-plus\.",
            "XHL Core keeps working."
        ],
        "{:?}",
        p.summary
    );
    run(&c, Ok(p));
    clean(&dir);
}

#[test]
fn a_mod_with_hundreds_of_files_still_gets_a_few_lines() {
    let dir = game("many");
    let c = ctx(&dir);
    // 20 files the player already has, which the mod replaces
    for i in 0..20 {
        let p = dir.join(format!(r"mods\big\old-{i}.txt"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"theirs").unwrap();
    }
    let names: Vec<String> = (0..320)
        .map(|i| if i < 20 { format!(r"mods\big\old-{i}.txt") } else { format!(r"mods\big\f{i}.txt") })
        .collect();
    let files: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), b"ours".as_slice())).collect();
    let p = plan_install(&c, &src("big.zip", alp(), &files)).unwrap();
    assert_eq!(
        p.summary,
        [
            "Auto Loot Plus will be added to your game.",
            r"Adds 300 more files, all in mods\big\.",
            r"Replaces 20 files that were already there, all in mods\big\.",
            "dllchain keeps the old ones, and Uninstall puts them back."
        ],
        "{:?}",
        p.summary
    );
    assert_eq!(p.files.len(), 321, "{:?}", p.files);
    apply(&c, &p).unwrap();
    // the player changes 3 of them; uninstall still words it in a few lines
    for i in 20..23 {
        std::fs::write(dir.join(format!(r"mods\big\f{i}.txt")), b"mine").unwrap();
    }
    let p = plan_uninstall(&c, Some("auto-loot-plus")).unwrap();
    assert_eq!(
        p.summary,
        [
            "Auto Loot Plus will be removed from your game.",
            "Deletes 298 files, most in mods\\big\\.",
            r"Puts back 20 files that were there before, all in mods\big\.",
            "dllchain's own folder is removed too: nothing is left to undo."
        ],
        "{:?}",
        p.summary
    );
    assert_eq!(p.notes, ["You changed 3 of these files, so they stay"], "{:?}", p.notes);
    run(&c, Ok(p));
    assert_eq!(std::fs::read(dir.join(r"mods\big\old-0.txt")).unwrap(), b"theirs");
    clean(&dir);
}

/// The missing requirements' lines, and the status details of the installed `word` that are notes.
fn needs(c: &Ctx, p: &Plan, word: &str) -> (Vec<String>, Vec<String>) {
    let shown = status(c).unwrap();
    let row = shown.rows.iter().find(|r| r.word.as_deref() == Some(word)).unwrap();
    let notes = row.details.iter().filter(|(l, _)| l == "Note").map(|(_, v)| v.clone()).collect();
    (p.missing.iter().map(|r| r.line()).collect(), notes)
}

#[test]
fn an_eml_package_warns_only_while_eml_is_missing() {
    let warning = "This mod also needs the Enshrouded Mod Loader (EML), which isn't installed. Parts of it won't work \
                   until you install EML.";
    let package: &[(&str, &[u8])] =
        &[(r"mods\auto-loot-plus\mod.json", b"{}"), (r"mods\auto-loot-plus\src\mod.lua", b"")];

    // without EML: a warning, and the install still goes ahead
    let dir = game("eml-missing");
    let c = ctx(&dir);
    let p = install(&c, &src("AutoLootPlus.zip", alp(), package));
    assert_eq!(needs(&c, &p, "auto-loot-plus"), (vec![warning.to_string()], vec![warning.to_string()]));
    assert!(dir.join(r"mods\auto-loot-plus\mod.json").exists());

    // EML as dbghelp.dll, or chained as a guest of the host: no warning
    let eml = fake_dll(&["EML_LOG_FILE_PATH"], None);
    for (name, at) in [("eml-dbghelp", "dbghelp.dll"), ("eml-guest", "eml.dll")] {
        let dir = game(name);
        std::fs::write(dir.join(at), &eml).unwrap();
        if at == "eml.dll" {
            std::fs::write(dir.join("dinput8-chain.ini"), "eml.dll\r\n").unwrap();
        }
        let c = ctx(&dir);
        let p = install(&c, &src("AutoLootPlus.zip", alp(), package));
        assert_eq!(needs(&c, &p, "auto-loot-plus"), (vec![], vec![]), "{name}");
    }

    // a switched-off EML does not run
    let dir = game("eml-off");
    std::fs::write(dir.join("eml.dll"), &eml).unwrap();
    std::fs::write(dir.join("dinput8-chain.ini"), "; eml.dll\r\n").unwrap();
    let c = ctx(&dir);
    assert_eq!(plan_install(&c, &src("AutoLootPlus.zip", alp(), package)).unwrap().missing.len(), 1);

    // no mods\ folder, or a mod.json that is not an EML package's: nothing to say
    let dir = game("no-package");
    let c = ctx(&dir);
    let p = install(&c, &src("Shiny.zip", shiny(), &[("shiny.txt", b"settings"), (r"mods\mod.json", b"{}")]));
    assert_eq!(needs(&c, &p, "shiny-mod"), (vec![], vec![]));
}
