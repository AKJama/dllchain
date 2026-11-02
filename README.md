# dllchain

Installs several `dinput8.dll` mods for one game side by side, and takes them out again.

Many DLL mods ship as `dinput8.dll`, and a game folder can only hold one file with that name.
The community answer is a chain: the `dinput8.dll` in the game folder (the host) starts the other mods listed in `dinput8-chain.ini`.
dllchain sets that chain up for you, using the `dinput8-chain.ini` format other chain loaders already read, so it works alongside them.

Today it knows one game, Enshrouded (Steam app 1203620).
Another game is one more entry in `crates/chainkit/src/game.rs`.

## Use

Download the zip from the [releases](https://github.com/AKJama/dllchain/releases), unpack it anywhere, and run `dllchain.exe`.
Close the game and Steam first; dllchain changes nothing while either runs.

- **Install mod...** (or drop a mod's `.zip` or `dinput8.dll` on the window) adds a mod.
- **Uninstall selected** or **Uninstall all** take mods out again.
- **Repair** fixes the chain after another mod's `dinput8.dll` was copied over a chained one.
- **Rename** (or F2) changes the name a mod is shown under; no file is renamed.
- Select a mod to see everything dllchain knows about it: when it was installed, from which file and website, its SHA-256 and the files it added.

Every change is shown first and only made after you press OK.
The same actions are on the command line in `dllchain-cli.exe`:

```
dllchain-cli status
dllchain-cli install <mod.zip | dinput8.dll> [--yes] [--allow-replace]
dllchain-cli uninstall <name> | --all [--yes]
dllchain-cli rename <name> <new name> [--yes]
dllchain-cli repair [--yes]
```

Without `--yes` it only shows what it would do.

## What it does to the game folder

- One mod that can start others becomes `dinput8.dll`; every other mod is renamed to `<name>.dll` and listed in `dinput8-chain.ini`.
- When a mod loader that hosts every mod is installed, it stays `dinput8.dll` and every mod is chained behind it; dllchain never touches the loader's own files.
- A `dinput8.dll` that was already there, put in by hand or by another tool, is left as it is until you install another mod; then it joins the chain, and uninstalling gives it back.
- A file it replaces is copied to `dllchain\originals\` first, and put back on uninstall.
- What it installed is listed in `dllchain\record.txt`, and each run is logged in `dllchain\installer.log`.
- When the mod was downloaded with a browser, the website it came from is noted too (Windows keeps that with every download).
  Only the site and page are kept, never the part after `?`, which can hold your account id or a download key.
  It stays in the game folder; dllchain sends nothing anywhere.
- Before the first install it copies your saves once to `%LOCALAPPDATA%\dllchain\save-backups`.

It never writes programs (`.exe`), game data, Steam's files or another loader's folder, and never writes outside the game folder (apart from that one save copy).
Uninstall removes only what dllchain created, and gives back the rest byte for byte.
If a run fails or is cut off, everything it changed is undone, from `dllchain\journal.txt`, before anything else happens.

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
