//! dllchain-cli: the window's actions on the command line. Without `--yes` it only shows what it would change.

mod common;
mod update;

use chainkit::engine::{Ctx, Plan, plan_install, plan_rename, plan_repair, plan_undo, plan_uninstall_with, status};
use chainkit::hash::short;
use chainkit::{Error, Result, source};
use common::{GAME, Opts, describe, details_text, shown};
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
dllchain-cli: installs DLL mods side by side (dinput8.dll mods in one chain), and takes them out again.

  dllchain-cli status
  dllchain-cli install <mod.zip | mod's DLL> [--yes] [--allow-replace]
  dllchain-cli uninstall <name> | --all [--delete-logs] [--yes]
  dllchain-cli rename <name> <new name> [--yes]   (\"\" gives the mod its own name back)
  dllchain-cli repair [--yes]
  dllchain-cli undo [--yes]   (takes back the last change; undo again to make it again)
  dllchain-cli check-update   (asks GitHub for the newest version; nothing else is sent)

  <name> is the mod's file-name word, as `status` shows it (\"enstash\", \"auto-loot-plus\").

  --game <folder>        the game folder (default: found through Steam)
  --steam-root <folder>  Steam's folder (default: from the registry)
  --yes                  make the changes; without it dllchain only shows them
  --allow-replace        let an install replace files dllchain did not install (a copy is kept)
  --delete-logs          on uninstall, also delete the logs and other files the mod made while it ran

Exit codes: 0 done (or nothing to do), 1 refused or needs --yes/--allow-replace, 2 failed (everything undone).";

enum Cmd {
    Status,
    Install(PathBuf),
    Uninstall(Option<String>),
    Rename(String, String),
    Repair,
    Undo,
    CheckUpdate,
}

struct Args {
    cmd: Cmd,
    opts: Opts,
    yes: bool,
    allow_replace: bool,
    delete_logs: bool,
}

fn parse(args: Vec<String>) -> std::result::Result<Args, String> {
    let mut opts = Opts::default();
    let (mut yes, mut allow_replace, mut all, mut delete_logs) = (false, false, false, false);
    let mut free = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |name: &str| it.next().map(PathBuf::from).ok_or(format!("{name} needs a folder"));
        match a.as_str() {
            "--yes" | "-y" => yes = true,
            "--allow-replace" => allow_replace = true,
            "--all" => all = true,
            "--delete-logs" => delete_logs = true,
            "--game" => opts.game = Some(val("--game")?),
            "--steam-root" => opts.steam_root = Some(val("--steam-root")?),
            "--test-saved-games" => opts.saved_games = Some(val("--test-saved-games")?),
            "--test-save-store" => opts.save_store = Some(val("--test-save-store")?),
            "--test-skip-running-check" => opts.skip_running_check = true,
            "-h" | "--help" | "help" => return Err(String::new()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ => free.push(a),
        }
    }
    let cmd = match (free.first().map(String::as_str), free.len(), all) {
        (Some("status"), 1, false) => Cmd::Status,
        (Some("repair"), 1, false) => Cmd::Repair,
        (Some("undo"), 1, false) => Cmd::Undo,
        (Some("check-update"), 1, false) => Cmd::CheckUpdate,
        (Some("install"), 2, false) => Cmd::Install(PathBuf::from(&free[1])),
        (Some("uninstall"), 2, false) => Cmd::Uninstall(Some(free[1].clone())),
        (Some("uninstall"), 1, true) => Cmd::Uninstall(None),
        (Some("rename"), 3, false) => Cmd::Rename(free[1].clone(), free[2].clone()),
        (None, _, _) => return Err(String::new()),
        _ => return Err(format!("not a command dllchain knows: {}", free.join(" "))),
    };
    if delete_logs && !matches!(cmd, Cmd::Uninstall(_)) {
        return Err("--delete-logs goes with uninstall".to_string());
    }
    Ok(Args { cmd, opts, yes, allow_replace, delete_logs })
}

enum Outcome {
    Done,
    Refused(String),
}

fn run(a: &Args) -> Result<Outcome> {
    // needs no game folder
    if let Cmd::CheckUpdate = a.cmd {
        let found = update::check();
        if found.is_err() {
            return Err(Error(update::describe(&found)));
        }
        println!("{}", update::describe(&found));
        if let Ok(update::Found::Newer(_, page)) = &found {
            println!("Download page: {page}");
        }
        return Ok(Outcome::Done);
    }
    let (ctx, recovered) = a.opts.open()?;
    println!("{} folder: {}", GAME.name, shown(&ctx.dir));
    for n in recovered {
        println!("{n}");
    }
    let plan = match &a.cmd {
        Cmd::Status => {
            print_status(&ctx)?;
            return Ok(Outcome::Done);
        }
        Cmd::Install(p) => {
            let src = source::read(p, GAME)?;
            println!("Mod: {} (SHA-256 {})", src.label, src.sha256);
            println!("Compare that SHA-256 with the one the mod's author publishes before you install.");
            plan_install(&ctx, &src)?
        }
        Cmd::Uninstall(w) => plan_uninstall_with(&ctx, w.as_deref(), a.delete_logs)?,
        Cmd::Rename(w, n) => plan_rename(&ctx, w, Some(n))?,
        Cmd::Repair => plan_repair(&ctx)?,
        Cmd::Undo => plan_undo(&ctx)?,
        Cmd::CheckUpdate => unreachable!("handled above"),
    };
    print!("\n{}", describe(&plan));
    if plan.is_empty() {
        return Ok(Outcome::Done);
    }
    if let Cmd::Uninstall(None) = a.cmd {
        print!("\n{}", common::removed_text(&plan, usize::MAX).0);
    }
    // with --all --delete-logs, the list of removed files above has them already
    let listed = matches!(a.cmd, Cmd::Uninstall(None)) && a.delete_logs;
    if let Some((_, list, _)) = common::made_choice(&plan, usize::MAX).filter(|_| !listed) {
        print!("\n{list}");
        if !a.delete_logs {
            println!("They stay. Add --delete-logs to delete them too.");
        }
    }
    if !plan.replaces.is_empty() && !a.allow_replace {
        return Ok(Outcome::Refused("Add --allow-replace to replace those files.".into()));
    }
    if !a.yes {
        return Ok(Outcome::Refused("Nothing was changed. Add --yes to make these changes.".into()));
    }
    apply(a, &ctx, &plan)?;
    Ok(Outcome::Done)
}

fn apply(a: &Args, ctx: &Ctx, plan: &Plan) -> Result<()> {
    for l in a.opts.apply(ctx, plan)? {
        println!("{l}");
    }
    println!("Done. Every change is in dllchain\\installer.log in the game folder.");
    Ok(())
}

fn print_status(ctx: &Ctx) -> Result<()> {
    let st = status(ctx)?;
    if st.rows.is_empty() {
        println!("No DLL mods.");
    }
    for r in &st.rows {
        let sha = r.sha.as_deref().map_or("missing".to_string(), |s| short(s).to_string());
        let word = r.word.as_deref().map_or(String::new(), |w| format!(" ({w})"));
        let version = r.version.as_deref().map_or(String::new(), |v| format!(" {v}"));
        println!("\n{}{version}{word}  {}  [{}]  SHA-256 {sha}", r.name, r.file, r.role);
        for l in details_text(&r.details, false).lines() {
            println!("    {l}");
        }
    }
    if !st.rows.is_empty() {
        println!();
    }
    if let Some(h) = &st.host {
        println!("{h}");
    }
    for n in &st.notes {
        println!("Note: {n}");
    }
    if let Some((what, when)) = &st.undo {
        println!("Last change: {what} ({when}). `dllchain-cli undo` takes it back.");
    }
    if st.repair.is_empty() {
        println!("Nothing to repair.");
    } else {
        println!("A repair would:");
        for l in &st.repair {
            println!("  - {l}");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = match parse(std::env::args().skip(1).collect()) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(if e.is_empty() { 0 } else { 1 });
        }
    };
    match run(&args) {
        Ok(Outcome::Done) => ExitCode::SUCCESS,
        Ok(Outcome::Refused(why)) => {
            println!("\n{why}");
            ExitCode::from(1)
        }
        Err(Error(e)) => {
            eprintln!("\n{e}");
            // a failed run was undone; a refusal changed nothing
            ExitCode::from(if e.contains("every step was undone") { 2 } else { 1 })
        }
    }
}
