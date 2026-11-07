# dllchain status

## 2026-11-07: mods that bring their own loader (branch wip/own-loader-mods)

Town Portals (Greeneye Loader as `dbgcore.dll`) and Auto Doors (`powrprof.dll`) were refused with "has no dinput8.dll".
They now install, generically, for any mod whose DLL has one of the game's loader names (`Game::proxy_names`) other than `dinput8.dll`.

### Rules

- The loader keeps its name and stays out of `dinput8-chain.ini`; the mod's other files go where the zip puts them.
- Mods on the same loader share it: an identical file is kept and listed by the first mod, the next ones share it, and it goes with the last one.
  A DLL in the mod's folders that dllchain did not install (another mod for that loader, put in by hand) keeps the loader on uninstall too.
- A different file under that name: the same loader family (tool word) and a newer version replaces it, with the old one in `dllchain\originals\<word>\`; the same or older keeps yours.
  Any other program under that name (EML's `dbghelp.dll`) is refused, and so is a loader that would stop ShroudForge's folders.
- A zip with `client\` and `server\` installs only `client\` and says so; a single top folder is unwrapped as before.
- Read-me, licence and checksum files at the top of a zip stay out of the game folder, like those next to the DLL.
- Checksum lists (`SHA256.json`, `SHA256SUMS`, `SHA256SUMS.txt`, GNU or BSD lines) are checked for every installed file.
  A mismatch, or a listed file missing from the download, refuses the install; a listed archive (the release's own zip) may be missing; a list dllchain cannot read gives a note.
- Adoption: files already in the game, identical to the zip's, are recorded as `found` (new record line `found\t<word>\t<rel>\t<sha>`).
  Uninstall leaves them, so a hand-installed mod comes back exactly as it was; files the zip adds (Town Portals' other voices) go again.
- What the loader and its mods write in their own folders (`greeneye\loader.log`, `greeneye\town-portal\town-portal.ini`, `AutoDoors\logs\`) counts as files the mod made, for the "delete logs" tick box.
- A folder a mod made that another mod's files are in now passes to that mod on uninstall, so the last one out removes it.
- Reinstalling the same zip changes nothing (the record entry keeps its place and time).
- Status and the window show these mods with the role "its own loader; the game loads it"; texts say "DLL mods" instead of "dinput8.dll mods".
- Not done, as decided: no language trimming.

### Checks

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo test --workspace --locked`, debug and release: 110 tests (43 chainkit lib, 56 engine, 6 window, 5 command line), up from 95.
- End to end on a copy of the game folder (game data left out), with both real zips:
  - adopting the hand-installed Town Portals (English voices only) and Auto Doors, then uninstalling both, gives back every file byte for byte;
  - Undo and Undo again work, and `--delete-logs` takes `greeneye\loader.log` and `town-portal.ini` (Undo brings them back);
  - a fresh install of both into a copy without them, then uninstall with `--delete-logs`, leaves the folder as it was;
  - `dinput8.dll`, `dinput8-chain.ini` (ALP hosting enstash) and EML's `dbghelp.dll` are unchanged throughout.
- Not checked: the window by eye with these mods listed (only its texts changed).

### Reviews

- `/code-review medium`: pending.
- `/simplify`: pending.
