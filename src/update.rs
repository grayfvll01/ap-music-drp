//! Updates from the project's GitHub releases: a check shortly after start
//! and twice a day (can be switched off), and a one-click install that runs
//! the new release's installer.

use crate::app::SHARED;
use crate::http::Client;
use crate::json::{self, Json};
use crate::prelude::*;
use crate::sys::{self, Lock};
use crate::{config, tray};
use core::sync::atomic::{AtomicBool, AtomicIsize, Ordering::SeqCst};

pub const REPO: &str = "grayfvll01/apple-music-discord-presence";
pub const PAGE: &str = "https://github.com/grayfvll01/apple-music-discord-presence";
const SETUP: &str = "AppleMusicDiscordPresence-Setup.exe";
const FIRST_CHECK_MS: u32 = 20_000;
const CHECK_EVERY_MS: u32 = 12 * 3600 * 1000;

#[derive(Clone)]
pub struct Update {
    pub version: String,
    setup_url: String,
    sha_url: String,
}

/// The newest release, if it's newer than this copy.
pub static AVAILABLE: Lock<Option<Update>> = Lock::new(None);
static MANUAL: AtomicBool = AtomicBool::new(false);
static INSTALLING: AtomicBool = AtomicBool::new(false);
static WAKE: AtomicIsize = AtomicIsize::new(0);

/// "v1.2.3" / "1.2" -> [1, 2, 3] / [1, 2, 0].
fn version(s: &str) -> Option<[u32; 3]> {
    let mut parts = s.trim().trim_start_matches(['v', 'V']).split(['.', '-', '+']);
    let mut next = || parts.next().map(|p| p.parse::<u32>());
    Some([next()?.ok()?, next()?.ok()?, next().and_then(Result::ok).unwrap_or(0)])
}

pub fn is_newer(tag: &str, current: &str) -> bool {
    matches!((version(tag), version(current)), (Some(a), Some(b)) if a > b)
}

/// Picks the installer (and its checksum) out of a GitHub "latest release"
/// reply. Only files published in this project's own releases are accepted.
fn parse_release(v: &Json, current: &str) -> Option<Update> {
    let tag = v.str("tag_name")?;
    if !is_newer(tag, current) {
        return None;
    }
    let prefix = format!("https://github.com/{REPO}/releases/download/");
    let url = |name: &str| {
        v.arr("assets")
            .iter()
            .find(|a| a.str("name") == Some(name))
            .and_then(|a| a.str("browser_download_url"))
            .filter(|u| u.starts_with(&prefix))
            .map(String::from)
    };
    Some(Update {
        version: tag.trim_start_matches(['v', 'V']).into(),
        setup_url: url(SETUP)?,
        sha_url: url(&format!("{SETUP}.sha256"))?,
    })
}

/// Ok(None) = up to date.
fn latest() -> Result<Option<Update>, ()> {
    let http = Client::new().ok_or(())?;
    let body = http.get("api.github.com", &format!("/repos/{REPO}/releases/latest"), sys::ticks() + 15_000);
    let v = json::parse(&String::from_utf8_lossy(&body.map_err(|_| ())?)).ok_or(())?;
    Ok(parse_release(&v, env!("CARGO_PKG_VERSION")))
}

pub fn init() {
    WAKE.store(sys::event_new(), SeqCst);
}

/// "Check for updates now" from the menu: always reports the result.
pub fn check_now() {
    MANUAL.store(true, SeqCst);
    sys::event_set(WAKE.load(SeqCst));
}

/// The checker thread.
pub fn run() {
    let mut wait = FIRST_CHECK_MS;
    let mut announced = String::new();
    loop {
        sys::event_wait(WAKE.load(SeqCst), wait);
        if SHARED.quit.load(SeqCst) {
            return;
        }
        let manual = MANUAL.swap(false, SeqCst);
        if !manual && !config::load().unwrap_or_default().update_check {
            wait = CHECK_EVERY_MS;
            continue;
        }
        wait = CHECK_EVERY_MS;
        match latest() {
            Ok(Some(u)) => {
                let v = u.version.clone();
                AVAILABLE.with(|a| *a = Some(u));
                if manual || announced != v {
                    tray::notify(&format!("Version {v} is available.\nClick the music note, then \"Install update\"."));
                    announced = v;
                }
            }
            Ok(None) => {
                AVAILABLE.with(|a| *a = None);
                if manual {
                    tray::notify(&format!("You have the latest version ({}).", env!("CARGO_PKG_VERSION")));
                }
            }
            Err(()) if manual => tray::notify("Couldn't check for updates. Are you online?"),
            Err(()) => wait = 3600 * 1000, // try again in an hour
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use windows::Win32::Security::Cryptography::{BCRYPT_SHA256_ALG_HANDLE, BCryptHash};
    let mut out = [0u8; 32];
    if unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, None, data, &mut out) }.is_err() {
        return String::new();
    }
    let mut hex = String::with_capacity(64);
    for b in out {
        for n in [b >> 4, b & 15] {
            hex.push(char::from_digit(n as u32, 16).unwrap_or('0'));
        }
    }
    hex
}

/// Menu "Install update": download, verify, run the installer, and quit so
/// it can replace this exe (it starts the new version when done). Runs on
/// its own thread.
pub fn install() {
    if INSTALLING.swap(true, SeqCst) {
        return;
    }
    if let Err(why) = download_and_run() {
        tray::notify(&format!("{why}\nOpening the download page instead."));
        tray::open(&format!("{PAGE}/releases/latest"));
        INSTALLING.store(false, SeqCst);
    }
}

fn download_and_run() -> Result<(), &'static str> {
    let u = AVAILABLE.with(|a| a.clone()).ok_or("No update found.")?;
    tray::notify(&format!("Downloading version {}\u{2026}", u.version));
    let http = Client::new().ok_or("Couldn't download the update.")?;
    let deadline = sys::ticks() + 180_000;
    let sum = http.get_url(&u.sha_url, deadline, 4096).map_err(|_| "Couldn't download the update.")?;
    let want = String::from_utf8_lossy(&sum).split_whitespace().next().unwrap_or("").to_ascii_lowercase();
    let setup = http.get_url(&u.setup_url, deadline, 64 << 20).map_err(|_| "Couldn't download the update.")?;
    if want.len() != 64 || sha256_hex(&setup) != want {
        return Err("The download was damaged.");
    }
    let path = sys::env("TEMP").ok_or("Couldn't save the update.")? + "\\" + SETUP;
    if !sys::write_file(&path, &setup) {
        return Err("Couldn't save the update.");
    }
    if !tray::run_program(&path, "/SILENT /SUPPRESSMSGBOXES /NORESTART") {
        return Err("Couldn't start the installer.");
    }
    tray::quit_soon();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("v0.2.1", "0.2.0"));
        assert!(is_newer("v1.0", "0.9.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("v0.2.0", "0.2.0"));
        assert!(!is_newer("v0.1.9", "0.2.0"));
        assert!(!is_newer("nightly", "0.2.0"));
        assert!(!is_newer("", "0.2.0"));
    }

    #[test]
    fn release_assets() {
        let dl = format!("https://github.com/{REPO}/releases/download/v9.0.0");
        let v = json::parse(&format!(
            r#"{{"tag_name":"v9.0.0","assets":[
                {{"name":"AppleMusicDiscordPresence.exe","browser_download_url":"{dl}/AppleMusicDiscordPresence.exe"}},
                {{"name":"{SETUP}","browser_download_url":"{dl}/{SETUP}"}},
                {{"name":"{SETUP}.sha256","browser_download_url":"{dl}/{SETUP}.sha256"}}]}}"#
        ))
        .unwrap();
        let u = parse_release(&v, "0.2.0").unwrap();
        assert_eq!(u.version, "9.0.0");
        assert_eq!(u.setup_url, format!("{dl}/{SETUP}"));
        assert!(parse_release(&v, "9.0.0").is_none(), "not newer");
        // Files from anywhere else are never offered.
        let evil = json::parse(&format!(
            r#"{{"tag_name":"v9.0.0","assets":[
                {{"name":"{SETUP}","browser_download_url":"https://evil.example/{SETUP}"}},
                {{"name":"{SETUP}.sha256","browser_download_url":"{dl}/{SETUP}.sha256"}}]}}"#
        ))
        .unwrap();
        assert!(parse_release(&evil, "0.2.0").is_none());
    }

    #[test]
    fn checksum() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
