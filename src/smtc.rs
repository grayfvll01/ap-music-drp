//! Reads the Apple Music session from Windows' media controls (SMTC).
//!
//! Every other media session (browsers, Spotify, games, video players, ...) is
//! skipped by an exact package-identity check, so nothing else can ever reach
//! Discord. `GetCurrentSession()` is deliberately never used: it returns
//! whichever app Windows considers "current", which may not be Apple Music.

use crate::prelude::*;
use crate::sys::{self, EPOCH_DIFF};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

/// AUMID ("<package family>!<app>") of the Microsoft Store Apple Music app.
/// The package suffix is Apple's publisher hash. Matched in full: Apple TV
/// shares the publisher hash and the "!App" part, iTunes/Cider/browsers/the
/// web player all use other ids.
const APPLE_MUSIC_AUMID: &str = "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App";

pub fn is_apple_music(aumid: &str) -> bool {
    aumid.eq_ignore_ascii_case(APPLE_MUSIC_AUMID)
}

/// Waits for a WinRT async result, giving up after 3 s. Not `join()`: in
/// windows 0.62 that can block forever on an error, which would leave a
/// stale song on the status.
macro_rules! wait {
    ($op:expr) => {{
        let op = $op;
        let deadline = sys::ticks() + 3000;
        loop {
            let status = op.Status()?.0; // 0 started, 1 completed, 2 canceled, 3 error
            if status == 1 {
                break op.GetResults();
            }
            if status != 0 || sys::ticks() > deadline {
                let _ = op.Cancel();
                break Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
            }
            sys::sleep(1);
        }
    }};
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Playing,
    Paused,
    /// Between tracks / buffering: keep whatever is currently shown.
    Changing,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub state: State,
    /// Track length in ms, 0 when unknown (e.g. radio).
    pub duration_ms: i64,
    /// Playback position in ms, extrapolated to "now" while playing.
    pub position_ms: i64,
}

const DASH: char = '\u{2014}';
const STATION_SUFFIXES: [&str; 7] =
    ["\u{2019}s Station", "'s Station", "s Sender", "さんのステーション", "的电台", "的電台", "의 스테이션"];
const STATION_PREFIXES: [&str; 7] =
    ["Emisora de ", "Estación de ", "Estação de ", "Station de : ", "Station för ", "Station van ", "Stazione di "];

/// Personal radio stations are named after the listener ("John Doe’s
/// Station"), which would put the user's own name on their status. These are
/// the localised forms Apple Music uses.
fn is_station(s: &str) -> bool {
    let s = s.trim();
    STATION_SUFFIXES.iter().any(|x| s.len() > x.len() && s.ends_with(x))
        || STATION_PREFIXES.iter().any(|x| s.len() > x.len() && s.starts_with(x))
        || (s.starts_with("Моя станция (") && s.ends_with(')'))
}

fn mentions_station(s: &str) -> bool {
    s.split(DASH).any(is_station) || STATION_SUFFIXES.iter().any(|x| s.find(x).is_some_and(|i| i > 0))
}

/// Cleans Apple Music's raw SMTC fields into (title, artist, album), or
/// `None` for "don't show this".
///
/// Apple Music on Windows puts "Artist — Album" in the Artist field and
/// leaves AlbumTitle empty; while a personal station plays it becomes
/// "Artist — Album — John Doe’s Station". This fails closed: only the
/// first two parts are ever used, so station names in any language stay off
/// the status. A title without an artist is a placeholder ("Connecting…",
/// a station loading) and is never shown.
pub fn tidy(title: &str, artist: &str, album_title: &str) -> Option<(String, String, String)> {
    let title = title.trim();
    if title.is_empty() || mentions_station(title) {
        return None;
    }
    let mut parts = artist.split(DASH).map(str::trim);
    let artist = parts.next().unwrap_or("");
    if artist.is_empty() || is_station(artist) {
        return None;
    }
    let album = if album_title.trim().is_empty() { parts.next() } else { album_title.split(DASH).next() };
    let album = album.map(str::trim).filter(|a| !is_station(a)).unwrap_or("");
    Some((title.to_string(), artist.to_string(), album.to_string()))
}

#[derive(Default)]
pub struct Smtc {
    mgr: Option<Manager>,
}

impl Smtc {
    /// `Ok(None)` when Apple Music has no active session.
    pub fn poll(&mut self) -> windows::core::Result<Option<Track>> {
        let r = self.poll_inner();
        if r.is_err() {
            self.mgr = None; // re-acquire on the next poll
        }
        r
    }

    fn poll_inner(&mut self) -> windows::core::Result<Option<Track>> {
        if self.mgr.is_none() {
            self.mgr = Some(wait!(Manager::RequestAsync()?)?);
        }
        let sessions = self.mgr.as_ref().unwrap().GetSessions()?;
        let mut found = None;
        for i in 0..sessions.Size()? {
            let s = sessions.GetAt(i)?;
            if is_apple_music(&s.SourceAppUserModelId()?.to_string_lossy()) {
                if found.is_some() {
                    // Windows doesn't verify session ids; two "Apple Music"
                    // sessions means one is an impostor. Show nothing.
                    return Ok(None);
                }
                found = Some(s);
            }
        }
        match found {
            Some(s) => read(&s),
            None => Ok(None),
        }
    }

    /// Diagnostic listing of every session's app id (used by `--dump`).
    pub fn session_ids(&mut self) -> Vec<String> {
        let _ = self.poll();
        let Some(mgr) = &self.mgr else { return Vec::new() };
        let Ok(sessions) = mgr.GetSessions() else { return Vec::new() };
        (0..sessions.Size().unwrap_or(0))
            .filter_map(|i| sessions.GetAt(i).ok()?.SourceAppUserModelId().ok())
            .map(|h| h.to_string_lossy())
            .collect()
    }
}

fn read(s: &Session) -> windows::core::Result<Option<Track>> {
    let state = match s.GetPlaybackInfo()?.PlaybackStatus()? {
        Status::Playing => State::Playing,
        Status::Paused => State::Paused,
        Status::Changing | Status::Opened => State::Changing,
        _ => return Ok(None), // Stopped / Closed
    };
    let props = wait!(s.TryGetMediaPropertiesAsync()?)?;
    let fields = tidy(
        &props.Title()?.to_string_lossy(),
        &props.Artist()?.to_string_lossy(),
        &props.AlbumTitle()?.to_string_lossy(),
    );
    // Placeholders (loading, station names) are treated as a transition; the
    // worker clears the presence if they persist.
    let Some((title, artist, album)) = fields else { return Ok(Some(changing())) };

    let tl = s.GetTimelineProperties()?;
    let start = tl.StartTime()?.Duration;
    let duration_ms = ((tl.EndTime()?.Duration - start) / 10_000).max(0);
    let mut position_ms = (tl.Position()?.Duration - start) / 10_000;
    let updated = tl.LastUpdatedTime()?.UniversalTime;
    if state == State::Playing && updated > EPOCH_DIFF {
        let since = sys::now_ms() - (updated - EPOCH_DIFF) / 10_000;
        if (0..24 * 3600 * 1000).contains(&since) {
            position_ms += since;
        }
    }
    let position_ms = if duration_ms > 0 { position_ms.clamp(0, duration_ms) } else { position_ms.max(0) };
    Ok(Some(Track { title, artist, album, state, duration_ms, position_ms }))
}

fn changing() -> Track {
    Track {
        title: String::new(),
        artist: String::new(),
        album: String::new(),
        state: State::Changing,
        duration_ms: 0,
        position_ms: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity() {
        assert!(is_apple_music("AppleInc.AppleMusicWin_nzyj5cx40ttqa!App"));
        assert!(is_apple_music("appleinc.applemusicwin_NZYJ5CX40TTQA!APP"));
        for other in [
            "Chrome",
            "Spotify.exe",
            "msedge.exe",
            "308046B0AF4A39CB", // Firefox
            "AppleMusic.exe",
            "iTunes.exe",
            "AppleInc.iTunes_nzyj5cx40ttqa!iTunes",
            "music.apple.com-A5F611C_7vh1tm7h3g5s0!App", // web player PWA
            "electron.app.Apple Music Beta",
            "CiderCollective.Cider_a6qxe093bx5xj!App",
            "AppleInc.AppleMusicWin_nzyj5cx40ttqa!LibraryServer",
            " AppleInc.AppleMusicWin_nzyj5cx40ttqa!App",
            "AppleInc.AppleMusicWin_nzyj5cx40ttqa",
            "AppleInc.AppleMusicWin_nzyj5cx40ttqa!",
            "AppleInc.AppleMusicWin_nzyj5cx40ttqaX!App",
            "AppleInc.AppleMusicWin_evil!App",
            "AppleInc.AppleTVWin_nzyj5cx40ttqa!App",
            "Evil!AppleInc.AppleMusicWin_nzyj5cx40ttqa!App",
            "AppleInc.AppleMusicWin_nzyj5cx40ttqa!App!x",
            "",
        ] {
            assert!(!is_apple_music(other), "{other}");
        }
    }

    fn t(title: &str, artist: &str, album: &str) -> Option<(String, String, String)> {
        tidy(title, artist, album)
    }
    fn some(a: &str, b: &str, c: &str) -> Option<(String, String, String)> {
        Some((a.into(), b.into(), c.into()))
    }

    #[test]
    fn split() {
        assert_eq!(
            t("GIMME A HUG", "Drake \u{2014} $ome $exy $ongs 4 U", ""),
            some("GIMME A HUG", "Drake", "$ome $exy $ongs 4 U")
        );
        assert_eq!(t("S", "A \u{2014} B \u{2014} C", ""), some("S", "A", "B"));
        assert_eq!(t("S", "A\u{2014}B", ""), some("S", "A", "B"));
        assert_eq!(t("S", "Solo", ""), some("S", "Solo", ""));
        assert_eq!(t(" S ", "X - Y", "Album"), some("S", "X - Y", "Album"));
        assert_eq!(t("", "A", ""), None);
    }

    #[test]
    fn personal_stations_never_leak() {
        let it = "It's My Life";
        assert_eq!(
            t(it, "Bon Jovi \u{2014} Crush \u{2014} John Doe\u{2019}s Station", ""),
            some(it, "Bon Jovi", "Crush")
        );
        assert_eq!(t(it, "Bon Jovi \u{2014} Crush \u{2014} John's Station", ""), some(it, "Bon Jovi", "Crush"));
        assert_eq!(t(it, "Bon Jovi", "Crush \u{2014} Estación de John"), some(it, "Bon Jovi", "Crush"));
        assert_eq!(t(it, "Bon Jovi \u{2014} Crush \u{2014} Моя станция (John)", ""), some(it, "Bon Jovi", "Crush"));
        assert_eq!(t(it, "Bon Jovi \u{2014} Johns Sender", ""), some(it, "Bon Jovi", ""));
        assert_eq!(t(it, "Bon Jovi \u{2014} John さんのステーション", ""), some(it, "Bon Jovi", ""));
        assert_eq!(t("John Doe\u{2019}s Station", "", ""), None);
        assert_eq!(t("Station van John", "", ""), None);
        assert_eq!(t("Connecting\u{2026}", "", ""), None);
        assert_eq!(t("connecting...", "", ""), None);
        assert_eq!(t("Connecting", "", ""), None);
        assert_eq!(t("Station", "Artist", ""), some("Station", "Artist", ""));
        // Languages the list doesn't know are still dropped (3rd part).
        assert_eq!(t(it, "Bon Jovi \u{2014} Crush \u{2014} Johns stasjon", ""), some(it, "Bon Jovi", "Crush"));
        assert_eq!(
            t(it, "Bon Jovi \u{2014} Crush \u{2014} Stacja u\u{017c}ytkownika John", ""),
            some(it, "Bon Jovi", "Crush")
        );
        assert_eq!(t(it, "Bon Jovi \u{2014} Crush \u{2014} James\u{2019} Station", ""), some(it, "Bon Jovi", "Crush"));
        // A placeholder title with no artist is never shown, whatever it says.
        assert_eq!(t("Johns stasjon", "", ""), None);
        assert_eq!(t("John Doe\u{2019}s Station", "Bon Jovi", ""), None);
    }
}
