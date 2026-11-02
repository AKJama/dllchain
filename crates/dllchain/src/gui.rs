//! dllchain: a small window to install, uninstall and repair dinput8.dll mods. Every change is shown first and only
//! made after the player says yes.

#![windows_subsystem = "windows"]

mod common;

use chainkit::engine::{Ctx, Plan, Row, plan_install, plan_repair, plan_uninstall, status};
use chainkit::hash::short;
use chainkit::{Error, Result, source, steam};
use common::{GAME, Opts, describe};
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    COLOR_BTNFACE, COLOR_WINDOW, COLOR_WINDOWTEXT, CreateFontIndirectW, DeleteObject, GetSysColor, GetSysColorBrush,
    HDC, HFONT, SetBkColor, SetTextColor, UpdateWindow,
};
use windows_sys::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Controls::{
    ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVIF_TEXT, LVITEMW, LVM_DELETEALLITEMS, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW, LVM_INSERTITEMW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMTEXTW, LVNI_SELECTED, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NM_DBLCLK, NMHDR, SetWindowTheme,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
    SystemParametersInfoForDpi,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows_sys::Win32::UI::Shell::{
    BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW, DragAcceptFiles, DragFinish, DragQueryFileW, HDROP,
    SHBrowseForFolderW, SHGetPathFromIDListW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const ID_GAME: i32 = 101;
const ID_CHANGE: i32 = 102;
const ID_LIST: i32 = 103;
const ID_INSTALL: i32 = 104;
const ID_UNINSTALL: i32 = 105;
const ID_UNINSTALL_ALL: i32 = 106;
const ID_REPAIR: i32 = 107;
const ID_OPEN: i32 = 108;
const ID_LOG: i32 = 109;
const WM_START: u32 = WM_APP + 1;

#[derive(Clone, Copy)]
struct Ctl {
    main: HWND,
    game_label: HWND,
    game: HWND,
    change: HWND,
    list_label: HWND,
    list: HWND,
    buttons: [HWND; 5],
    log: HWND,
    font: HFONT,
}

struct Data {
    opts: Opts,
    rows: Vec<Row>,
    /// A mod given on the command line, installed once the window is up.
    pending: Option<PathBuf>,
}

thread_local! {
    static CTL: Cell<Option<Ctl>> = const { Cell::new(None) };
    static DATA: RefCell<Option<Data>> = const { RefCell::new(None) };
}

fn ctl() -> Ctl {
    CTL.with(|c| c.get()).expect("the window exists")
}

fn opts() -> Opts {
    DATA.with(|d| d.borrow().as_ref().map(|d| d.opts.clone()).unwrap_or_default())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// Text for an edit control or a message box: Windows line ends.
fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn set_text(h: HWND, s: &str) {
    let w = wide(&crlf(s));
    // SAFETY: `h` is one of our controls and `w` is NUL-terminated.
    unsafe { SetWindowTextW(h, w.as_ptr()) };
}

fn message(title: &str, text: &str, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    let (t, x) = (wide(title), wide(&crlf(text)));
    // SAFETY: the strings outlive the call.
    unsafe { MessageBoxW(ctl().main, x.as_ptr(), t.as_ptr(), flags) }
}

fn dpi() -> i32 {
    // SAFETY: `main` is our window.
    let d = unsafe { GetDpiForWindow(ctl().main) } as i32;
    if d == 0 { 96 } else { d }
}

fn make_font(dpi: u32) -> HFONT {
    // SAFETY: NONCLIENTMETRICSW is plain data, sized as the call expects.
    unsafe {
        let mut ncm: NONCLIENTMETRICSW = std::mem::zeroed();
        ncm.cbSize = size_of::<NONCLIENTMETRICSW>() as u32;
        SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS, ncm.cbSize, (&raw mut ncm).cast(), 0, dpi);
        CreateFontIndirectW(&ncm.lfMessageFont)
    }
}

fn child(parent: HWND, class: &str, text: &str, style: u32, ex: u32, id: i32) -> HWND {
    let (c, t) = (wide(class), wide(text));
    // SAFETY: plain control creation; the strings outlive the call.
    unsafe {
        CreateWindowExW(
            ex,
            c.as_ptr(),
            t.as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            0,
            0,
            parent,
            id as isize as *mut c_void,
            GetModuleHandleW(null()),
            null(),
        )
    }
}

fn create_controls(main: HWND) {
    let button = |text: &str, id: i32| child(main, "BUTTON", text, WS_TABSTOP | BS_PUSHBUTTON as u32, 0, id);
    // SAFETY: `main` is the window being created.
    let font = make_font(unsafe { GetDpiForWindow(main) });
    let c = Ctl {
        main,
        game_label: child(main, "STATIC", "Game folder:", 0, 0, 0),
        game: child(
            main,
            "EDIT",
            "",
            WS_TABSTOP | ES_AUTOHSCROLL as u32 | ES_READONLY as u32,
            WS_EX_CLIENTEDGE,
            ID_GAME,
        ),
        change: button("&Change...", ID_CHANGE),
        list_label: child(main, "STATIC", "dinput8.dll mods in the game folder:", 0, 0, 0),
        list: child(
            main,
            "SysListView32",
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            WS_EX_CLIENTEDGE,
            ID_LIST,
        ),
        buttons: [
            button("&Install mod...", ID_INSTALL),
            button("&Uninstall selected", ID_UNINSTALL),
            button("Uninstall &all", ID_UNINSTALL_ALL),
            button("&Repair", ID_REPAIR),
            button("&Open game folder", ID_OPEN),
        ],
        log: child(
            main,
            "EDIT",
            "",
            WS_TABSTOP | WS_VSCROLL | ES_MULTILINE as u32 | ES_READONLY as u32 | ES_AUTOVSCROLL as u32,
            WS_EX_CLIENTEDGE,
            ID_LOG,
        ),
        font,
    };
    CTL.with(|x| x.set(Some(c)));
    set_fonts(&c);
    let ex = (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize;
    let explorer = wide("Explorer");
    // SAFETY: our list view; each column text is copied by the control.
    unsafe {
        SendMessageW(c.list, LVM_SETEXTENDEDLISTVIEWSTYLE, ex as usize, ex);
        SetWindowTheme(c.list, explorer.as_ptr(), null());
        for (i, name) in COLUMNS.iter().enumerate() {
            let mut text = wide(name.0);
            let col =
                LVCOLUMNW { mask: LVCF_TEXT | LVCF_WIDTH, cx: 100, pszText: text.as_mut_ptr(), ..std::mem::zeroed() };
            SendMessageW(c.list, LVM_INSERTCOLUMNW, i, (&raw const col) as isize);
        }
    }
}

/// The list's columns, with their width in pixels at 96 DPI (the last one takes the rest).
const COLUMNS: [(&str, i32); 5] =
    [("File", 120), ("Mod", 130), ("Role", 150), ("SHA-256", 110), ("Where it came from", 0)];

fn size_columns() {
    let c = ctl();
    let d = dpi();
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: our list view.
    unsafe { GetClientRect(c.list, &mut r) };
    let fixed: i32 = COLUMNS.iter().map(|(_, w)| w * d / 96).sum();
    for (i, (_, w)) in COLUMNS.iter().enumerate() {
        // the last column takes what is left, so there is no sideways scrolling
        let w = if *w == 0 { (r.right - fixed).max(100 * d / 96) } else { w * d / 96 };
        // SAFETY: our list view.
        unsafe { SendMessageW(c.list, LVM_SETCOLUMNWIDTH, i, w as isize) };
    }
}

fn set_fonts(c: &Ctl) {
    for h in [c.game_label, c.game, c.change, c.list_label, c.list, c.log].into_iter().chain(c.buttons) {
        // SAFETY: our controls and our font.
        unsafe { SendMessageW(h, WM_SETFONT, c.font as usize, 1) };
    }
}

fn layout() {
    let c = ctl();
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    // SAFETY: `main` is our window.
    unsafe { GetClientRect(c.main, &mut r) };
    let d = dpi();
    let s = |v: i32| v * d / 96;
    let (w, h) = (r.right, r.bottom);
    let (m, bh, gap) = (s(12), s(28), s(8));
    let place = |hw: HWND, x: i32, y: i32, cw: i32, ch: i32| {
        // SAFETY: our control.
        unsafe { MoveWindow(hw, x, y, cw.max(0), ch.max(0), 1) };
    };
    let change_w = s(96);
    place(c.game_label, m, m + s(6), s(84), s(20));
    place(c.change, w - m - change_w, m, change_w, bh);
    place(c.game, m + s(88), m + s(2), w - m - change_w - gap - (m + s(88)), s(24));
    let y = m + bh + s(12);
    place(c.list_label, m, y, w - 2 * m, s(20));
    let list_top = y + s(22);
    let rest = h - list_top - m - bh - 2 * s(10);
    let list_h = (rest * 45 / 100).max(s(60));
    place(c.list, m, list_top, w - 2 * m, list_h);
    let by = list_top + list_h + s(10);
    let bw = ((w - 2 * m - 4 * gap) / 5).min(s(150));
    for (i, b) in c.buttons.iter().enumerate() {
        place(*b, m + i as i32 * (bw + gap), by, bw, bh);
    }
    let ly = by + bh + s(10);
    place(c.log, m, ly, w - 2 * m, h - ly - m);
    size_columns();
}

/// The list and the message pane again, with `msg` on top.
fn refresh(msg: &str) {
    let c = ctl();
    let o = opts();
    let mut text = String::new();
    if !msg.is_empty() {
        text.push_str(msg.trim_end());
        text.push_str("\n\n");
    }
    let rows = match o.game_dir().and_then(|d| Ctx::new(&d, GAME)) {
        Err(e) => {
            set_text(c.game, "");
            text.push_str(&format!("{e}\nClick Change... to pick the folder that has {}.", GAME.exes[0]));
            Vec::new()
        }
        Ok(ctx) => {
            set_text(c.game, &ctx.dir.display().to_string());
            match status(&ctx) {
                Err(e) => {
                    text.push_str(&e.0);
                    Vec::new()
                }
                Ok(st) => {
                    if let Err(e) = o.check_closed() {
                        text.push_str(&format!("{e}\n"));
                    }
                    if st.rows.is_empty() {
                        text.push_str(
                            "No dinput8.dll mods yet. Click Install mod... or drop a mod's zip on this window.\n",
                        );
                    }
                    for n in &st.notes {
                        text.push_str(&format!("Note: {n}\n"));
                    }
                    if st.repair.is_empty() && !st.rows.is_empty() {
                        text.push_str(
                            "Everything is in place: nothing to repair. Select a mod to uninstall it.
",
                        );
                    }
                    if !st.repair.is_empty() {
                        text.push_str("\nSomething changed the chain since dllchain last ran. Repair would:\n");
                        for l in &st.repair {
                            text.push_str(&format!("  - {l}\n"));
                        }
                    }
                    st.rows
                }
            }
        }
    };
    // SAFETY: our list view; each text is NUL-terminated and copied by the control.
    unsafe {
        SendMessageW(c.list, LVM_DELETEALLITEMS, 0, 0);
        for (i, r) in rows.iter().enumerate() {
            let sha = r.sha.as_deref().map_or("missing".to_string(), |s| short(s).to_string());
            let cells = [r.file.to_string(), r.name.clone(), r.role.clone(), sha, r.source.clone()];
            for (k, cell) in cells.iter().enumerate() {
                let mut text = wide(cell);
                let item = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: i as i32,
                    iSubItem: k as i32,
                    pszText: text.as_mut_ptr(),
                    ..std::mem::zeroed()
                };
                let msg = if k == 0 { LVM_INSERTITEMW } else { LVM_SETITEMTEXTW };
                SendMessageW(c.list, msg, i, (&raw const item) as isize);
            }
        }
    }
    size_columns();
    let has_installed = rows.iter().any(|r| r.installed);
    let has_game = o.game_dir().is_ok();
    // SAFETY: our buttons.
    unsafe {
        EnableWindow(c.buttons[0], has_game as i32);
        EnableWindow(c.buttons[1], has_installed as i32);
        EnableWindow(c.buttons[2], has_installed as i32);
        EnableWindow(c.buttons[3], has_game as i32);
        EnableWindow(c.buttons[4], has_game as i32);
    }
    DATA.with(|d| {
        if let Some(d) = d.borrow_mut().as_mut() {
            d.rows = rows;
        }
    });
    set_text(c.log, text.trim_end());
}

enum Action {
    Install(PathBuf),
    Uninstall(Option<String>),
    Repair,
}

/// Plans the action, asks, and makes the changes. Never holds a borrow while a dialog is open (dialogs run messages).
fn act(a: Action) {
    let o = opts();
    let result = (|| -> Result<String> {
        let (ctx, recovered) = o.open()?;
        let mut head = String::new();
        for n in &recovered {
            head.push_str(&format!("{n}\n"));
        }
        let (plan, title, intro): (Plan, String, String) = match &a {
            Action::Install(p) => {
                let src = source::read(p, GAME)?;
                let intro = format!(
                    "{}\nSHA-256 {}\nCompare it with the SHA-256 the mod's author publishes.\n\n",
                    src.label, src.sha256
                );
                (plan_install(&ctx, &src)?, format!("Install {}", src.label), intro)
            }
            Action::Uninstall(w) => (plan_uninstall(&ctx, w.as_deref())?, "Uninstall".into(), String::new()),
            Action::Repair => (plan_repair(&ctx)?, "Repair".into(), String::new()),
        };
        let words = describe(&plan);
        if plan.is_empty() {
            return Ok(format!("{head}{intro}{words}"));
        }
        let icon = if plan.replaces.is_empty() { MB_ICONQUESTION } else { MB_ICONWARNING };
        if message(&format!("dllchain - {title}"), &format!("{intro}{words}\nGo ahead?"), MB_OKCANCEL | icon) != IDOK {
            return Ok(format!("{head}Nothing was changed."));
        }
        let mut out = head;
        for l in o.apply(&ctx, &plan)? {
            out.push_str(&format!("{l}\n"));
        }
        out.push_str(&format!("{intro}{words}\nDone. Every change is in dllchain\\installer.log in the game folder."));
        Ok(out)
    })();
    match result {
        Ok(msg) => refresh(&msg),
        Err(Error(e)) => {
            refresh(&e);
            message("dllchain", &e, MB_OK | MB_ICONERROR);
        }
    }
}

fn pick_mod() -> Option<PathBuf> {
    let filter: Vec<u16> = "Mod zip or DLL (*.zip, *.dll)\0*.zip;*.dll\0".encode_utf16().chain([0]).collect();
    let title = wide("Pick a mod's zip or its dinput8.dll");
    let mut buf = vec![0u16; 4096];
    // SAFETY: OPENFILENAMEW is plain data; every pointer in it outlives the call.
    unsafe {
        let mut ofn: OPENFILENAMEW = std::mem::zeroed();
        ofn.lStructSize = size_of::<OPENFILENAMEW>() as u32;
        ofn.hwndOwner = ctl().main;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = buf.as_mut_ptr();
        ofn.nMaxFile = buf.len() as u32;
        ofn.lpstrTitle = title.as_ptr();
        ofn.Flags = OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
        if GetOpenFileNameW(&mut ofn) == 0 {
            return None;
        }
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

fn pick_folder() -> Option<PathBuf> {
    let title = wide(&format!("Pick {}'s folder (the one with {})", GAME.name, GAME.exes[0]));
    let mut buf = vec![0u16; 1024];
    // SAFETY: BROWSEINFOW is plain data; the returned item list is freed with CoTaskMemFree.
    unsafe {
        let mut bi: BROWSEINFOW = std::mem::zeroed();
        bi.hwndOwner = ctl().main;
        bi.lpszTitle = title.as_ptr();
        bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
        let pidl = SHBrowseForFolderW(&bi);
        if pidl.is_null() {
            return None;
        }
        let ok = SHGetPathFromIDListW(pidl, buf.as_mut_ptr());
        CoTaskMemFree(pidl as *const c_void);
        if ok == 0 {
            return None;
        }
    }
    let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..n])))
}

fn on_command(id: i32, code: u32) {
    match (id, code) {
        (ID_CHANGE, BN_CLICKED) => {
            let Some(dir) = pick_folder() else { return };
            if !steam::is_game_folder(&dir, GAME) {
                message(
                    "dllchain",
                    &format!("{} has no {}. Pick {}'s folder.", dir.display(), GAME.exes[0], GAME.name),
                    MB_OK | MB_ICONWARNING,
                );
                return;
            }
            DATA.with(|d| {
                if let Some(d) = d.borrow_mut().as_mut() {
                    d.opts.game = Some(dir);
                }
            });
            refresh("");
        }
        (ID_INSTALL, BN_CLICKED) => {
            if let Some(p) = pick_mod() {
                act(Action::Install(p));
            }
        }
        (ID_UNINSTALL, BN_CLICKED) => uninstall_selected(),
        (ID_UNINSTALL_ALL, BN_CLICKED) => act(Action::Uninstall(None)),
        (ID_REPAIR, BN_CLICKED) => act(Action::Repair),
        (ID_OPEN, BN_CLICKED) => {
            if let Ok(dir) = opts().game_dir() {
                let (verb, path) = (wide("open"), wide(&dir.display().to_string()));
                // SAFETY: opens the folder in Explorer; the strings outlive the call.
                unsafe { ShellExecuteW(ctl().main, verb.as_ptr(), path.as_ptr(), null(), null(), SW_SHOWNORMAL) };
            }
        }
        _ => {}
    }
}

fn uninstall_selected() {
    let c = ctl();
    // SAFETY: our list view.
    let i = unsafe { SendMessageW(c.list, LVM_GETNEXTITEM, usize::MAX, LVNI_SELECTED as isize) };
    let row =
        DATA.with(|d| d.borrow().as_ref().and_then(|d| usize::try_from(i).ok().and_then(|i| d.rows.get(i).cloned())));
    match row {
        None => {
            message("dllchain", "Pick a mod in the list first.", MB_OK | MB_ICONINFORMATION);
        }
        Some(r) if !r.installed => {
            message(
                "dllchain",
                &format!(
                    "dllchain did not install {} ({}), so it does not uninstall it. Remove it the way it was installed.",
                    r.file, r.name
                ),
                MB_OK | MB_ICONINFORMATION,
            );
        }
        Some(r) => act(Action::Uninstall(r.word)),
    }
}

fn dropped(h: HDROP) -> Option<PathBuf> {
    let mut buf = vec![0u16; 4096];
    // SAFETY: `h` is the drop handle from WM_DROPFILES, released here.
    let n = unsafe {
        let n = DragQueryFileW(h, 0, buf.as_mut_ptr(), buf.len() as u32);
        DragFinish(h);
        n
    };
    (n > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])))
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            create_controls(hwnd);
            0
        }
        WM_SIZE => {
            layout();
            0
        }
        WM_GETMINMAXINFO => {
            // SAFETY: Windows passes a MINMAXINFO with this message.
            unsafe {
                let d = GetDpiForWindow(hwnd).max(96) as i32;
                let mmi = lp as *mut MINMAXINFO;
                (*mmi).ptMinTrackSize.x = 660 * d / 96;
                (*mmi).ptMinTrackSize.y = 440 * d / 96;
            }
            0
        }
        WM_DPICHANGED => {
            let mut c = ctl();
            let old = c.font;
            c.font = make_font(((wp >> 16) & 0xffff) as u32);
            CTL.with(|x| x.set(Some(c)));
            set_fonts(&c);
            // SAFETY: the old font is no longer used by any control; the suggested rectangle comes with the message.
            unsafe {
                DeleteObject(old as _);
                let r = &*(lp as *const RECT);
                SetWindowPos(
                    hwnd,
                    null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            layout();
            0
        }
        WM_CTLCOLORSTATIC if lp as HWND == ctl().log => {
            // the message pane reads like a document, so it gets the window colour although it is read-only
            // SAFETY: the device context comes with the message.
            unsafe {
                SetBkColor(wp as HDC, GetSysColor(COLOR_WINDOW));
                SetTextColor(wp as HDC, GetSysColor(COLOR_WINDOWTEXT));
                GetSysColorBrush(COLOR_WINDOW) as LRESULT
            }
        }
        WM_COMMAND => {
            on_command((wp & 0xffff) as i32, ((wp >> 16) & 0xffff) as u32);
            0
        }
        WM_NOTIFY => {
            // SAFETY: WM_NOTIFY comes with an NMHDR.
            let h = unsafe { &*(lp as *const NMHDR) };
            if h.hwndFrom == ctl().list && h.code == NM_DBLCLK {
                uninstall_selected();
            }
            0
        }
        WM_DROPFILES => {
            if let Some(p) = dropped(wp as HDROP) {
                act(Action::Install(p));
            }
            0
        }
        WM_START => {
            let pending = DATA.with(|d| d.borrow_mut().as_mut().and_then(|d| d.pending.take()));
            // undo a run that was cut off, when the game and Steam are closed
            let recovered = opts().open().map(|(_, n)| n.join("\n")).unwrap_or_default();
            refresh(&recovered);
            if let Some(p) = pending {
                act(Action::Install(p));
            }
            0
        }
        WM_DESTROY => {
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: default handling.
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn parse_args() -> (Opts, Option<PathBuf>) {
    let mut o = Opts::default();
    let mut pending = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game" => o.game = it.next().map(PathBuf::from),
            "--steam-root" => o.steam_root = it.next().map(PathBuf::from),
            "--test-saved-games" => o.saved_games = it.next().map(PathBuf::from),
            "--test-save-store" => o.save_store = it.next().map(PathBuf::from),
            "--test-skip-running-check" => o.skip_running_check = true,
            _ => pending = Some(PathBuf::from(a)),
        }
    }
    (o, pending)
}

fn main() {
    let (opts, pending) = parse_args();
    DATA.with(|d| *d.borrow_mut() = Some(Data { opts, rows: Vec::new(), pending }));
    // SAFETY: plain Win32 start-up on this thread; every pointer passed outlives its call.
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let icc = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_STANDARD_CLASSES | ICC_LISTVIEW_CLASSES,
        };
        InitCommonControlsEx(&icc);
        CoInitializeEx(null(), COINIT_APARTMENTTHREADED as u32);
        let inst = GetModuleHandleW(null());
        let class = wide("dllchain-main");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: inst,
            hIcon: LoadIconW(null_mut(), IDI_APPLICATION),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
            hIconSm: null_mut(),
        };
        RegisterClassExW(&wc);
        let title = wide(&format!("dllchain - {} dinput8.dll mods", GAME.name));
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            null_mut(),
            null_mut(),
            inst,
            null(),
        );
        if hwnd.is_null() {
            return;
        }
        let d = GetDpiForWindow(hwnd).max(96) as i32;
        SetWindowPos(hwnd, null_mut(), 0, 0, 820 * d / 96, 600 * d / 96, SWP_NOMOVE | SWP_NOZORDER);
        DragAcceptFiles(hwnd, 1);
        ShowWindow(hwnd, SW_SHOWNORMAL);
        UpdateWindow(hwnd);
        PostMessageW(hwnd, WM_START, 0, 0);
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if IsDialogMessageW(hwnd, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}
