# dllchain status

## 2026-11-07: a chain loader that arrives after another host (branch wip/loader-arrives)

XHL-Core's zip can now be installed when another `dinput8.dll` already hosts the chain (Auto Loot Plus, as the user has it), so XHL plugins such as Area Harvest run.

### Rules

- Method 1 (XHL-Core's own page): Core becomes `dinput8.dll`, the old host is renamed (`auto-loot-plus.dll`) and listed first in `dinput8-chain.ini`, so it still starts before the mods it started; the rest of the chain stays.
- `Loader.install` is `None` only for a loader dllchain installs (XHL-Core); EMF and ModLoader Proxy keep their "install it yourself" refusal.
  A loader is never installed over another loader or next to ShroudForge.
- `Forward=` is never written: Auto Loot Plus does all its work in `DllMain` (its source), so it needs no `DirectInput8Create`; Core's "auto" forward picks the first chain entry that exports it.
- A hand copy of the same Core, identical, is taken on with no file change and stays on uninstall; a newer one replaces it with a copy kept.
- Updating the old host (an ALP build installed with `install <its dinput8.dll>`) replaces only its chained `auto-loot-plus.dll`; Core stays `dinput8.dll`.
- Uninstalling Core gives `dinput8.dll` back to the mod listed first that can host, and the chain file comes back byte for byte.

### In the game (rig, enshrouded-chests `mm-xhl-core`, wip/rig-dev 7ab5265)

- Runs 20261107-190826 and 20261107-192230 (after the review fixes): EML and ALP by hand, enstash 0.15.0 with dllchain v0.3.0, then XHL-Core 2.0.6 and XHL-Area-Harvest 2.0.0 with this build; both passed, restore byte-identical.
- `XHL-Native-Plugin-Host.log`: "role=primary", chain loaded `auto-loot-plus.dll` and `enstash.dll`, "DirectInput8Create is forwarded to auto-loot-plus.dll", "loaded plugin: xhl-area-harvest 2.0.0".
- `auto-loot-plus.log`: "Patches angewendet"; enstash started as a chained DLL and Quick Stack ran.

### Follow-ups

- Remember the displaced host in the record instead of reading the first chain line on uninstall (simplify's altitude finding); a host that could not start others is not chosen back today.
- enstash's mod list calls Core "DINPUT8.dll (not a mod enstash knows)": teach enstash XHL-Core.
- No warning yet that a mod with `mods\<name>\native-plugin.ini` needs XHL-Core.

### Checks

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --release -- -D warnings`: clean.
- `cargo test --release`: 135 tests (48 chainkit lib, 76 engine, 6 window, 5 command line), rerun after both reviews' fixes.

### Reviews

- `/code-review medium`: 10 findings, 4 fixed (uninstall gives dinput8.dll back to the displaced host, only a loader dllchain installs takes the host's place, no "starts the other mods" line without other mods, one inspect per mod); the rest are refusals that are right, edge cases or existing behaviour.
- `/simplify`: 16 findings, 13 after dedup, 4 fixed (one parse of the chain via `scan.parsed`, `host_goes` once, `same_tool` simpler, named format captures); the rest skipped, one as the follow-up above.

## 2026-11-07: other loaders and mod managers (branch wip/other-loaders)

dllchain now keeps everything it does not own in another tool's list file, and adds mods to another loader's list instead of replacing it.

### Rules

- `chainini` keeps every line of the file as it was: other sections (`[EMF]`, `[Settings]`), `[Options]` with `Forward=`, comments, blank lines, order and each line's own line ending (CRLF, LF, none on the last line).
  dllchain only adds or removes its own `[Chain]` (or `[Load]`) lines; a new line takes the file's first line ending, a new file CRLF.
- A loader (`Game::loaders`: EMF, XHL-Core, ModLoader Proxy) as `dinput8.dll` is always the host: never moved, adopted, replaced or deleted, and its own zip is refused as a mod with "install it first".
  Every mod, even one that could host, is a guest in the loader's list file.
- ModLoader Proxy (from its source, github.com/Entrodor/modloader-proxy): `chainloader.ini` next to it, `[Settings]` and `[Load]`, `;`/`#` comments.
  dllchain lists mods under `[Load]`, which loads them whether `LoadFromFolder` is on or off; its default ModsFolder `mods` is EML's and ShroudForge's `mods\`, so no loose DLLs go there.
  Without a `chainloader.ini` dllchain makes one and deletes it again once it is empty.
- The record has a new line `chain-file\t<name>` when the list file is not `dinput8-chain.ini`.
  When the host changes (ModLoader Proxy put in by hand over a dllchain chain), repair moves dllchain's lines to the new list file, in their old order, and says so.
- Forward (`[Options] Forward=`, `[Settings] ForwardTo`): dllchain never edits it, as it is the loader's setting.
  When a run removes the file it names, the plan summary says so plainly before anything happens; status notes a forward that names a missing file; Undo brings the mod back.
- ShroudForge: unchanged (mods in `mods\<mod>\`), and its `dinput8-chain.ini` with `Forward=` stays byte for byte.
- Multi DLL Installer: `dinput<N>.dll` files are left alone and noted in status; a mod whose name would be `dinput<N>` gets `-mod`.
- Status has a host line ("XHL-Core loads the chain (dinput8-chain.ini).", "ShroudForge loads the mods from mods\<mod>\."), shown in the window and the command line, and entries say "started by <loader>".

### Not verified

- XHL-Core's detection is a guess: a DLL with both "dinput8-chain.ini" and "XHL" in it, unless a known mod tool matches; its file format beyond the chain and `[Options]` is assumed.
- EMF's `[EMF]` section contents are unknown; it is kept as opaque lines.
- Where ShroudForge keeps a forward, if it has one.
- How Multi DLL Installer's loader finds its `dinput<N>.dll` files.
- None of this was tried in the game or with the real loaders' DLLs; all of it is fake DLLs in fixture folders.

### Checks

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --release -- -D warnings`: clean.
- `cargo test --release`: 129 tests (46 chainkit lib, 72 engine, 6 window, 5 command line), up from 115; rerun after both reviews' fixes.
  New: round trips of six other tools' file shapes and ModLoader Proxy's ini; install, update, uninstall and undo with XHL-Core, EMF (with `[EMF]` and `[Options]`) and ModLoader Proxy (folder loading on and off), each ending byte for byte as it started; ModLoader Proxy without its ini, and put in by hand over a chain; a forward target removed (XHL `Forward`, ModLoader Proxy `ForwardTo`); ShroudForge with a forward; Multi DLL Installer names.

### Reviews

- `/code-review medium`: 10 findings, 7 fixed.
  - Fixed: a loader an older dllchain installed as a mod at `dinput8.dll` would have been renamed away (it now stays the host); XHL-Core was found by the bare text "XHL" (now its name, with the chain file); ModLoader Proxy by "chainloader.ini" alone, even in a known mod (now both its texts); the host line named a `dinput8.dll` that starts no others; the Multi DLL Installer note counted dllchain's own files; a forward outside the game folder was called missing; dllchain's own switched-off mod whose file went missing lost its row.
  - Not fixed: the old list's "dllchain made it" is forgotten when the user's own line keeps that file after a move (the file then stays, with the user's line, which is the safer side); a forward in the old list is not checked in the same run that moves the lines (rare); `Scan.loader` and `Plan.list`'s format as derived state (done in the simplify pass).
- `/simplify`: 4 agents, 25 findings, 14 after dedup, 8 fixed.
  - Fixed: one `Game::renamed_by_others` for Multi DLL Installer names, used by layout and status (status now lists the folder once instead of checking 32 names, and has no upper bound); `Scan::loader()` derived from the host instead of a stored copy; `Plan.list` holds only the file, its format comes from `Ctx::format_of`; `DllInfo::is_emf` removed; `say_forward_gone` names the host the way status does; `Game::list_files` became `is_list_file`; the status row match has one "switched off" arm; `inspect` checks the free condition before scanning for a loader's texts.
  - Skipped (deeper refactors, later items below): a `ListFile { name, format }` shared by `Game` and `Loader`; loaders as `Tool`s with an optional loader part instead of two tables and `own_texts`; one host-kind value in `Status` with its sentence, instead of `package_host`, `loader` and `host` plus the window rebuilding that sentence; the refused-line wording as a property of `Format`; `ChainFile::new`/`decode`/`Default` kept for tests; `counted` in one pass; the tests' `load_list` next to `chain`.

### Later

- A `ListFile { name, format }` for the chain file and each loader's list, so `format_of` and the scattered defaults go.
- Loaders as entries of `tools` with an optional loader part, one ordered first-match pass, no `own_texts`.
- One host kind in `Status` with its "a mod you install goes to ..." sentence, used by the window and the command line.
- A mod that bundles ModLoader Proxy under another proxy name with its own `chainloader.ini`, and ModLoader Proxy as a host under a name other than `dinput8.dll`.

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
