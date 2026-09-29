//! `%APPDATA%\AppleMusicDiscordPresence\config.ini`: flat `key = value` lines, `#`/`;` comments.
//! Missing keys fall back to defaults; the file is re-read whenever it changes.

use crate::prelude::*;
use crate::sys;

pub const DEFAULT_FILE: &str = include_str!("../config.default.ini");

/// `status_display` choice that shows "<song> — <artist>" (not a Discord
/// value: presence.rs turns it into details = song — artist, state = album).
pub const SONG_ARTIST: u8 = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub client_id: String,
    pub name: String,
    pub activity_type: u8,
    pub status_display: u8,
    pub details: String,
    pub state: String,
    pub large_text: String,
    pub show_progress: bool,
    pub show_paused: bool,
    pub artwork: bool,
    pub artwork_size: u32,
    pub links: bool,
    pub country: String,
    pub fallback_image: String,
    pub button_listen: bool,
    pub button_songlink: bool,
    pub poll_ms: u32,
    pub log: bool,
    pub update_check: bool,
}

impl Default for Config {
    fn default() -> Self {
        // The shipped file is the single source of truth for defaults.
        parse_into(Config::base(), DEFAULT_FILE)
    }
}

impl Config {
    /// Values used only if the embedded default file itself lacks a key.
    fn base() -> Self {
        Config {
            client_id: String::new(),
            name: String::new(),
            activity_type: 2,
            status_display: 0,
            details: "{title}".into(),
            state: "{artist}".into(),
            large_text: "{album}".into(),
            show_progress: true,
            show_paused: false,
            artwork: true,
            artwork_size: 512,
            links: true,
            country: "auto".into(),
            fallback_image: String::new(),
            button_listen: false,
            button_songlink: false,
            poll_ms: 1000,
            log: false,
            update_check: true,
        }
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// `"x"` or `'x'` -> `x`.
fn unquote(v: &str) -> &str {
    let b = v.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] { &v[1..v.len() - 1] } else { v }
}

/// `false   ; no links` -> `false`.
fn strip_comment(v: &str) -> &str {
    let cut = [" #", " ;", "\t#", "\t;"].iter().filter_map(|p| v.find(p)).min().unwrap_or(v.len());
    v[..cut].trim_end()
}

pub fn parse_into(mut c: Config, text: &str) -> Config {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') || line.starts_with('[') {
            continue;
        }
        let Some((k, raw)) = line.split_once('=') else { continue };
        let k = k.trim().to_ascii_lowercase();
        // Text values (templates, name) keep '#'/';', which can be part of a
        // title; everything else may carry a trailing comment.
        let text = unquote(raw.trim());
        let v = unquote(strip_comment(raw.trim())).trim();
        let set_bool = |dst: &mut bool| {
            if let Some(b) = parse_bool(v) {
                *dst = b;
            }
        };
        match k.as_str() {
            "client_id" if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) => c.client_id = v.into(),
            "name" => c.name = text.into(),
            "activity_type" => {
                c.activity_type = match v.to_ascii_lowercase().as_str() {
                    "playing" => 0,
                    "listening" => 2,
                    "watching" => 3,
                    _ => c.activity_type,
                }
            }
            "status_display" => {
                c.status_display = match v.to_ascii_lowercase().as_str() {
                    "name" | "app" => 0,
                    "state" | "artist" => 1,
                    "details" | "title" => 2,
                    "song_artist" | "both" => SONG_ARTIST,
                    _ => c.status_display,
                }
            }
            "details" => c.details = text.into(),
            "state" => c.state = text.into(),
            "large_text" => c.large_text = text.into(),
            "show_progress" => set_bool(&mut c.show_progress),
            "show_paused" => set_bool(&mut c.show_paused),
            "artwork" => set_bool(&mut c.artwork),
            "artwork_size" => {
                if let Ok(n) = v.parse::<u32>() {
                    c.artwork_size = n.clamp(64, 3000);
                }
            }
            "links" => set_bool(&mut c.links),
            "country"
                if v.eq_ignore_ascii_case("auto") || (v.len() == 2 && v.bytes().all(|b| b.is_ascii_alphabetic())) =>
            {
                c.country = v.to_ascii_lowercase()
            }
            "fallback_image" if v.is_empty() || v.starts_with("https://") => c.fallback_image = v.into(),
            "button_listen" => set_bool(&mut c.button_listen),
            "button_songlink" => set_bool(&mut c.button_songlink),
            "poll_ms" => {
                if let Ok(n) = v.parse::<u32>() {
                    c.poll_ms = n.clamp(250, 10_000);
                }
            }
            "log" => set_bool(&mut c.log),
            "update_check" => set_bool(&mut c.update_check),
            _ => {}
        }
    }
    c
}

fn appdata() -> String {
    sys::env("APPDATA").unwrap_or_else(|| ".".into())
}

pub fn dir() -> String {
    appdata() + "\\AppleMusicDiscordPresence"
}

pub fn path() -> String {
    dir() + "\\config.ini"
}

/// Writes the commented default file if there is none yet; true if it
/// did (first run).
pub fn ensure_file() -> bool {
    let p = path();
    if sys::stat(&p).is_some() {
        return false;
    }
    sys::create_dir(&dir());
    // Settings from before the app was renamed (0.1.x) carry over.
    if sys::rename(&(appdata() + "\\ap-music-drp\\config.ini"), &p) {
        return false;
    }
    // CRLF so old Notepad shows it right, whatever the checkout's line endings.
    sys::create_file(&p, DEFAULT_FILE.replace("\r\n", "\n").replace('\n', "\r\n").as_bytes())
}

/// Sets one `key = value` in the file (the tray menu's switches), keeping
/// everything else, comments included, as it is.
pub fn set(key: &str, value: &str) -> bool {
    ensure_file();
    let Some(text) = read_text() else { return false };
    // Write a new file and swap it in, so a reader never sees half of it.
    let tmp = path() + ".new";
    sys::write_file(&tmp, with_value(&text, key, value).as_bytes()) && sys::replace(&tmp, &path())
}

/// Back to the shipped defaults.
pub fn reset() {
    sys::delete_file(&path());
    ensure_file();
}

pub(crate) fn with_value(text: &str, key: &str, value: &str) -> String {
    let mut out = String::with_capacity(text.len() + 32);
    let mut done = false;
    for line in text.lines() {
        let t = line.trim_start();
        let ours =
            !t.starts_with(['#', ';']) && t.split_once('=').is_some_and(|(k, _)| k.trim().eq_ignore_ascii_case(key));
        if !ours {
            out.push_str(line);
        } else if !done {
            out.push_str(&format!("{key} = {value}"));
            done = true;
        } else {
            continue; // a later duplicate would override the new value
        }
        out.push_str("\r\n");
    }
    if !done {
        out.push_str(&format!("{key} = {value}\r\n"));
    }
    out
}

fn read_text() -> Option<String> {
    let bytes = sys::read_file(&path())?;
    if bytes.is_empty() {
        return None;
    }
    // Notepad's "UTF-16 LE" encoding starts with FF FE.
    let text = match bytes.strip_prefix(&[0xFF, 0xFE]) {
        Some(b) => {
            String::from_utf16_lossy(&b.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect::<Vec<_>>())
        }
        None => String::from_utf8_lossy(&bytes).into_owned(),
    };
    Some(text.strip_prefix('\u{feff}').map(String::from).unwrap_or(text))
}

/// Change marker for the file: (last write time, size).
pub fn stamp() -> Option<(u64, u64)> {
    sys::stat(&path())
}

/// Loads the config (creating the default file on first run). `None` if the
/// file exists but can't be read right now, e.g. an editor is mid-save:
/// callers keep their current settings and try again.
pub fn load() -> Option<Config> {
    ensure_file();
    Some(parse_into(Config::default(), &read_text()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse() {
        let c = Config::default();
        assert!(!c.client_id.is_empty());
        assert_eq!(c.activity_type, 2);
    }

    #[test]
    fn overrides_and_garbage() {
        let c = parse_into(
            Config::default(),
            "client_id = 123\nactivity_type = playing\nstatus_display=details\nshow_paused = yes\npoll_ms = 5\ncountry = gb\nfallback_image = http://x\nnonsense\nclient_id = abc\n",
        );
        assert_eq!(c.client_id, "123");
        assert_eq!(c.activity_type, 0);
        assert_eq!(c.status_display, 2);
        assert!(c.show_paused);
        assert_eq!(c.poll_ms, 250);
        assert_eq!(c.country, "gb");
        assert_eq!(c.fallback_image, Config::default().fallback_image);
    }

    #[test]
    fn menu_switches_keep_the_file() {
        let before = "# comment\r\nartwork = true\r\n; links = x\r\nartwork = false\r\nstate = {artist}\r\n";
        let after = with_value(before, "artwork", "false");
        assert_eq!(after, "# comment\r\nartwork = false\r\n; links = x\r\nstate = {artist}\r\n");
        let added = with_value(&after, "show_paused", "true");
        assert!(added.ends_with("show_paused = true\r\n"));
        let c = parse_into(Config::default(), &added);
        assert!(!c.artwork && c.show_paused);
        assert_eq!(c.state, "{artist}");
    }

    #[test]
    fn quotes_and_comments() {
        let c = parse_into(
            Config::default(),
            "links = false   ; no links\nartwork = \"false\"\nname = \"My Music\"\ndetails = #1 {title} ; keep\nstate = '{artist}'\npoll_ms = 2000 # ms\n",
        );
        assert!(!c.links && !c.artwork);
        assert_eq!(c.name, "My Music");
        assert_eq!(c.details, "#1 {title} ; keep");
        assert_eq!(c.state, "{artist}");
        assert_eq!(c.poll_ms, 2000);
    }
}
