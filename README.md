# dllchain

Installs several `dinput8.dll` mods for one game side by side, and takes them out again.

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
5. Do the same for each other `dinput8.dll` mod.
   A `dinput8.dll` you already put in by hand is kept and joins the chain.
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

- **Install mod...** (or drop a mod's `.zip` or `dinput8.dll` on the window) adds a mod.
  For a mod you already unpacked, pick its `dinput8.dll`: the files next to it (such as its `mods\` folder) are installed with it.
  A folder that also holds programs, archives, pictures or documents, such as Downloads itself, is refused, so unpack each mod into a folder of its own.
  Licence, checksum and read-me files next to the mod's `dinput8.dll` (such as `SHA256SUMS.txt`) are for you, not the game, so they are never copied into the game folder.
  When the mod is also a script package for the [Enshrouded Mod Loader (EML)](https://brabb3l.github.io/kfc-parser/eml/usage.html) (it ships `mods\<name>\mod.json`) and EML is not installed, dllchain says so, with a link to EML's page.
  It installs the mod anyway and never downloads EML itself.
- **Uninstall** or **Uninstall all** take mods out again.
  Uninstall all first asks whether you are sure, and lists every file it removes and the folder it is in; Cancel changes nothing.
  It cannot be undone, as dllchain's own folder goes too.
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
dllchain-cli install <mod.zip | dinput8.dll> [--yes] [--allow-replace]
dllchain-cli uninstall <name> | --all [--yes]
dllchain-cli rename <name> <new name> [--yes]
dllchain-cli repair [--yes]
dllchain-cli undo [--yes]
dllchain-cli check-update
```

Without `--yes` it only shows what it would do.

## What it does to the game folder

- One mod that can start others becomes `dinput8.dll`; every other mod is renamed to `<name>.dll` and listed in `dinput8-chain.ini`.
- When a mod loader that hosts every mod is installed, it stays `dinput8.dll` and every mod is chained behind it; dllchain never touches the loader's own files.
- When ShroudForge is installed, it stays `dinput8.dll` and each mod goes into a folder of its own, `mods\<name>\`, where ShroudForge loads it; no chain is used.
  A mod that ships a `native-plugin.ini` (which says which DLL to load), as enstash does, goes into that folder whole.
  A `mod.json` from the zip is left out of that folder, as ShroudForge does not load a native mod from a folder that has one.
  A mod made for the game folder, such as Auto Loot Plus, keeps its other files where its zip puts them; its DLL goes into a folder of its own (`mods\<name>-dll\` when the zip already uses `mods\<name>\`), and dllchain writes the `native-plugin.ini` there.
  A mod that an older dllchain chained next to ShroudForge, where it never ran, is moved into its folder when you install it again.
  So is a mod that was there before dllchain: dllchain keeps its copy, and Uninstall puts it back.
  ShroudForge next to another loader's `dbghelp.dll` (EML's own) is refused with an explanation, as the two cannot run together.
- A file an older version of a mod left behind and the new version no longer reads (such as enstash's `enstash-data.txt` once `enstash-data.dat` is there) is taken out on install, kept in `dllchain\originals\`, and put back on uninstall.
  Your own files next to a mod (its marks, settings, `.off` switches, backups and logs) are never touched.
- A `dinput8.dll` that was already there, put in by hand or by another tool, is left as it is until you install another mod; then it joins the chain, and uninstalling gives it back.
- A file it replaces is copied to `dllchain\originals\` first, and put back on uninstall.
- What it installed is listed in `dllchain\record.txt`, and each run is logged in `dllchain\installer.log`.
- When the mod was downloaded with a browser, the website it came from is noted too (Windows keeps that with every download).
  Only the site and page are kept, never the part after `?`, which can hold your account id or a download key.
  It stays in the game folder.
- Before the first install it copies your saves once to `%LOCALAPPDATA%\dllchain\save-backups`.

dllchain sends nothing anywhere, except when you check for updates: then it asks GitHub for the newest version number, and nothing about your PC or game goes with it.

It never writes programs (`.exe`), game data, Steam's files or another loader's own files, and never writes outside the game folder (apart from that one save copy).
Uninstall removes only what dllchain created, and gives back the rest byte for byte.
If a run fails or is cut off, everything it changed is undone, from `dllchain\journal.txt`, before anything else happens.
The last finished run is kept in `dllchain\undo.txt`, with copies of what it changed, so Undo can take it back.

dllchain does not trust its own record: a mod could have edited it.
Entries that point at a protected file, outside the game folder, or at anything dllchain could not have made are dropped with a note.

## Is this download the real one?

The releases are built by GitHub Actions from the tagged source, never on someone's machine.
Each release has `SHA256SUMS.txt`, and a build attestation you can check with:

```
gh attestation verify dllchain.exe --repo AKJama/dllchain
```

dllchain also shows the SHA-256 of every mod you install.
Compare it with the one the mod's author publishes.

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
