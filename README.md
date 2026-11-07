# dllchain

Installs several DLL mods for one game side by side, and takes them out again.
Most ship as `dinput8.dll` and go into one chain; a mod that brings its own loader under another name (such as `dbgcore.dll` or `powrprof.dll`) is installed as it is, next to them.

## First time? Step by step

1. Close the game, then close Steam too (Steam > Exit).
   dllchain changes nothing while either runs.
2. Download the dllchain zip from the [latest release](https://github.com/AKJama/dllchain/releases/latest) and unzip it into a folder of its own, such as `Documents\dllchain`.
   Not into the game folder.
3. Run `dllchain.exe` from that folder.
   It finds the game through Steam.
   `dllchain-cli.exe` is the same for the command line; you don't need it.
4. Click **Install mod...** and pick a mod's zip as you downloaded it (no need to unzip it).
   dllchain says what it will do; press **Install**.
5. Do the same for each other mod.
   A `dinput8.dll` you already put in by hand is kept and joins the chain.
   A mod with its own loader that you already put in by hand is taken on as it is when you install its zip, and Uninstall leaves it as it was.
6. There is no ini to edit: dllchain writes `dinput8-chain.ini` itself.
7. Start Steam and the game.
   Each mod's own log says whether it loaded.

Never copy one mod's `dinput8.dll` over another's: the mod that was there is switched off without any error.

Many DLL mods ship as `dinput8.dll`, and a game folder can only hold one file with that name.
The community answer is a chain: the `dinput8.dll` in the game folder (the host) starts the other mods listed in `dinput8-chain.ini`.
dllchain sets that chain up for you, using the `dinput8-chain.ini` format other chain loaders already read, so it works alongside them.

Today it knows one game, Enshrouded (Steam app 1203620).
Another game is one more entry in `crates/chainkit/src/game.rs`.

## Use

Download the zip from the [releases](https://github.com/AKJama/dllchain/releases), unpack it anywhere, and run `dllchain.exe`.
Close the game and Steam first; dllchain changes nothing while either runs.

- **Install mod...** (or drop a mod's `.zip` or DLL on the window) adds a mod.
  For a mod you already unpacked, pick its `dinput8.dll` (or its own loader, such as `powrprof.dll`): the files next to it (such as its `mods\` folder) are installed with it.
  A folder that also holds programs, archives, pictures or documents, such as Downloads itself, is refused, so unpack each mod into a folder of its own.
  Licence, checksum and read-me files next to the mod's DLL or at the top of its zip (such as `SHA256SUMS.txt`) are for you, not the game, so they are never copied into the game folder.
  When the zip carries a checksum list (`SHA256.json`, `SHA256SUMS` or `SHA256SUMS.txt`), every file dllchain installs is checked against it first, and a file that does not match, or is listed but missing, stops the install.
  A zip with a `client\` and a `server\` folder installs only `client\`, and says so; `server\` is for a dedicated server.
  When the mod is also a script package for the [Enshrouded Mod Loader (EML)](https://brabb3l.github.io/kfc-parser/eml/usage.html) (it ships `mods\<name>\mod.json`) and EML is not installed, dllchain says so, with a link to EML's page.
  It installs the mod anyway and never downloads EML itself.
- **Uninstall** or **Uninstall all** take mods out again.
  Uninstall all first asks whether you are sure, and lists every file it removes and the folder it is in; Cancel changes nothing.
  It cannot be undone, as dllchain's own folder goes too.
  The logs, backups and reports a mod made while it ran (files and folders next to it whose names start with its name, such as `enstash-logs\`) stay, unless you tick "Also delete ... log and the other files it made" in the question, which lists them and where they are.
  Undo brings them back, except after Uninstall all or uninstalling the last mod.
  dllchain leaves anything it cannot tell is the mod's, and never touches saves, programs, DLLs or another mod's files.
- **Undo** takes back the last change (an install, an update, an uninstall, a repair or a rename), from the copies dllchain kept of every file it changed.
  Press Undo again to make the change again.
  When a file was changed by hand after that change, Undo refuses rather than lose your edit.
- **Repair** fixes the chain after another mod's `dinput8.dll` was copied over a chained one.
  A chained mod you replaced by hand with a newer build of the same mod is kept, not put back: dllchain keeps a copy of your build from then on.
- **Rename** (or F2) changes the name a mod is shown under; no file is renamed.
- **Mod page** opens the page the selected mod was downloaded from, when Windows noted one.
  To update a mod, download the new zip there and install it over the old one: it keeps its place in the chain, and uninstalling still gives back what was there before.
  An update that is only the new `dinput8.dll` keeps the mod's other files from the earlier version; only a full new version can leave files out, and the question says which go.
- **Help > Check for updates** asks GitHub whether a newer dllchain is out and offers to open its page.
  dllchain never downloads or installs anything by itself.
- Select a mod to see everything dllchain knows about it: when it was installed, from which file and website, its version, its SHA-256 and the files it added.
  The version comes from the DLL's version information, or else from the zip's name (`SmartQuickStack-0.12.2-beta.zip` is 0.12.2-beta).
- For a mod that writes a log dllchain knows (such as enstash), the list also says whether it loaded at the game's last start, and says plainly when it did not.

Every change is shown first, in plain words, and only made after you press Install, Uninstall, Repair or Undo.
**Show details** in that question shows the mod's SHA-256 and the first files that change; `dllchain\installer.log` in the game folder lists every one.
The same actions are on the command line in `dllchain-cli.exe`:

```
dllchain-cli status
dllchain-cli install <mod.zip | mod's DLL> [--yes] [--allow-replace]
dllchain-cli uninstall <name> | --all [--delete-logs] [--yes]
dllchain-cli rename <name> <new name> [--yes]
dllchain-cli repair [--yes]
dllchain-cli undo [--yes]
dllchain-cli check-update
```

Without `--yes` it only shows what it would do.

## What it does to the game folder

- One mod that can start others becomes `dinput8.dll`; every other mod is renamed to `<name>.dll` and listed in `dinput8-chain.ini`.
- When a loader for other mods is `dinput8.dll` (EMF, XHL-Core, ModLoader Proxy), it stays `dinput8.dll` and every mod is listed in its own list file; dllchain never touches the loader itself (see "Works with").
  XHL-Core's zip is the one loader zip dllchain installs: Core becomes `dinput8.dll`, and the mod that was `dinput8.dll` before is renamed and listed first, so it still starts first.
- When ShroudForge is installed, it stays `dinput8.dll` and each mod goes into a folder of its own, `mods\<name>\`, where ShroudForge loads it; no chain is used.
  A mod that ships a `native-plugin.ini` (which says which DLL to load), as enstash does, goes into that folder whole.
  A `mod.json` from the zip is left out of that folder, as ShroudForge does not load a native mod from a folder that has one.
  A mod made for the game folder, such as Auto Loot Plus, keeps its other files where its zip puts them; its DLL goes into a folder of its own (`mods\<name>-dll\` when the zip already uses `mods\<name>\`), and dllchain writes the `native-plugin.ini` there.
  A mod that an older dllchain chained next to ShroudForge, where it never ran, is moved into its folder when you install it again.
  So is a mod that was there before dllchain: dllchain keeps its copy, and Uninstall puts it back.
  ShroudForge next to another loader's `dbghelp.dll` (EML's own) is refused with an explanation, as the two cannot run together.
- A file an older version of a mod left behind and the new version no longer reads (such as enstash's `enstash-data.txt` once `enstash-data.dat` is there) is taken out on install, kept in `dllchain\originals\`, and put back on uninstall.
  Your own files next to a mod (its marks, settings, `.off` switches, backups and logs) are never touched.
- A mod that brings its own loader under another name the game loads (Town Portals' `dbgcore.dll`, Greeneye's loader; Auto Doors' `powrprof.dll`) keeps that name and its folders, and stays out of the chain: the game loads it by itself.
  Mods that use the same loader share it, and it goes with the last of them.
  A newer build of the same loader replaces an older one (the older one is kept in `dllchain\originals\` and comes back on uninstall); an older or equal one leaves yours in place.
  A different program under that name, such as EML's `dbghelp.dll`, is never replaced: the install is refused with an explanation.
  So is a loader that would stop ShroudForge's folders from loading.
  The files such a loader and its mods write while they run (`greeneye\loader.log`, `greeneye\town-portal\town-portal.ini`) count as files the mod made.
- Files that were already there, the same as in the zip (a mod you put in by hand), are listed but stay yours: Uninstall leaves them, so the folder is as it was before.
- A `dinput8.dll` that was already there, put in by hand or by another tool, is left as it is until you install another mod; then it joins the chain, and uninstalling gives it back (a loader for other mods stays `dinput8.dll` instead).
- A file it replaces is copied to `dllchain\originals\` first, and put back on uninstall.
- What it installed is listed in `dllchain\record.txt`, and each run is logged in `dllchain\installer.log`.
- When the mod was downloaded with a browser, the website it came from is noted too (Windows keeps that with every download).
  Only the site and page are kept, never the part after `?`, which can hold your account id or a download key.
  It stays in the game folder.
- Before the first install it copies your saves once to `%LOCALAPPDATA%\dllchain\save-backups`.

dllchain sends nothing anywhere, except when you check for updates: then it asks GitHub for the newest version number, and nothing about your PC or game goes with it.

It never writes programs (`.exe`), game data, Steam's files or another loader's own files (a newer build of the loader a mod brings aside, with a copy kept), and never writes outside the game folder (apart from that one save copy).
Uninstall removes only what dllchain created, and gives back the rest byte for byte.
The files a mod made while it ran go only when you ask (the tick box, or `--delete-logs`).
If a run fails or is cut off, everything it changed is undone, from `dllchain\journal.txt`, before anything else happens.
The last finished run is kept in `dllchain\undo.txt`, with copies of what it changed, so Undo can take it back.

dllchain does not trust its own record: a mod could have edited it.
Entries that point at a protected file, outside the game folder, or at anything dllchain could not have made are dropped with a note.

## Works with

dllchain shares the game folder with the other Enshrouded loaders and mod managers.
Status (and the window) says which one is in charge, in a line such as "XHL-Core loads the chain (dinput8-chain.ini)."

- **XHL-Core and EMF** (as `dinput8.dll`): they stay `dinput8.dll`, and dllchain adds your mods to their `dinput8-chain.ini`.
- **ModLoader Proxy** (renamed to `dinput8.dll`): it stays, and dllchain lists your mods under `[Load]` in its `chainloader.ini` (it makes one if there is none, and takes it away again when it is empty).
  That works whether `LoadFromFolder` is on or off, so dllchain does not drop loose DLLs into its mods folder, which by default is the same `mods\` folder other tools use.
- **ShroudForge**: each mod goes into a folder of its own under `mods\`, where ShroudForge loads it; `dinput8-chain.ini` is left as it is.
- **Multi DLL Installer**: the mods it renamed to `dinput9.dll`, `dinput10.dll` and so on are left as they are and noted in status; dllchain never gives a mod such a name.
- **XHL-Core** can also come after your mods: install its zip and it becomes `dinput8.dll`, the mod that was `dinput8.dll` (such as Auto Loot Plus) is renamed and listed first in `dinput8-chain.ini`, and the rest of the chain stays.
  XHL plugins (such as Area Harvest, which ships `mods\<name>\native-plugin.ini`) then install as usual, and Core loads them.
  Uninstalling Core puts the old `dinput8.dll` and the chain back as they were.
- Any other loader's own zip is not installed as a mod: install the loader by hand first, then dllchain adds your mods to its list.
  dllchain never puts one loader in place of another, or a loader next to ShroudForge.

In any list file, dllchain only adds and removes its own mods' lines.
Everything else stays as it was: other sections (such as `[EMF]` or `[Settings]`), `[Options]` and its `Forward=` line, comments, blank lines, the order and each line's line ending.
New lines take the file's own line ending.
Uninstall gives the file back byte for byte, apart from dllchain's lines.

`Forward=` (in `[Options]`) and ModLoader Proxy's `ForwardTo` hand the game's own calls to another DLL.
They are the loader's settings, so dllchain never changes them.
When you remove the mod such a line names, the plan says so plainly before anything happens, and status keeps a note while the line names a file that is not there; change or remove the line yourself, or Undo to get the mod back.

## Is this download the real one?

The releases are built by GitHub Actions from the tagged source, never on someone's machine.
Each release has `SHA256SUMS.txt`, and a build attestation you can check with:

```
gh attestation verify dllchain.exe --repo AKJama/dllchain
```

dllchain also shows the SHA-256 of every mod you install.
Compare it with the one the mod's author publishes.
When the mod's zip has its own checksum list, dllchain checks the files against it as well.

## Build

You need Rust 1.85 or newer on Windows.

```
cargo test --workspace
cargo build --release
```

`crates/chainkit` is the engine and has the tests; it runs against fake game folders and never needs the game.
`crates/dllchain` is the window (`dllchain.exe`) and the command line (`dllchain-cli.exe`).

## Licence

MIT, see [LICENSE](LICENSE).
Some tool names in `game.rs` come from another MIT project; its notice is in [THIRD-PARTY-NOTICES.txt](THIRD-PARTY-NOTICES.txt).
