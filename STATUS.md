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
  This now holds for every kind of mod: `finish()` keeps the old install time of any entry that is otherwise unchanged.
  Before, a chained mod reinstalled across a second boundary rewrote the record and said "another tool moved a mod" (a flaky test showed it).
- Status and the window show these mods with the role "its own loader; the game loads it"; texts say "DLL mods" instead of "dinput8.dll mods".
- Not done, as decided: no language trimming.

### Checks

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`: clean.
- `cargo test --workspace --locked`, debug and release: 115 tests (43 chainkit lib, 61 engine, 6 window, 5 command line), up from 95.
- End to end on a copy of the game folder (game data left out), with both real zips:
  - adopting the hand-installed Town Portals (English voices only) and Auto Doors, then uninstalling both, gives back every file byte for byte;
  - Undo and Undo again work, and `--delete-logs` takes `greeneye\loader.log` and `town-portal.ini` (Undo brings them back);
  - a fresh install of both into a copy without them, then uninstall with `--delete-logs`, leaves the folder as it was;
  - `dinput8.dll`, `dinput8-chain.ini` (ALP hosting enstash) and EML's `dbghelp.dll` are unchanged throughout.
- Rerun after both reviews' fixes: the same results.
- Not checked: the window by eye with these mods listed (only its texts changed).
- Not done: the dry run on the real game folder, because the game and Steam were running (dllchain refuses then); the copy is the real folder without game data.

### Reviews

- `/code-review medium`: 10 findings, 8 fixed, each with a test.
  - Fixed: an update of a mod whose DLL is its loader (no version) was refused; a loader another mod left made the next mod a "hand install"; the dll_sha of mods sharing a re-added loader went stale; repair did not name missing found files; what a loader mod made could claim a hand-installed sibling folder; files another mod found counted as free; a `client\` folder inside a mod's own folders was taken for a client/server zip; `plan_repair` now walks all mods.
  - Not fixed: a record's `dll_at` with a proxy name passing `keep_safe` (the record is trusted only through its SHA-256 checks, as before); using `all()` everywhere (the other places need separate borrows).
- `/simplify`: 4 agents, 31 findings, 23 after dedup, 12 fixed, 1 already gone.
  - Fixed: the install time kept once in `finish()` for all install paths (packages had no such rule); the loader branches of `plan_install_own_loader` share `write_loader`/`keep_there` and hash the loader once; `ModRec::new` instead of six full literals; `Work::all_mut`; `newly_found` as a list; `most_in` with `first_part`; hash sets for "belongs to another mod", earlier found files and the checksum entries; `is_runtime_folder` by whole words (not `catalog\` or `templates\`); test helpers `tmp` and `put` shared.
  - Skipped (deeper refactors, later items below): `finish()` guessing "another tool moved a mod"; `found` as a flag on `FileRec` instead of a second list; one loader SHA instead of a copy per sharing mod; record order by `order`; created folders as one record-wide set; a `Work::take`; one folder walk shared by `foreign_dlls` and `made_in_own_folders`; hashing the zip's files once for checksums and install; the status rows re-hashing a shared loader; small `from_entries` tidy-ups.

### Later

- The layout code that follows another tool's move says so itself, and `finish()` stops guessing from an empty summary.
- `found` as an origin flag on `FileRec`, so no walker over a mod's files can forget it.
- Created folders kept record-wide and removed when no listed file is under them, instead of passing them to an heir mod.
- The record written in `order`, so the place of a mod in the work lists does not matter.
