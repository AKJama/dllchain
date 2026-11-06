//! Whole runs against fake game folders: every install order, updates, EMF, repairs, uninstalls, a failed run and a
//! hostile record.

use chainkit::chainini::ChainFile;
use chainkit::dllinfo::{fake_dll, fake_dll_with};
use chainkit::engine::{
    Ctx, Load, Plan, Row, Status, apply, plan_install, plan_rename, plan_repair, plan_undo, plan_uninstall,
    plan_uninstall_with, status, version_in,
};
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
        extras: Vec::new(),
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
fn an_updated_guest_keeps_its_file_name() {
    let dir = game("guest-update");
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    install(&c, &src("xhl.zip", xhl(), &[]));
    let xhl2 = fake_dll(&["version two"], Some("XHL Core"));
    let p = install(&c, &src("xhl.zip", xhl2.clone(), &[]));
    assert_eq!(p.files, vec!["xhl-core.dll (replaced)"]);
    assert_eq!(read(&dir, "xhl-core.dll"), Some(xhl2));
    assert!(!dir.join("xhl-core-2.dll").exists());
    assert_eq!(chain(&dir), vec![("xhl-core.dll".to_string(), true)]);
    assert!(plan_repair(&c).unwrap().is_empty());
}

#[test]
fn a_dll_only_update_keeps_the_mods_files_and_its_place() {
    // Auto Loot Plus installed in full, then a mod that can also host, then a fix shipped as the DLL alone
    let dir = game("dll-only");
    let c = ctx(&dir);
    let full = [
        ("AutoLootPlus-README.txt", b"read me" as &[u8]),
        ("mods\\alp\\config.txt", b"v1"),
        ("mods\\alp\\mod.lua", b"lua"),
        ("licenses\\ALP-LICENSE.txt", b"mit"),
    ];
    install(&c, &src("alp-fix1\\", alp(), &full));
    install(&c, &src("shiny.zip", shiny(), &[]));
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp()));
    let before = without_own(&snapshot(&dir));
    let p = install(&c, &src("alp-fix2\\dinput8.dll", alp2(), &[]));
    assert_eq!(
        p.summary,
        vec!["Auto Loot Plus will be updated to the new version.", "Keeps the other 4 files from the earlier version."]
    );
    assert_eq!(p.files, vec!["dinput8.dll (replaced)"]);
    // the host stays the host, and every other file is as it was
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp2()));
    assert_eq!(chain(&dir), vec![("shiny-mod.dll".to_string(), true)]);
    let mut after = without_own(&snapshot(&dir));
    after.insert("dinput8.dll".into(), alp());
    assert_eq!(after, before);
    assert!(plan_repair(&c).unwrap().is_empty());
    // the files are still the mod's: a full version can leave some out, and uninstall takes them all
    let p = install(&c, &src("alp-fix3\\", alp(), &full[1..2]));
    assert!(p.summary.iter().any(|l| l == "Deletes 2 files the new version does not have."), "{:?}", p.summary);
    assert!(!dir.join("mods\\alp\\mod.lua").exists() && !dir.join("licenses\\ALP-LICENSE.txt").exists());
    // a read-me the earlier version put next to the game is not taken for something the new one leaves out
    assert_eq!(read(&dir, "AutoLootPlus-README.txt"), Some(b"read me".to_vec()));
    run(&c, plan_uninstall(&c, None));
    assert_eq!(
        snapshot(&dir).into_keys().collect::<Vec<_>>(),
        vec!["enshrouded.exe".to_string(), "steam_api64.dll".to_string()]
    );
}

#[test]
fn an_updated_host_stays_the_host_whatever_the_order() {
    // the guest came later, so it is the newest mod that can host; the host still keeps its place
    let cases: [&[(&str, &[u8])]; 2] = [&[], &[("mods\\alp\\config.txt", b"v2")]];
    for (i, files) in cases.iter().enumerate() {
        let dir = game(&format!("host-update{i}"));
        let c = ctx(&dir);
        install(&c, &src("alp.zip", alp(), &[("mods\\alp\\config.txt", b"v1")]));
        install(&c, &src("shiny.zip", shiny(), &[]));
        let p = install(&c, &src("alp.zip", alp2(), files));
        assert_eq!(read(&dir, "dinput8.dll"), Some(alp2()), "{:?}", p.files);
        assert_eq!(read(&dir, "shiny-mod.dll"), Some(shiny()));
        assert_eq!(chain(&dir), vec![("shiny-mod.dll".to_string(), true)]);
        assert!(!dir.join("auto-loot-plus.dll").exists());
        assert!(plan_repair(&c).unwrap().is_empty());
    }
}

#[test]
fn release_extras_are_named_and_never_installed() {
    let dir = game("extras");
    let c = ctx(&dir);
    let mut s = src("alp-fix2\\dinput8.dll", alp(), &[]);
    s.extras = ["AutoLootPlus-LICENSE.txt", "DearImGui-LICENSE.txt", "SHA256SUMS.txt", "Vulkan-Headers-LICENSE.md"]
        .map(String::from)
        .to_vec();
    let p = install(&c, &s);
    let list = "AutoLootPlus-LICENSE.txt, DearImGui-LICENSE.txt, SHA256SUMS.txt, Vulkan-Headers-LICENSE.md";
    assert!(p.notes.iter().any(|n| n.ends_with(&format!("stay out of the game folder: {list}"))), "{:?}", p.notes);
    assert_eq!(p.files, vec!["dinput8.dll (new)"]);
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
    let t = chainkit::txn::Txn::begin(&dir, "cut", "test").unwrap();
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

fn enstash() -> Vec<u8> {
    fake_dll(&["enstash Quick Stack"], None)
}
fn enstash2() -> Vec<u8> {
    fake_dll(&["enstash Quick Stack", "a newer build"], None)
}

/// enstash was `dinput8.dll` first, so dllchain chained it (and kept a copy) when ALP came.
fn enstash_chained(name: &str) -> (PathBuf, Ctx, BTreeMap<String, Vec<u8>>) {
    let dir = game(name);
    std::fs::write(dir.join("dinput8.dll"), enstash()).unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);
    install(&c, &src("alp.zip", alp(), &[]));
    assert_eq!(read(&dir, "enstash.dll"), Some(enstash()));
    (dir, c, before)
}

fn store(dir: &Path, word: &str) -> Vec<String> {
    let prefix = format!("dllchain\\mods\\{word}\\");
    snapshot(dir)
        .into_keys()
        .filter_map(|k| k.strip_prefix(&prefix).filter(|f| !f.is_empty()).map(String::from))
        .collect()
}

#[test]
fn a_guest_replaced_by_hand_with_a_newer_build_is_kept() {
    let (dir, c, before) = enstash_chained("rebuilt");
    std::fs::write(dir.join("enstash.dll"), enstash2()).unwrap();
    let st = status(&c).unwrap();
    assert!(st.repair.iter().any(|l| l.contains("keeps yours")), "{:?}", st.repair);
    assert!(st.repair.iter().all(|l| !l.contains("put back")), "{:?}", st.repair);
    let row = st.rows.iter().find(|r| r.file == RelPath::of("enstash.dll")).unwrap();
    assert_eq!(row.word.as_deref(), Some("enstash"));
    assert!(row.details.iter().any(|(k, v)| k == "SHA-256" && *v == sha256_hex(&enstash2())), "{:?}", row.details);
    assert!(row.details.iter().any(|(_, v)| v.contains("replaced by hand")), "{:?}", row.details);

    let p = run(&c, plan_repair(&c));
    clean(&dir);
    assert!(p.files.is_empty(), "only dllchain's own files change: {:?}", p.files);
    assert_eq!(read(&dir, "enstash.dll"), Some(enstash2()));
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp()));
    assert_eq!(chain(&dir), vec![("enstash.dll".to_string(), true)]);
    assert_eq!(store(&dir, "enstash"), vec![format!("{}.dll", sha256_hex(&enstash2()))]);
    assert!(plan_repair(&c).unwrap().is_empty());

    // the newer build is what comes back as dinput8.dll
    let mut expect = before;
    expect.insert("dinput8.dll".into(), enstash2());
    run(&c, plan_uninstall(&c, None));
    assert_eq!(snapshot(&dir), expect);
}

#[test]
fn a_newer_build_is_kept_by_any_run_not_only_a_repair() {
    let (dir, c, _) = enstash_chained("rebuilt-uninstall");
    std::fs::write(dir.join("enstash.dll"), enstash2()).unwrap();
    run(&c, plan_uninstall(&c, None));
    assert_eq!(read(&dir, "dinput8.dll"), Some(enstash2()));
    assert!(!dir.join("enstash.dll").exists());
    assert!(!dir.join("dllchain").exists());
}

#[test]
fn installing_over_a_rebuilt_mod_gives_the_rebuilt_one_back_on_uninstall() {
    let (dir, c, _) = enstash_chained("rebuilt-install");
    std::fs::write(dir.join("enstash.dll"), enstash2()).unwrap();
    let v3 = fake_dll(&["enstash Quick Stack", "version three"], None);
    install(&c, &src("enstash.zip", v3.clone(), &[]));
    assert_eq!(read(&dir, "enstash.dll"), Some(v3));
    run(&c, plan_uninstall(&c, Some("enstash")));
    assert_eq!(read(&dir, "enstash.dll"), Some(enstash2()));
    assert!(plan_repair(&c).unwrap().is_empty());
}

#[test]
fn a_guest_that_is_missing_broken_or_another_mod_is_still_repaired() {
    for (i, what) in ["missing", "not a dll", "another mod"].iter().enumerate() {
        let (dir, c, _) = enstash_chained(&format!("not-rebuilt{i}"));
        match *what {
            "missing" => std::fs::remove_file(dir.join("enstash.dll")).unwrap(),
            "not a dll" => std::fs::write(dir.join("enstash.dll"), b"half a download").unwrap(),
            _ => std::fs::write(dir.join("enstash.dll"), xhl()).unwrap(),
        }
        let st = status(&c).unwrap();
        assert!(st.repair.iter().all(|l| !l.contains("keeps yours")), "{what}: {:?}", st.repair);
        let put_back = if *what == "missing" {
            "enstash is put back from dllchain's copy."
        } else {
            "enstash is put back from dllchain's copy, as enstash-2.dll."
        };
        assert!(st.repair.iter().any(|l| l == put_back), "{what}: {:?}", st.repair);
        run(&c, plan_repair(&c));
        clean(&dir);
        // dllchain's copy runs again, and nothing of someone else's is deleted
        let names: Vec<String> = chain(&dir).into_iter().map(|e| e.0).collect();
        assert_eq!(names.len(), 1, "{what}: {names:?}");
        assert_eq!(read(&dir, &names[0]), Some(enstash()), "{what}");
        match *what {
            "missing" => {}
            "not a dll" => assert_eq!(read(&dir, "enstash.dll"), Some(b"half a download".to_vec())),
            _ => assert_eq!(read(&dir, "enstash.dll"), Some(xhl())),
        }
        assert_eq!(store(&dir, "enstash"), vec![format!("{}.dll", sha256_hex(&enstash()))], "{what}");
    }
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

// ---- every layout enstash meets: alone, chained with Auto Loot Plus, under ShroudForge, next to EML ----

const ENSTASH_ZIP: &str = "SmartQuickStack-0.12.2-beta-03c1cbd.zip";
const PLUGIN_INI: &[u8] = b"[Plugin]\r\nEnabled=1\r\nDll=dinput8.dll\r\n";

/// enstash's zip as its release script makes it: the DLL, its data, an EML package's mod.json, ShroudForge's
/// native-plugin.ini and licences, all at the top.
fn enstash_zip(dll: Vec<u8>, data: &[u8]) -> ModSource {
    src(
        ENSTASH_ZIP,
        dll,
        &[
            ("enstash-data.dat", data),
            ("mod.json", b"{\"id\":\"enstash\"}"),
            ("native-plugin.ini", PLUGIN_INI),
            ("licenses\\enstash-zstd-LICENSE.txt", b"licence"),
        ],
    )
}

/// The player's own files next to enstash's DLL, which no install or uninstall may touch.
fn enstash_user_files(dir: &Path) {
    std::fs::create_dir_all(dir.join("enstash-backups")).unwrap();
    std::fs::write(dir.join("enstash-marks.txt"), b"my marks").unwrap();
    std::fs::write(dir.join("enstash-quickstack.off"), b"").unwrap();
    std::fs::write(dir.join("enstash-settings.ini"), b"radius=20").unwrap();
    std::fs::write(dir.join("enstash-quickstack.log"), b"an old log").unwrap();
    std::fs::write(dir.join("enstash-backups\\marks-1.txt"), b"backup").unwrap();
}

fn user_files_kept(dir: &Path) {
    assert_eq!(read(dir, "enstash-marks.txt").as_deref(), Some(&b"my marks"[..]));
    assert_eq!(read(dir, "enstash-quickstack.off").as_deref(), Some(&b""[..]));
    assert_eq!(read(dir, "enstash-settings.ini").as_deref(), Some(&b"radius=20"[..]));
    assert_eq!(read(dir, "enstash-quickstack.log").as_deref(), Some(&b"an old log"[..]));
    assert_eq!(read(dir, "enstash-backups\\marks-1.txt").as_deref(), Some(&b"backup"[..]));
}

/// ShroudForge 1.7.8 as its installer leaves it: its loader under three proxy names, its folder and its packages.
fn shroudforge(dir: &Path) -> Vec<u8> {
    let sf = fake_dll(&["shroudforge", "EML_LOG_FILE_PATH"], None);
    for n in ["dinput8.dll", "dbghelp.dll", "winmm.dll"] {
        std::fs::write(dir.join(n), &sf).unwrap();
    }
    std::fs::create_dir_all(dir.join("mods\\sf-survival")).unwrap();
    std::fs::write(dir.join("mods\\sf-survival\\mod.json"), b"{}").unwrap();
    std::fs::create_dir_all(dir.join("shroudforge")).unwrap();
    std::fs::write(dir.join("shroudforge\\settings.json"), b"{}").unwrap();
    sf
}

fn row<'a>(st: &'a Status, word: &str) -> &'a Row {
    st.rows.iter().find(|r| r.word.as_deref() == Some(word)).unwrap_or_else(|| panic!("no row for {word}"))
}

#[test]
fn enstash_alone_takes_out_the_old_data_file_and_gives_it_back() {
    let dir = game("enstash-alone");
    enstash_user_files(&dir);
    // an older enstash, put there by hand, read its item data from a text file
    std::fs::write(dir.join("enstash-data.txt"), b"old data").unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);

    let p = install(&c, &enstash_zip(enstash(), b"dat"));
    assert!(p.summary.iter().any(|l| l.starts_with("Takes out enstash-data.txt")), "{:?}", p.summary);
    assert!(p.replaces.is_empty());
    assert_eq!(read(&dir, "dinput8.dll"), Some(enstash()));
    assert_eq!(read(&dir, "enstash-data.txt"), None);
    assert_eq!(read(&dir, "enstash-data.dat").as_deref(), Some(&b"dat"[..]));
    user_files_kept(&dir);
    assert_eq!(row(&status(&c).unwrap(), "enstash").version.as_deref(), Some("0.12.2-beta"));

    // an update that still ships the .dat: the .txt stays out
    install(&c, &enstash_zip(enstash2(), b"dat 2"));
    assert_eq!(read(&dir, "enstash-data.txt"), None);
    user_files_kept(&dir);

    run(&c, plan_uninstall(&c, None));
    clean(&dir);
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn enstash_chained_with_auto_loot_plus_and_back() {
    let dir = game("enstash-alp");
    enstash_user_files(&dir);
    std::fs::write(dir.join("enstash-data.txt"), b"old data").unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);
    install(&c, &src("AutoLootPlus-1-1-8.zip", alp(), &[("mods\\alp\\config.txt", b"cfg")]));
    install(&c, &enstash_zip(enstash(), b"dat"));
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp()));
    assert_eq!(read(&dir, "enstash.dll"), Some(enstash()));
    assert_eq!(chain(&dir), vec![("enstash.dll".to_string(), true)]);
    assert_eq!(read(&dir, "enstash-data.txt"), None);
    user_files_kept(&dir);

    // an update keeps its place in the chain
    install(&c, &enstash_zip(enstash2(), b"dat 2"));
    assert_eq!(read(&dir, "enstash.dll"), Some(enstash2()));
    assert_eq!(chain(&dir), vec![("enstash.dll".to_string(), true)]);

    run(&c, plan_uninstall(&c, Some("enstash")));
    assert_eq!(read(&dir, "enstash-data.txt").as_deref(), Some(&b"old data"[..]));
    run(&c, plan_uninstall(&c, None));
    clean(&dir);
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn under_shroudforge_enstash_goes_into_its_own_folder_without_mod_json() {
    let dir = game("shroudforge");
    let sf = shroudforge(&dir);
    let before = snapshot(&dir);
    let c = ctx(&dir);

    let p = install(&c, &enstash_zip(enstash(), b"dat"));
    assert!(p.replaces.is_empty());
    assert!(p.notes.iter().any(|n| n.starts_with("mod.json from the zip is left out")), "{:?}", p.notes);
    // ShroudForge and its packages as they were; nothing next to the game, no chain file
    for n in ["dinput8.dll", "dbghelp.dll", "winmm.dll"] {
        assert_eq!(read(&dir, n), Some(sf.clone()), "{n}");
    }
    assert_eq!(read(&dir, "dinput8-chain.ini"), None);
    for n in ["enstash.dll", "mod.json", "native-plugin.ini", "enstash-data.dat", "licenses"] {
        assert!(!dir.join(n).exists(), "{n}");
    }
    let pkg = dir.join("mods\\enstash");
    assert_eq!(read(&pkg, "dinput8.dll"), Some(enstash()));
    assert_eq!(read(&pkg, "native-plugin.ini").as_deref(), Some(PLUGIN_INI));
    assert_eq!(read(&pkg, "enstash-data.dat").as_deref(), Some(&b"dat"[..]));
    assert_eq!(read(&pkg, "licenses\\enstash-zstd-LICENSE.txt").as_deref(), Some(&b"licence"[..]));
    assert!(!pkg.join("mod.json").exists());
    let st = status(&c).unwrap();
    let r = row(&st, "enstash");
    assert_eq!(r.file.as_str(), "mods\\enstash\\dinput8.dll");
    assert_eq!(r.role, "loaded by ShroudForge from its own folder");
    assert_eq!(r.version.as_deref(), Some("0.12.2-beta"));
    assert!(st.repair.is_empty(), "{:?}", st.repair);

    // the player's files in its folder stay through an update, and the update stays in the folder
    enstash_user_files(&pkg);
    let p = install(&c, &enstash_zip(enstash2(), b"dat 2"));
    assert!(p.replaces.is_empty());
    assert_eq!(read(&pkg, "dinput8.dll"), Some(enstash2()));
    assert_eq!(read(&pkg, "enstash-data.dat").as_deref(), Some(&b"dat 2"[..]));
    assert!(!pkg.join("mod.json").exists());
    assert_eq!(read(&dir, "dinput8.dll"), Some(sf.clone()));
    user_files_kept(&pkg);
    // a new DLL alone keeps the rest
    install(&c, &src("dinput8.dll", enstash(), &[]));
    assert_eq!(read(&pkg, "dinput8.dll"), Some(enstash()));
    assert_eq!(read(&pkg, "native-plugin.ini").as_deref(), Some(PLUGIN_INI));
    assert!(plan_repair(&c).unwrap().is_empty());

    run(&c, plan_uninstall(&c, Some("enstash")));
    clean(&dir);
    user_files_kept(&pkg);
    for n in ["dinput8.dll", "native-plugin.ini", "enstash-data.dat", "licenses"] {
        assert!(!pkg.join(n).exists(), "{n}");
    }
    std::fs::remove_dir_all(pkg).unwrap();
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn under_shroudforge_a_hand_install_is_updated_and_given_back() {
    let dir = game("shroudforge-hand");
    let sf = shroudforge(&dir);
    // installed by hand from the zip, with mod.json (which keeps the folder switched off) and an older data file
    let pkg = dir.join("mods\\enstash");
    enstash_user_files(&pkg);
    std::fs::write(pkg.join("dinput8.dll"), fake_dll(&["enstash Quick Stack", "0.11"], None)).unwrap();
    std::fs::write(pkg.join("mod.json"), b"{\"id\":\"enstash\"}").unwrap();
    std::fs::write(pkg.join("native-plugin.ini"), PLUGIN_INI).unwrap();
    std::fs::write(pkg.join("enstash-data.txt"), b"old data").unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);

    let p = install(&c, &enstash_zip(enstash(), b"dat"));
    // the copy there is the same mod's: no leave to replace is needed, and dllchain keeps it
    assert!(p.replaces.is_empty(), "{:?}", p.replaces);
    assert!(p.summary.iter().any(|l| l.starts_with("Takes out mods\\enstash\\mod.json")), "{:?}", p.summary);
    assert!(p.summary.iter().any(|l| l.starts_with("Takes out mods\\enstash\\enstash-data.txt")), "{:?}", p.summary);
    assert_eq!(read(&pkg, "dinput8.dll"), Some(enstash()));
    assert!(!pkg.join("mod.json").exists());
    assert!(!pkg.join("enstash-data.txt").exists());
    assert_eq!(read(&dir, "dinput8.dll"), Some(sf));
    user_files_kept(&pkg);

    run(&c, plan_uninstall(&c, None));
    clean(&dir);
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn a_mod_chained_with_shroudforge_moves_into_its_folder() {
    // ShroudForge put in after enstash, as dllchain 0.1.5 let it: enstash chained next to the game, where
    // ShroudForge never loads it
    let dir = game("shroudforge-legacy");
    let host_enstash = fake_dll(&["enstash Quick Stack", "dinput8-chain.ini"], None);
    let sf = fake_dll(&["shroudforge", "EML_LOG_FILE_PATH"], None);
    let c = ctx(&dir);
    install(&c, &enstash_zip(host_enstash.clone(), b"dat"));
    install(&c, &src("ShroudForge.dll", sf.clone(), &[]));
    assert_eq!(read(&dir, "dinput8.dll"), Some(sf.clone()));
    assert_eq!(read(&dir, "enstash.dll"), Some(host_enstash.clone()));
    let st = status(&c).unwrap();
    assert!(st.notes.iter().any(|n| n.starts_with("enstash does not run: ShroudForge")), "{:?}", st.notes);

    enstash_user_files(&dir);
    let p = install(&c, &enstash_zip(host_enstash.clone(), b"dat"));
    assert!(
        p.notes
            .iter()
            .any(|n| n.starts_with("Your own enstash files next to the game stay there")
                && n.contains("enstash-marks.txt")),
        "{:?}",
        p.notes
    );
    assert!(p.summary.iter().any(|l| l.starts_with("enstash moves from the game folder into mods\\enstash\\")));
    assert_eq!(read(&dir, "dinput8.dll"), Some(sf));
    assert_eq!(read(&dir, "mods\\enstash\\dinput8.dll"), Some(host_enstash));
    assert_eq!(read(&dir, "dinput8-chain.ini"), None);
    user_files_kept(&dir);
    for n in ["shroudforge.dll", "enstash.dll", "mod.json", "native-plugin.ini", "enstash-data.dat", "licenses"] {
        assert!(!dir.join(n).exists(), "{n}");
    }
    let st = status(&c).unwrap();
    assert!(st.notes.iter().all(|n| !n.contains("does not run")), "{:?}", st.notes);
    assert!(st.repair.is_empty(), "{:?}", st.repair);
}

#[test]
fn under_shroudforge_a_mod_made_for_the_game_folder_gets_its_own_folder_and_declaration() {
    let dir = game("shroudforge-alp");
    let sf = shroudforge(&dir);
    let before = snapshot(&dir);
    let c = ctx(&dir);
    // Auto Loot Plus as its zip has it: its DLL, and a script package of its own in mods\auto-loot-plus\
    let zip = src(
        "AutoLootPlus-1-1-8.zip",
        alp(),
        &[(r"mods\auto-loot-plus\mod.json", b"{}"), (r"mods\auto-loot-plus\config.txt", b"cfg")],
    );
    let p = install(&c, &zip);
    assert!(p.replaces.is_empty());
    assert!(p.notes.iter().any(|n| n.contains("has no native-plugin.ini, so dllchain writes one")), "{:?}", p.notes);
    assert!(
        p.notes.iter().any(|n| n.starts_with(r"ShroudForge keeps mods\auto-loot-plus\ switched off")),
        "{:?}",
        p.notes
    );
    assert_eq!(read(&dir, "dinput8.dll"), Some(sf.clone()));
    // the script package stays where the zip puts it, mod.json and all; the DLL gets a folder of its own
    assert_eq!(read(&dir, r"mods\auto-loot-plus\mod.json").as_deref(), Some(&b"{}"[..]));
    assert_eq!(read(&dir, r"mods\auto-loot-plus\config.txt").as_deref(), Some(&b"cfg"[..]));
    let pkg = dir.join(r"mods\auto-loot-plus-dll");
    assert_eq!(read(&pkg, "dinput8.dll"), Some(alp()));
    assert_eq!(read(&pkg, "native-plugin.ini").as_deref(), Some(PLUGIN_INI));
    assert!(!pkg.join("mod.json").exists());
    assert_eq!(read(&dir, "dinput8-chain.ini"), None);
    let st = status(&c).unwrap();
    assert_eq!(row(&st, "auto-loot-plus").file.as_str(), r"mods\auto-loot-plus-dll\dinput8.dll");
    assert!(st.repair.is_empty(), "{:?}", st.repair);

    // a new DLL alone stays in that folder
    install(&c, &src("dinput8.dll", alp2(), &[]));
    assert_eq!(read(&pkg, "dinput8.dll"), Some(alp2()));
    assert_eq!(read(&pkg, "native-plugin.ini").as_deref(), Some(PLUGIN_INI));

    // enstash next to it, as a DLL alone: its own folder, with a declaration too
    install(&c, &src("enstash.dll", enstash(), &[]));
    assert_eq!(read(&dir, r"mods\enstash\dinput8.dll"), Some(enstash()));
    assert_eq!(read(&dir, r"mods\enstash\native-plugin.ini").as_deref(), Some(PLUGIN_INI));

    // Undo takes the written declaration out with the rest
    run(&c, plan_undo(&c));
    assert!(!dir.join(r"mods\enstash").exists());
    run(&c, plan_uninstall(&c, None));
    clean(&dir);
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn under_shroudforge_an_update_that_adds_a_script_package_moves_the_dll_out_of_it() {
    let dir = game("shroudforge-alp-update");
    shroudforge(&dir);
    let before = snapshot(&dir);
    let c = ctx(&dir);
    // first a version that says how it loads, in mods\auto-loot-plus\
    install(&c, &src("AutoLootPlus-1-1-7.zip", alp(), &[("native-plugin.ini", PLUGIN_INI)]));
    assert_eq!(read(&dir, r"mods\auto-loot-plus\dinput8.dll"), Some(alp()));
    // then one made for the game folder, whose script package is mods\auto-loot-plus\: the DLL cannot stay there
    let zip = src("AutoLootPlus-1-1-8.zip", alp2(), &[(r"mods\auto-loot-plus\mod.json", b"{}")]);
    install(&c, &zip);
    assert_eq!(read(&dir, r"mods\auto-loot-plus-dll\dinput8.dll"), Some(alp2()));
    assert_eq!(read(&dir, r"mods\auto-loot-plus-dll\native-plugin.ini").as_deref(), Some(PLUGIN_INI));
    assert!(!dir.join(r"mods\auto-loot-plus\dinput8.dll").exists());
    assert_eq!(read(&dir, r"mods\auto-loot-plus\mod.json").as_deref(), Some(&b"{}"[..]));
    run(&c, plan_uninstall(&c, None));
    clean(&dir);
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn under_shroudforge_a_declaration_that_is_wrong_is_refused() {
    let dir = game("shroudforge-refuse");
    shroudforge(&dir);
    let before = snapshot(&dir);
    let c = ctx(&dir);
    // a declaration that loads another DLL, or is switched off
    let other = src(ENSTASH_ZIP, enstash(), &[("native-plugin.ini", b"[Plugin]\r\nEnabled=1\r\nDll=other.dll\r\n")]);
    assert!(plan_install(&c, &other).unwrap_err().0.contains("loads other.dll"));
    let off = src(ENSTASH_ZIP, enstash(), &[("native-plugin.ini", b"[Plugin]\r\nEnabled=0\r\nDll=dinput8.dll\r\n")]);
    assert!(plan_install(&c, &off).unwrap_err().0.contains("Enabled=0"));
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn a_mod_from_before_dllchain_next_to_shroudforge_moves_into_its_folder_and_comes_back_on_uninstall() {
    let dir = game("shroudforge-adopted");
    // enstash put in by hand, then ShroudForge installed with dllchain: enstash is chained, where it never runs
    let hand = fake_dll(&["enstash Quick Stack", "0.11"], None);
    std::fs::write(dir.join("dinput8.dll"), &hand).unwrap();
    let sf = fake_dll(&["shroudforge", "EML_LOG_FILE_PATH"], None);
    let c = ctx(&dir);
    install(&c, &src("ShroudForge.dll", sf.clone(), &[]));
    assert_eq!(read(&dir, "dinput8.dll"), Some(sf.clone()));
    assert_eq!(read(&dir, "enstash.dll"), Some(hand.clone()));

    let p = install(&c, &enstash_zip(enstash(), b"dat"));
    assert!(
        p.summary.iter().any(|l| l.starts_with("Your copy of enstash that was enstash.dll before dllchain is kept")),
        "{:?}",
        p.summary
    );
    assert_eq!(read(&dir, "dinput8.dll"), Some(sf.clone()));
    assert!(!dir.join("enstash.dll").exists());
    assert_eq!(read(&dir, r"mods\enstash\dinput8.dll"), Some(enstash()));
    assert!(status(&c).unwrap().repair.is_empty());

    // Undo puts the hand copy back where it was
    let moved = snapshot(&dir);
    run(&c, plan_undo(&c));
    assert_eq!(read(&dir, "enstash.dll"), Some(hand.clone()));
    run(&c, plan_undo(&c));
    assert_eq!(without_own(&snapshot(&dir)), without_own(&moved));

    // Uninstall gives the hand copy back next to the game, under its own name
    run(&c, plan_uninstall(&c, Some("enstash")));
    assert_eq!(read(&dir, "enstash.dll"), Some(hand));
    assert!(!dir.join(r"mods\enstash").exists());
}

#[test]
fn shroudforge_with_another_loaders_eml_is_refused() {
    let dir = game("shroudforge-eml");
    shroudforge(&dir);
    // EML's own dbghelp.dll copied over ShroudForge's
    std::fs::write(dir.join("dbghelp.dll"), fake_dll(&["EML_LOG_FILE_PATH"], None)).unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);
    let e = plan_install(&c, &enstash_zip(enstash(), b"dat")).unwrap_err();
    assert!(e.0.contains("dbghelp.dll is EML, not ShroudForge's own"), "{}", e.0);
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn next_to_eml_enstash_chains_with_auto_loot_plus() {
    let dir = game("eml-enstash");
    std::fs::write(dir.join("dbghelp.dll"), fake_dll(&["EML_LOG_FILE_PATH"], None)).unwrap();
    std::fs::create_dir_all(dir.join("mods\\other")).unwrap();
    std::fs::write(dir.join("mods\\other\\mod.json"), b"{}").unwrap();
    let before = snapshot(&dir);
    let c = ctx(&dir);
    let p = install(&c, &src("AutoLootPlus.zip", alp(), &[(r"mods\auto-loot-plus\mod.json", b"{}")]));
    assert!(p.missing.is_empty());
    install(&c, &enstash_zip(enstash(), b"dat"));
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp()));
    assert_eq!(read(&dir, "enstash.dll"), Some(enstash()));
    assert!(!dir.join("mods\\enstash").exists());
    run(&c, plan_uninstall(&c, None));
    clean(&dir);
    assert_eq!(snapshot(&dir), before);
}

/// Sets a file's last-write time `secs` from now.
fn touch(path: &Path, secs: i64) {
    let now = std::time::SystemTime::now();
    let d = std::time::Duration::from_secs(secs.unsigned_abs());
    let t = if secs >= 0 { now + d } else { now - d };
    std::fs::File::options().write(true).open(path).unwrap().set_modified(t).unwrap();
}

#[test]
fn status_says_whether_enstash_loaded_at_the_last_start() {
    let dir = game("load-check");
    shroudforge(&dir);
    let c = ctx(&dir);
    // the DLL says which version it is; a version from the zip's name alone is never held against the log
    install(&c, &enstash_zip(fake_dll_with(&["enstash Quick Stack"], &[("ProductVersion", "0.12.2")]), b"dat"));
    let pkg = dir.join("mods\\enstash");
    let load = || row(&status(&c).unwrap(), "enstash").load.clone().unwrap();

    assert_eq!(load().0, Load::NotYet);
    // the game started, and enstash wrote nothing
    std::fs::write(dir.join("enshrouded.log"), b"game").unwrap();
    touch(&dir.join("enshrouded.log"), 60);
    assert_eq!(load().0, Load::NotLoaded);
    // a log from before the install says nothing about this one
    std::fs::write(pkg.join("enstash-quickstack.log"), b"05:55:27 enstash Quick Stack placeholders 0.12.2\r\n")
        .unwrap();
    touch(&pkg.join("enstash-quickstack.log"), -3600);
    assert_eq!(load().0, Load::NotLoaded);

    let log = |text: &str| {
        std::fs::write(pkg.join("enstash-quickstack.log"), text).unwrap();
        touch(&pkg.join("enstash-quickstack.log"), 60);
        load()
    };
    let ok = log("05:55:27 enstash Quick Stack placeholders 0.12.2\r\n05:55:30 hooked\r\n05:55:31 placeholders ON\r\n");
    assert_eq!(ok, (Load::Loaded, "loaded at the game's last start (0.12.2)".to_string()));
    let off = log(
        "05:55:27 enstash Quick Stack placeholders 0.12.2\r\n05:55:31 placeholders OFF (enstash-quickstack.off)\r\n",
    );
    assert_eq!(off.0, Load::Off);
    let older = log("05:55:27 enstash Quick Stack placeholders 0.11.0\r\n05:55:31 placeholders ON\r\n");
    assert_eq!(older.0, Load::NotLoaded);
    assert!(older.1.contains("says 0.11.0, not 0.12.2"), "{}", older.1);
    let stuck = log("05:55:27 enstash Quick Stack placeholders 0.12.2\r\n");
    assert_eq!(stuck.0, Load::NotLoaded);
    let st = status(&c).unwrap();
    assert!(row(&st, "enstash").details.iter().any(|(k, v)| k == "Last start" && v.contains("did not finish")));
}

#[test]
fn undo_takes_back_the_last_change_and_undo_again_makes_it_again() {
    let dir = game("undo");
    let c = ctx(&dir);
    assert!(plan_undo(&c).unwrap_err().0.contains("no change to undo"));
    let empty = without_own(&snapshot(&dir));

    install(&c, &src("AutoLootPlus.zip", alp(), &[("mods\\alp\\config.txt", b"cfg")]));
    let one = snapshot(&dir);
    install(&c, &enstash_zip(enstash(), b"dat"));
    let two = snapshot(&dir);
    let record = |s: &BTreeMap<String, Vec<u8>>| s.get("dllchain\\record.txt").cloned();
    assert_eq!(status(&c).unwrap().undo.unwrap().0, format!("install {ENSTASH_ZIP}"));

    let p = run(&c, plan_undo(&c));
    assert!(
        p.summary[0].starts_with(&format!("Takes back the last change: install {ENSTASH_ZIP} (")),
        "{:?}",
        p.summary
    );
    clean(&dir);
    assert_eq!(without_own(&snapshot(&dir)), without_own(&one));
    assert_eq!(record(&snapshot(&dir)), record(&one));
    assert!(status(&c).unwrap().repair.is_empty());

    // undo again: the change is made again
    run(&c, plan_undo(&c));
    assert_eq!(without_own(&snapshot(&dir)), without_own(&two));
    assert_eq!(record(&snapshot(&dir)), record(&two));

    // a file the change made, changed since: undo would lose it, so it is refused and nothing changes
    std::fs::write(dir.join("enstash-data.dat"), b"edited").unwrap();
    let now = snapshot(&dir);
    let e = plan_undo(&c).unwrap_err();
    assert!(e.0.contains("enstash-data.dat changed after the last change"), "{}", e.0);
    assert_eq!(snapshot(&dir), now);

    // undoing the very first install leaves the game as it was
    let dir = game("undo-first");
    let c = ctx(&dir);
    install(&c, &src("AutoLootPlus.zip", alp(), &[("mods\\alp\\config.txt", b"cfg")]));
    run(&c, plan_undo(&c));
    assert_eq!(without_own(&snapshot(&dir)), empty);
    assert!(status(&c).unwrap().rows.is_empty());
}

#[test]
fn a_version_in_a_file_name() {
    assert_eq!(version_in(ENSTASH_ZIP).as_deref(), Some("0.12.2-beta"));
    assert_eq!(version_in("C:\\x\\enstash-v0.12.2.zip").as_deref(), Some("0.12.2"));
    assert_eq!(version_in("Mod 1.4.zip").as_deref(), Some("1.4"));
    assert_eq!(version_in(r"C:\x\SmartQuickStack-0.12.2-beta\").as_deref(), Some("0.12.2-beta"));
    assert_eq!(version_in("AutoLootPlus-1-1-8.zip"), None);
    assert_eq!(version_in("x64mod.zip"), None);
    assert_eq!(version_in("mod.zip"), None);
}

#[test]
fn uninstalling_everything_lists_every_file_by_folder() {
    let dir = game("uninstall-list");
    let c = ctx(&dir);
    install(&c, &src("AutoLootPlus.zip", alp(), &[(r"mods\auto-loot-plus\config.txt", b"cfg")]));
    install(&c, &enstash_zip(enstash(), b"dat"));
    let p = plan_uninstall(&c, None).unwrap();
    let list = p.removed_by_folder();
    let folders: Vec<&str> = list.iter().map(|(f, _)| f.as_str()).collect();
    assert_eq!(folders, ["", r"licenses\", r"mods\auto-loot-plus\"]);
    assert_eq!(
        list[0].1,
        ["dinput8-chain.ini", "dinput8.dll", "enstash-data.dat", "enstash.dll", "mod.json", "native-plugin.ini"]
    );
    assert_eq!(list[2].1, ["config.txt"]);
    // dllchain's own files are not listed
    assert!(list.iter().flat_map(|(_, n)| n).all(|n| !n.contains("record")));
}

/// What enstash writes while it runs, next to its DLL, besides the player's files `enstash_user_files` makes.
fn enstash_run_files(dir: &Path) {
    std::fs::create_dir_all(dir.join("enstash-logs")).unwrap();
    std::fs::create_dir_all(dir.join(r"enstash-backups\2026-11-05")).unwrap();
    std::fs::write(dir.join(r"enstash-logs\run-1.log"), b"run").unwrap();
    std::fs::write(dir.join(r"enstash-backups\2026-11-05\chests.bin"), b"chests").unwrap();
    std::fs::write(dir.join("enstash-routes.txt"), b"routes").unwrap();
    std::fs::write(dir.join("enstash-report.txt"), b"report").unwrap();
    // enstash writes it on its first start (enstash item 78)
    std::fs::write(dir.join("enstash-first-start.txt"), b"first start").unwrap();
}

/// Files that start like enstash's but are not its to delete, and must outlive any uninstall.
fn not_enstash_files(dir: &Path) -> Vec<&'static str> {
    let files = [
        "enstash-tool.exe",
        "enstash-extra.dll",
        "enstash-extra.log",
        "enstashish.txt",
        "my-enstash.log",
        "enshrouded.log",
    ];
    for f in files {
        std::fs::write(dir.join(f), b"not enstash's").unwrap();
    }
    files.to_vec()
}

fn made_names(p: &Plan) -> Vec<String> {
    p.made
        .iter()
        .flat_map(|(_, m)| m)
        .map(|m| if m.is_dir { format!("{}\\", m.rel) } else { m.rel.to_string() })
        .collect()
}

#[test]
fn uninstall_deletes_what_the_mod_made_only_when_asked_and_undo_brings_it_back() {
    let dir = game("made-chained");
    let c = ctx(&dir);
    install(&c, &src("AutoLootPlus-1-1-8.zip", alp(), &[]));
    install(&c, &enstash_zip(enstash(), b"dat"));
    enstash_user_files(&dir);
    enstash_run_files(&dir);
    let traps = not_enstash_files(&dir);
    // a link that looks like enstash's: neither it nor what it points at is touched
    let outside = game("made-chained-outside");
    std::fs::write(outside.join("keep.txt"), b"elsewhere").unwrap();
    let linked = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(dir.join("enstash-link"))
        .arg(&outside)
        .output()
        .unwrap()
        .status
        .success();
    assert!(linked, "could not make a junction");

    let expected = [
        r"enstash-backups\",
        "enstash-first-start.txt",
        r"enstash-logs\",
        "enstash-marks.txt",
        "enstash-quickstack.log",
        "enstash-quickstack.off",
        "enstash-report.txt",
        "enstash-routes.txt",
        "enstash-settings.ini",
    ];
    // asked for nothing: listed, and left
    let p = run(&c, plan_uninstall(&c, Some("enstash")));
    assert_eq!(made_names(&p), expected);
    assert_eq!(p.made[0].0, "enstash");
    assert!(p.undoable() && !p.summary.iter().any(|l| l.starts_with("Deletes what")), "{:?}", p.summary);
    user_files_kept(&dir);
    assert_eq!(read(&dir, "enstash-routes.txt").as_deref(), Some(&b"routes"[..]));
    run(&c, plan_undo(&c));
    assert_eq!(read(&dir, "enstash.dll"), Some(enstash()));

    // asked: deleted, and nothing else
    let before = snapshot(&dir);
    let p = run(&c, plan_uninstall_with(&c, Some("enstash"), true));
    assert!(p.undoable());
    assert!(p.summary.iter().any(|l| l == "Deletes what enstash made while it ran (10 files)."), "{:?}", p.summary);
    for e in expected {
        assert!(!dir.join(e).exists(), "{e} is still there");
    }
    for t in &traps {
        assert_eq!(read(&dir, t).as_deref(), Some(&b"not enstash's"[..]), "{t}");
    }
    assert_eq!(read(&outside, "keep.txt").as_deref(), Some(&b"elsewhere"[..]));
    assert!(dir.join("enstash-link").exists());
    assert_eq!(read(&dir, "dinput8.dll"), Some(alp()));
    clean(&dir);

    // Auto Loot Plus is still there, so dllchain's copies are too: Undo brings it all back
    run(&c, plan_undo(&c));
    assert_eq!(without_own(&snapshot(&dir)), without_own(&before));
    user_files_kept(&dir);

    // the last mod: dllchain's folder goes, so it cannot be undone
    let p = run(&c, plan_uninstall_with(&c, None, true));
    assert!(!p.undoable());
    assert!(!dir.join("enstash-logs").exists() && !dir.join("dllchain").exists());
    assert!(!dir.join("enstash-marks.txt").exists());
    for t in &traps {
        assert!(dir.join(t).exists(), "{t}");
    }
    clean(&dir);
}

#[test]
fn under_shroudforge_what_the_mod_made_in_its_folder_goes_with_the_folder() {
    let dir = game("made-shroudforge");
    shroudforge(&dir);
    let c = ctx(&dir);
    install(&c, &enstash_zip(enstash(), b"dat"));
    std::fs::write(dir.join(r"mods\enstash\enstash-quickstack.log"), b"log").unwrap();
    std::fs::create_dir_all(dir.join(r"mods\enstash\enstash-backups")).unwrap();
    std::fs::write(dir.join(r"mods\enstash\enstash-backups\chests.bin"), b"chests").unwrap();
    std::fs::write(dir.join(r"mods\enstash\enstash-first-start.txt"), b"first start").unwrap();
    // from before ShroudForge, next to the game
    std::fs::write(dir.join("enstash-quickstack.log"), b"older log").unwrap();
    let p = plan_uninstall(&c, Some("enstash")).unwrap();
    assert_eq!(
        made_names(&p),
        [
            "enstash-quickstack.log",
            r"mods\enstash\enstash-backups\",
            r"mods\enstash\enstash-first-start.txt",
            r"mods\enstash\enstash-quickstack.log"
        ]
    );
    run(&c, plan_uninstall_with(&c, Some("enstash"), true));
    assert!(!dir.join(r"mods\enstash").exists());
    assert!(!dir.join("enstash-quickstack.log").exists());
    // ShroudForge and its packages are untouched
    assert!(dir.join(r"mods\sf-survival\mod.json").exists() && dir.join(r"shroudforge\settings.json").exists());
    clean(&dir);
}
