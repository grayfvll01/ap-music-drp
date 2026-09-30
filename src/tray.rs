//! The tray icon and its menu (every everyday setting lives here, so nobody
//! has to open the settings file), notifications, and `--dump`.

use crate::app::{self, SHARED};
use crate::prelude::*;
use crate::sys::Lock;
use crate::{autostart, config, discord, itunes, presence, smtc, sys, update};
use core::sync::atomic::{AtomicIsize, AtomicU32, Ordering::SeqCst};
use windows::Win32::Foundation::*;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleW, GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCSTR, PCWSTR, w};

const CLASS: PCWSTR = w!("AppleMusicDiscordPresence");
const WM_TRAY: u32 = WM_APP + 1;
/// Sent by a second copy of the app: point the user at this one.
const WM_HELLO: u32 = WM_APP + 3;
/// Show the text in BALLOON as a notification.
const WM_BALLOON: u32 = WM_APP + 4;

const ID_TOGGLE: usize = 10;
const ID_AUTOSTART: usize = 11;
const ID_ADVANCED: usize = 12;
const ID_QUIT: usize = 13;
const ID_UPDATE: usize = 14;
const ID_CHECK: usize = 15;
const ID_RESET: usize = 16;
const ID_ABOUT: usize = 17;
/// On/off switches, one per `config.ini` key (see `switches`).
const ID_SWITCH: usize = 20;
/// Status text choices, in `status_display` order (name, state, details, song — artist).
const ID_STATUS: usize = 40;

const SWITCHES: usize = 9;

/// The on/off settings in the menu: (config key, current value).
fn switches(c: &config::Config) -> [(&'static str, bool); SWITCHES] {
    [
        ("artwork", c.artwork),
        ("show_progress", c.show_progress),
        ("show_paused", c.show_paused),
        ("links", c.links),
        ("button_listen", c.button_listen),
        ("button_songlink", c.button_songlink),
        ("update_check", c.update_check),
        ("apple_music", c.apple_music),
        ("spotify", c.spotify),
    ]
}

const SWITCH_TEXT: [PCWSTR; SWITCHES] = [
    w!("Album art"),
    w!("Time bar"),
    w!("Keep showing when paused"),
    w!("Clickable song, artist and album"),
    w!("\"Listen on Apple Music\" (or Spotify)"),
    w!("\"song.link\" (any streaming service)"),
    w!("Check for updates automatically"),
    w!("Apple Music"),
    w!("Spotify"),
];
const STATUS_TEXT: [PCWSTR; 4] = [
    w!("Listening to Apple Music / Spotify"),
    w!("Listening to <artist>"),
    w!("Listening to <song>"),
    w!("Listening to <song> \u{2014} <artist>"),
];
const STATUS_VALUE: [&str; 4] = ["name", "state", "details", "song_artist"];

static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
static ICON: AtomicIsize = AtomicIsize::new(0);
static BALLOON: Lock<String> = Lock::new(String::new());

/// Runs the app; returns the process exit code.
pub fn real_main() -> u32 {
    if sys::has_arg("--dump") {
        dump(sys::has_arg("--send"));
        return 0;
    }
    unsafe {
        // One instance only; launching again points at the running one. The
        // mutex handle is intentionally kept open for the process lifetime.
        let _mutex =
            windows::Win32::System::Threading::CreateMutexW(None, true, w!("Local\\AppleMusicDiscordPresence"));
        // ACCESS_DENIED: another copy is running elevated.
        if matches!(GetLastError(), ERROR_ALREADY_EXISTS | ERROR_ACCESS_DENIED) {
            if let Ok(h) = FindWindowW(CLASS, PCWSTR::null()) {
                let _ = PostMessageW(Some(h), WM_HELLO, WPARAM(0), LPARAM(0));
            }
            return 0;
        }
        run_tray();
    }
    0
}

#[cfg_attr(test, allow(dead_code))]
unsafe fn run_tray() {
    unsafe {
        allow_dark_menus();
        let hinst: HINSTANCE = GetModuleHandleW(None).unwrap_or_default().into();
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: hinst, lpszClassName: CLASS, ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) =
            CreateWindowExW(WINDOW_EX_STYLE(0), CLASS, CLASS, WS_OVERLAPPED, 0, 0, 0, 0, None, None, Some(hinst), None)
        else {
            return;
        };
        SHARED.hwnd.store(hwnd.0 as isize, SeqCst);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), SeqCst);

        let cx = GetSystemMetrics(SM_CXSMICON);
        let icon = LoadImageW(Some(hinst), PCWSTR(1 as _), IMAGE_ICON, cx, cx, LR_DEFAULTCOLOR)
            .map(|h| h.0 as isize)
            .or_else(|_| LoadIconW(None, IDI_APPLICATION).map(|h| h.0 as isize))
            .unwrap_or(0);
        ICON.store(icon, SeqCst);

        SHARED.init();
        update::init();
        autostart::migrate();
        let first_run = config::ensure_file();
        config::migrate();
        let _ = tray(NIM_ADD);
        if first_run {
            balloon("Your Apple Music and Spotify songs now show on Discord.\nClick the music note here for options.");
        }
        if !sys::spawn(app::run) {
            return;
        }
        sys::spawn(update::run);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Windows menus ignore dark mode unless the app opts in via uxtheme
/// ordinal 135 (SetPreferredAppMode on 1903+, AllowDarkModeForApp(BOOL) on
/// 1809 — passing 1 means "allow dark" for both) and 136 (FlushMenuThemes).
/// Undocumented but used by many tray apps; skipped if missing.
unsafe fn allow_dark_menus() {
    unsafe {
        let Ok(ux) = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32) else { return };
        if let Some(f) = GetProcAddress(ux, PCSTR(135 as _)) {
            let set_preferred_app_mode: extern "system" fn(i32) -> i32 = core::mem::transmute(f);
            set_preferred_app_mode(1);
        }
        if let Some(f) = GetProcAddress(ux, PCSTR(136 as _)) {
            let flush_menu_themes: extern "system" fn() = core::mem::transmute(f);
            flush_menu_themes();
        }
    }
}

/// Menu text treats '&' as a mnemonic marker.
fn escape_amp(s: &str) -> String {
    s.replace('&', "&&")
}

/// Copies `s` into a fixed UTF-16 buffer, NUL-terminated, never splitting a
/// surrogate pair.
fn copy_wide(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for c in s.chars() {
        let mut units = [0u16; 2];
        let enc = c.encode_utf16(&mut units);
        if n + enc.len() >= dst.len() {
            break;
        }
        dst[n..n + enc.len()].copy_from_slice(enc);
        n += enc.len();
    }
    dst[n] = 0;
}

/// Adds, updates or removes the tray icon; false if Explorer refused.
unsafe fn tray(op: NOTIFY_ICON_MESSAGE) -> bool {
    let mut nid = NOTIFYICONDATAW {
        cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(SHARED.hwnd.load(SeqCst) as _),
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: HICON(ICON.load(SeqCst) as _),
        ..Default::default()
    };
    let playing = SHARED.status.with(|s| s.playing.clone());
    let tip = if playing.is_empty() { crate::APP_NAME.to_string() } else { format!("{}\n{playing}", crate::APP_NAME) };
    copy_wide(&mut nid.szTip, &escape_amp(&tip));
    unsafe { Shell_NotifyIconW(op, &nid) }.as_bool()
}

/// A notification bubble from the tray icon.
unsafe fn balloon(text: &str) {
    let mut nid = NOTIFYICONDATAW {
        cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: HWND(SHARED.hwnd.load(SeqCst) as _),
        uID: 1,
        uFlags: NIF_INFO,
        dwInfoFlags: NIIF_INFO,
        ..Default::default()
    };
    copy_wide(&mut nid.szInfoTitle, crate::APP_NAME);
    copy_wide(&mut nid.szInfo, text);
    let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
}

fn post(msg: u32) {
    let h = HWND(SHARED.hwnd.load(SeqCst) as _);
    unsafe {
        let _ = PostMessageW(Some(h), msg, WPARAM(0), LPARAM(0));
    }
}

/// Shows a notification (from any thread).
pub fn notify(text: &str) {
    BALLOON.with(|b| *b = text.into());
    post(WM_BALLOON);
}

/// Quits the app as if "Quit" was clicked (from any thread).
pub fn quit_soon() {
    post(WM_CLOSE);
}

/// Opens a web page or file with its default program.
pub fn open(target: &str) {
    let _ = run_program(target, "");
}

/// Starts a program (or opens a document); false if Windows couldn't.
pub fn run_program(path: &str, args: &str) -> bool {
    let (path, args) = (HSTRING::from(path), HSTRING::from(args));
    let r = unsafe { ShellExecuteW(None, w!("open"), &path, &args, PCWSTR::null(), SW_SHOWNORMAL) };
    r.0 as isize > 32
}

unsafe fn show_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else { return };
        let st = SHARED.status.with(|s| s.clone());
        let cfg = config::load().unwrap_or_default();
        let sw = switches(&cfg);
        let check = |on: bool| if on { MF_CHECKED } else { MF_UNCHECKED };
        let item = |m: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: PCWSTR| {
            let _ = AppendMenuW(m, MF_STRING | flags, id, text);
        };
        let line = |m: HMENU| {
            let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
        };
        let submenu = |m: HMENU, text: PCWSTR, fill: &dyn Fn(HMENU)| {
            if let Ok(sub) = CreatePopupMenu() {
                fill(sub);
                let _ = AppendMenuW(m, MF_POPUP, sub.0 as usize, text);
            }
        };

        for text in [&st.playing, &st.discord] {
            if !text.is_empty() {
                item(menu, MF_GRAYED, 0, PCWSTR(HSTRING::from(escape_amp(text)).as_ptr()));
            }
        }
        line(menu);
        if let Some(v) = update::AVAILABLE.with(|a| a.as_ref().map(|u| u.version.clone())) {
            item(
                menu,
                MENU_ITEM_FLAGS(0),
                ID_UPDATE,
                PCWSTR(HSTRING::from(format!("Install update (version {v})")).as_ptr()),
            );
        }
        item(menu, check(SHARED.enabled.load(SeqCst)), ID_TOGGLE, w!("Show on Discord"));
        line(menu);
        submenu(menu, w!("Music apps"), &|m| {
            for i in 7..9 {
                item(m, check(sw[i].1), ID_SWITCH + i, SWITCH_TEXT[i]);
            }
        });
        submenu(menu, w!("Status text"), &|m| {
            for (i, text) in STATUS_TEXT.into_iter().enumerate() {
                item(m, check(cfg.status_display as usize == i), ID_STATUS + i, text);
            }
        });
        for i in 0..4 {
            item(menu, check(sw[i].1), ID_SWITCH + i, SWITCH_TEXT[i]);
        }
        submenu(menu, w!("Buttons for friends"), &|m| {
            for i in 4..6 {
                item(m, check(sw[i].1), ID_SWITCH + i, SWITCH_TEXT[i]);
            }
        });
        line(menu);
        item(menu, check(autostart::enabled()), ID_AUTOSTART, w!("Start with Windows"));
        let about = HSTRING::from(format!("About (version {})", env!("CARGO_PKG_VERSION")));
        submenu(menu, w!("More"), &|m| {
            item(m, check(sw[6].1), ID_SWITCH + 6, SWITCH_TEXT[6]);
            item(m, MENU_ITEM_FLAGS(0), ID_CHECK, w!("Check for updates now"));
            line(m);
            item(m, MENU_ITEM_FLAGS(0), ID_RESET, w!("Reset all settings\u{2026}"));
            item(m, MENU_ITEM_FLAGS(0), ID_ADVANCED, w!("Advanced settings file\u{2026}"));
            line(m);
            item(m, MENU_ITEM_FLAGS(0), ID_ABOUT, PCWSTR(about.as_ptr()));
        });
        item(menu, MENU_ITEM_FLAGS(0), ID_QUIT, w!("Quit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_NONOTIFY, pt.x, pt.y, None, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu); // also destroys the submenus
        if cmd.0 != 0 {
            command(hwnd, cmd.0 as usize);
        }
    }
}

fn open_settings() {
    config::ensure_file();
    let path = config::path();
    if !run_program(&path, "") {
        run_program("notepad.exe", &path);
    }
}

/// Applies a menu command. Settings are written to the file; the worker
/// notices the change and applies it straight away.
fn command(hwnd: HWND, id: usize) {
    match id {
        ID_TOGGLE => SHARED.enabled.fetch_xor(true, SeqCst),
        ID_AUTOSTART => {
            autostart::set(!autostart::enabled());
            return;
        }
        ID_ADVANCED => return open_settings(),
        ID_QUIT => return unsafe { quit(hwnd) },
        ID_UPDATE => {
            sys::spawn(update::install);
            return;
        }
        ID_CHECK => return update::check_now(),
        ID_ABOUT => return open(update::PAGE),
        ID_RESET => {
            let ask = HSTRING::from("Reset all settings to how they were when you installed the app?");
            let title = HSTRING::from(crate::APP_NAME);
            if unsafe { MessageBoxW(Some(hwnd), &ask, &title, MB_YESNO | MB_ICONQUESTION) } != IDYES {
                return;
            }
            config::reset();
            false
        }
        _ if (ID_STATUS..ID_STATUS + STATUS_VALUE.len()).contains(&id) => {
            config::set("status_display", STATUS_VALUE[id - ID_STATUS]);
            false
        }
        _ if (ID_SWITCH..ID_SWITCH + SWITCHES).contains(&id) => {
            let (key, on) = switches(&config::load().unwrap_or_default())[id - ID_SWITCH];
            config::set(key, if on { "false" } else { "true" });
            false
        }
        _ => return,
    };
    SHARED.wake();
}

unsafe fn quit(hwnd: HWND) {
    if SHARED.quit.swap(true, SeqCst) {
        return; // already quitting
    }
    SHARED.wake();
    // Give the worker a moment to clear the presence (exiting closes the
    // pipe, which clears it anyway).
    for _ in 0..60 {
        if SHARED.done.load(SeqCst) {
            break;
        }
        sys::sleep(50);
    }
    unsafe {
        let _ = tray(NIM_DELETE);
        let _ = DestroyWindow(hwnd);
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAY => {
                if matches!(lp.0 as u32, WM_LBUTTONUP | WM_RBUTTONUP) {
                    show_menu(hwnd);
                }
                LRESULT(0)
            }
            WM_HELLO => {
                balloon("Already running. Click the music note here for options.");
                LRESULT(0)
            }
            WM_BALLOON => {
                balloon(&BALLOON.with(|b| b.clone()));
                LRESULT(0)
            }
            app::WM_STATUS => {
                // The icon can fail to appear at logon while Explorer is busy;
                // every status change retries.
                if !tray(NIM_MODIFY) {
                    tray(NIM_ADD);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                command(hwnd, wp.0 & 0xFFFF);
                LRESULT(0)
            }
            // Quit from the menu, the installer, `taskkill`, logoff or
            // shutdown: always clear the presence and remove the icon first.
            WM_CLOSE => {
                quit(hwnd);
                LRESULT(0)
            }
            WM_ENDSESSION if wp.0 != 0 => {
                quit(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            m if m != 0 && m == TASKBAR_CREATED.load(SeqCst) => {
                let _ = tray(NIM_ADD); // Explorer restarted
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

/// `AppleMusicDiscordPresence --dump [--send]`: prints what would be sent (debug aid).
#[cfg_attr(test, allow(dead_code))]
fn dump(send: bool) {
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
    let out = |s: String| sys::print(&(s + "\n"));
    let cfg = config::load().unwrap_or_default();
    out(format!("{} {}\nconfig: {}", crate::APP_NAME, env!("CARGO_PKG_VERSION"), config::path()));
    let mut s = smtc::Smtc::default();
    out("media sessions:".into());
    let players = app::players(&cfg);
    for id in s.session_ids() {
        let tag = match smtc::Player::of(&id) {
            Some(p) if players.contains(&p) => p.name(),
            Some(_) => "turned off",
            None => "ignored",
        };
        out(format!("  {id}  [{tag}]"));
    }
    let now = sys::now_ms();
    let track = match s.poll(&players) {
        Ok(Some(t)) => t,
        Ok(None) => return out("nothing playing".into()),
        Err(e) => return out(format!("error: {:#x}", e.code().0)),
    };
    let state = match track.state {
        smtc::State::Playing => "playing",
        smtc::State::Paused => "paused",
        smtc::State::Changing => "changing",
    };
    out(format!(
        "{}: title=\"{}\" artist=\"{}\" album=\"{}\" {state} {}s/{}s",
        track.player.name(),
        track.title,
        track.artist,
        track.album,
        track.position_ms / 1000,
        track.duration_ms / 1000
    ));
    let meta = app::wants_lookup(&cfg, track.player)
        .then(|| itunes::Lookup::new().find(&track, &app::country(&cfg), cfg.artwork_size))
        .flatten();
    match &meta {
        Some(m) => out(format!(
            "lookup: art={}\n        track={}\n        artist={}\n        album={}",
            m.artwork, m.track_url, m.artist_url, m.album_url
        )),
        None => out("lookup: no match".into()),
    }
    let Some(act) = presence::build(&track, meta.as_ref(), &cfg, now) else {
        return out("activity: (none)".into());
    };
    out(format!("activity: {}", act.json()));
    if send {
        match discord::Discord::connect(&cfg.client_id) {
            Err(e) => out(format!("discord: {e}")),
            Ok(mut d) => match d.set_activity(Some(&act.json())) {
                Ok(reply) => {
                    out(format!("discord: accepted: {reply}\nholding 20 s"));
                    sys::sleep(20_000);
                    let _ = d.set_activity(None);
                }
                Err(discord::Error::Rejected(e) | discord::Error::Disconnected(e)) => out(format!("discord: {e}")),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DEFAULT_FILE, parse_into, with_value};

    /// Every menu switch must flip exactly its own setting in the file.
    #[test]
    fn menu_switches_flip_their_setting() {
        let base = parse_into(Config::default(), DEFAULT_FILE);
        for (i, (key, on)) in switches(&base).into_iter().enumerate() {
            let text = with_value(DEFAULT_FILE, key, if on { "false" } else { "true" });
            let after = switches(&parse_into(Config::default(), &text));
            for (j, (_, v)) in after.into_iter().enumerate() {
                assert_eq!(v, if i == j { !on } else { switches(&base)[j].1 }, "switch {key} affected #{j}");
            }
        }
        assert_eq!(SWITCH_TEXT.len(), switches(&base).len());
    }

    /// Each status choice maps to its `status_display` value (the menu's
    /// check mark is `status_display == index`).
    #[test]
    fn status_choices() {
        for (i, v) in STATUS_VALUE.into_iter().enumerate() {
            let c = parse_into(Config::default(), &with_value(DEFAULT_FILE, "status_display", v));
            assert_eq!(c.status_display as usize, i, "{v}");
        }
        assert_eq!(STATUS_TEXT.len(), STATUS_VALUE.len());
    }
}
