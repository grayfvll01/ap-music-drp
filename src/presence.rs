//! Turns a track into a Discord activity payload.

use crate::config::Config;
use crate::itunes::Meta;
use crate::itunes::norm;
use crate::json::push_str;
use crate::prelude::*;
use crate::smtc::{Player, State, Track};

/// Discord text fields must be 2..=128 characters.
const MAX_TEXT: usize = 128;
const MAX_URL: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub struct Activity {
    /// Which song this is (player, title, artist, album).
    song: String,
    /// JSON object members except timestamps.
    fields: String,
    /// (start, end) in Unix ms.
    ts: Option<(i64, i64)>,
}

impl Activity {
    pub fn json(&self) -> String {
        let mut s = String::with_capacity(self.fields.len() + 64);
        s.push('{');
        s.push_str(&self.fields);
        if let Some((start, end)) = self.ts {
            s.push_str(&format!(r#","timestamps":{{"start":{start},"end":{end}}}"#));
        }
        s.push('}');
        s
    }

    /// The same song, though maybe shown differently (e.g. its album art
    /// arrived).
    pub fn same_song(&self, other: &Activity) -> bool {
        self.song == other.song
    }

    /// Equal content, and progress within `tolerance_ms` (so ticking time
    /// doesn't cause updates, but seeking does).
    pub fn same_as(&self, other: &Activity, tolerance_ms: i64) -> bool {
        self.fields == other.fields
            && match (self.ts, other.ts) {
                (Some(a), Some(b)) => (a.0 - b.0).abs() <= tolerance_ms && (a.1 - b.1).abs() <= tolerance_ms,
                (None, None) => true,
                _ => false,
            }
    }
}

/// "Album - Single" -> "Album" (Apple's store suffixes, not part of the name).
pub fn album_display(album: &str) -> &str {
    for sep in [" - ", " \u{2013} ", " \u{2014} "] {
        for kind in ["Single", "EP"] {
            if let Some(base) = album.strip_suffix(kind).and_then(|a| a.strip_suffix(sep))
                && !base.trim().is_empty()
            {
                return base.trim_end();
            }
        }
    }
    album
}

/// "Song (feat. X) [Remix]" -> ("Song [Remix]", "X"): takes a featured-artist
/// credit ("feat.", "ft.", "featuring", "with" in brackets) out of a title.
pub fn split_feat(title: &str) -> (String, String) {
    let mut from = 0;
    while let Some(i) = title[from..].find(['(', '[']).map(|i| i + from) {
        let close = if title.as_bytes()[i] == b'(' { ')' } else { ']' };
        let Some(len) = title[i..].find(close) else { break };
        let inner = title[i + 1..i + len].trim();
        let lower = inner.to_ascii_lowercase(); // same byte offsets as `inner`
        for key in ["feat. ", "feat ", "ft. ", "ft ", "featuring ", "with "] {
            if lower.starts_with(key) {
                let rest = format!("{} {}", &title[..i], &title[i + len + 1..]);
                let rest = rest.split_whitespace().collect::<Vec<_>>().join(" ");
                return (rest, inner[key.len()..].trim().to_string());
            }
        }
        from = i + len + 1;
    }
    (title.to_string(), String::new())
}

/// The album worth showing: none when it's just the song's own name, as it
/// is for singles ("Song - Single").
fn album_for(t: &Track) -> &str {
    let album = album_display(&t.album);
    let a = norm(album);
    if a.is_empty() || a == norm(&t.title) || a == norm(&split_feat(&t.title).0) { "" } else { album }
}

/// Same text, ignoring case and punctuation.
fn same(a: &Option<String>, b: &Option<String>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if norm(x) == norm(y))
}

/// Fills `{title}`, `{artist}`, `{album}` (in one pass, so a song title
/// containing "{artist}" stays literal) and tidies whitespace.
pub fn render(tpl: &str, t: &Track) -> String {
    let mut s = String::with_capacity(tpl.len() + 64);
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        s.push_str(&rest[..i]);
        rest = &rest[i..];
        let (value, len) = if rest.starts_with("{title}") {
            (t.title.as_str(), 7)
        } else if rest.starts_with("{artist}") {
            (t.artist.as_str(), 8)
        } else if rest.starts_with("{album}") {
            (album_for(t), 7)
        } else {
            ("{", 1)
        };
        s.push_str(value);
        rest = &rest[len..];
    }
    s.push_str(rest);
    let mut out = String::with_capacity(s.len());
    for w in s.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Clamps to Discord's length rules (counted in UTF-16 units, like
/// JavaScript); `None` if there's nothing to show.
fn fit(s: &str) -> Option<String> {
    let n = s.encode_utf16().count();
    if n == 0 {
        return None;
    }
    if n > MAX_TEXT {
        let mut t = String::new();
        let mut units = 0;
        for c in s.chars() {
            if units + c.len_utf16() > MAX_TEXT - 1 {
                break;
            }
            units += c.len_utf16();
            t.push(c);
        }
        t.push('\u{2026}');
        return Some(t);
    }
    // Single characters are rejected; pad with a blank (U+2800) like other clients.
    Some(if n < 2 { format!("{s}\u{2800}") } else { s.to_string() })
}

fn url_ok(u: &str) -> bool {
    u.starts_with("https://") && u.len() <= MAX_URL
}

fn member(out: &mut String, key: &str, value: &str) {
    if !out.is_empty() && !out.ends_with('{') && !out.ends_with('[') {
        out.push(',');
    }
    push_str(out, key);
    out.push(':');
    push_str(out, value);
}

/// Where the track, artist and album link to.
struct Links {
    track: String,
    artist: String,
    album: String,
}

/// A Spotify search page (tracks, artists or albums). Windows doesn't give
/// out Spotify's own ids, and a search for the song lists it first.
fn spotify_search(query: &str, kind: &str) -> String {
    let q = crate::http::encode(query).replace('+', "%20"); // a path, not a query string
    format!("https://open.spotify.com/search/{q}/{kind}")
}

fn links_for(t: &Track, meta: Option<&Meta>) -> Option<Links> {
    match t.player {
        Player::AppleMusic => {
            meta.map(|m| Links { track: m.track_url.clone(), artist: m.artist_url.clone(), album: m.album_url.clone() })
        }
        Player::Spotify => {
            let artist = crate::itunes::main_artist(&t.artist);
            let album = album_display(&t.album);
            Some(Links {
                track: spotify_search(&format!("{} {artist}", t.title), "tracks"),
                artist: spotify_search(artist, "artists"),
                album: if album.is_empty() {
                    String::new()
                } else {
                    spotify_search(&format!("{album} {artist}"), "albums")
                },
            })
        }
    }
}

pub fn build(t: &Track, meta: Option<&Meta>, c: &Config, now_ms: i64) -> Option<Activity> {
    let paused = t.state == State::Paused;
    if paused && !c.show_paused {
        return None;
    }
    let urls = links_for(t, meta);
    let links = if c.links { urls.as_ref() } else { None };
    let (name, fallback_image) = match t.player {
        Player::AppleMusic => (&c.name, &c.fallback_image),
        Player::Spotify => (&c.spotify_name, &c.spotify_fallback_image),
    };
    // "Song — Artist" on the member-list line. The featured-artist credit
    // moves out of the title (it would push the artist out of view) onto the
    // second line; otherwise that line is the album.
    let both = c.status_display == crate::config::SONG_ARTIST;
    let (short, feat) = split_feat(&t.title);
    let (details_text, state_text) = if both {
        let d = if t.artist.is_empty() { short } else { format!("{short} \u{2014} {}", t.artist) };
        let s = if !feat.is_empty() { format!("feat. {feat}") } else { album_for(t).to_string() };
        (d, s)
    } else {
        (render(&c.details, t), render(&c.state, t))
    };
    // Where the second line links: the artist, or in "Song — Artist" the
    // album (a featured-artist credit has no link).
    let state_link = match (both, feat.is_empty()) {
        (false, _) => links.map(|l| l.artist.as_str()),
        (true, true) => links.map(|l| l.album.as_str()),
        (true, false) => None,
    };
    let display = if both { 2 } else { c.status_display };
    let mut f = format!(r#""type":{},"status_display_type":{display}"#, c.activity_type);
    if let Some(name) = fit(name) {
        member(&mut f, "name", &name);
    }

    // Never show the same text twice.
    let details = fit(&details_text);
    let mut state = fit(&state_text);
    if same(&state, &details) {
        state = None;
    }
    let mut large_text = fit(&render(&c.large_text, t));
    if same(&large_text, &details) || same(&large_text, &state) {
        large_text = None;
    }
    if details.is_none() && state.is_none() {
        return None;
    }
    if let Some(d) = &details {
        member(&mut f, "details", d);
        if let Some(l) = links.filter(|l| url_ok(&l.track)) {
            member(&mut f, "details_url", &l.track);
        }
    }
    if let Some(s) = &state {
        member(&mut f, "state", s);
        if let Some(url) = state_link.filter(|u| url_ok(u)) {
            member(&mut f, "state_url", url);
        }
    }

    let image = match meta {
        Some(m) if c.artwork && url_ok(&m.artwork) => m.artwork.as_str(),
        _ if url_ok(fallback_image) => fallback_image.as_str(),
        _ => "",
    };
    if !image.is_empty() {
        f.push_str(r#","assets":{"#);
        member(&mut f, "large_image", image);
        if let Some(lt) = &large_text {
            member(&mut f, "large_text", lt);
        }
        if let Some(l) = links.filter(|l| url_ok(&l.album)) {
            member(&mut f, "large_url", &l.album);
        }
        f.push('}');
    }

    let mut buttons = Vec::new();
    if let Some(l) = urls.as_ref().filter(|l| c.button_listen && url_ok(&l.track)) {
        let label = match t.player {
            Player::AppleMusic => "Listen on Apple Music",
            Player::Spotify => "Listen on Spotify",
        };
        buttons.push((label, l.track.clone()));
    }
    if let Some(m) = meta.filter(|m| c.button_songlink && m.track_id > 0) {
        buttons.push(("song.link", format!("https://song.link/i/{}", m.track_id)));
    }
    if !buttons.is_empty() {
        f.push_str(r#","buttons":["#);
        for (i, (label, url)) in buttons.iter().enumerate() {
            if i > 0 {
                f.push(',');
            }
            f.push('{');
            member(&mut f, "label", label);
            member(&mut f, "url", url);
            f.push('}');
        }
        f.push(']');
    }

    let ts = (!paused && c.show_progress && t.duration_ms > 0).then(|| {
        let start = now_ms - t.position_ms;
        (start, start + t.duration_ms)
    });
    let song = format!("{}\0{}\0{}\0{}", t.player.name(), t.title, t.artist, t.album);
    Some(Activity { song, fields: f, ts })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json;

    fn track(state: State) -> Track {
        Track {
            player: Player::AppleMusic,
            title: "GIMME A HUG".into(),
            artist: "Drake".into(),
            album: "$ome $exy $ongs 4 U".into(),
            state,
            duration_ms: 193_000,
            position_ms: 59_000,
        }
    }

    fn meta() -> Meta {
        Meta {
            artwork: "https://is1-ssl.mzstatic.com/a/512x512bb.jpg".into(),
            track_url: "https://music.apple.com/us/album/g/9?i=3".into(),
            artist_url: "https://music.apple.com/us/artist/drake/271256".into(),
            album_url: "https://music.apple.com/us/album/g/9".into(),
            track_id: 3,
        }
    }

    /// Each setting changes what Discord is sent, and only that.
    #[test]
    fn every_setting_changes_the_payload() {
        let get = |c: &Config, t: &Track| json::parse(&build(t, Some(&meta()), c, 1_000_000).unwrap().json()).unwrap();
        let t = track(State::Playing);
        let d = Config::default();
        let v = get(&d, &t);
        assert!(v.get("timestamps").is_some() && v.str("details_url").is_some());
        assert_eq!(v.get("assets").unwrap().str("large_image"), Some(meta().artwork.as_str()));
        assert!(v.get("buttons").is_none());

        let mut c = d.clone();
        c.artwork = false;
        assert_eq!(get(&c, &t).get("assets").unwrap().str("large_image"), Some(d.fallback_image.as_str()));
        let mut c = d.clone();
        c.show_progress = false;
        assert!(get(&c, &t).get("timestamps").is_none());
        let mut c = d.clone();
        c.links = false;
        let v = get(&c, &t);
        assert!(v.str("details_url").is_none() && v.str("state_url").is_none());
        assert!(v.get("assets").unwrap().str("large_url").is_none());
        let mut c = d.clone();
        c.button_listen = true;
        assert_eq!(get(&c, &t).arr("buttons")[0].str("label"), Some("Listen on Apple Music"));
        let mut c = d.clone();
        c.button_songlink = true;
        assert_eq!(get(&c, &t).arr("buttons")[0].str("url"), Some("https://song.link/i/3"));
        for (i, want) in [0, 1, 2].into_iter().enumerate() {
            let mut c = d.clone();
            c.status_display = i as u8;
            assert_eq!(get(&c, &t).num("status_display_type"), Some(want));
        }
        let mut c = d.clone();
        c.status_display = crate::config::SONG_ARTIST;
        let v = get(&c, &t);
        assert_eq!(v.num("status_display_type"), Some(2));
        assert_eq!(v.str("details"), Some("GIMME A HUG \u{2014} Drake"));
        assert_eq!(v.str("state"), Some("$ome $exy $ongs 4 U"));
        assert_eq!(v.str("state_url"), Some(meta().album_url.as_str()));
        let mut solo = track(State::Playing);
        solo.artist.clear();
        assert_eq!(get(&c, &solo).str("details"), Some("GIMME A HUG"));

        let paused = track(State::Paused);
        assert!(build(&paused, None, &d, 0).is_none(), "paused hidden by default");
        let mut c = d.clone();
        c.show_paused = true;
        assert!(get(&c, &paused).get("timestamps").is_none(), "paused: shown without a time bar");
    }

    #[test]
    fn featured_artists() {
        assert_eq!(
            split_feat("Nena Maldici\u{f3}n (feat. Lenny Tav\u{e1}rez)"),
            ("Nena Maldici\u{f3}n".into(), "Lenny Tav\u{e1}rez".into())
        );
        assert_eq!(split_feat("Song [ft. A & B] (Remix)"), ("Song (Remix)".into(), "A & B".into()));
        assert_eq!(split_feat("Song (Live)"), ("Song (Live)".into(), String::new()));
        assert_eq!(split_feat("Unclosed (feat. X"), ("Unclosed (feat. X".into(), String::new()));
    }

    /// A single whose album is the song's own name, with a featured artist.
    #[test]
    fn singles_never_repeat() {
        let t = Track {
            player: Player::AppleMusic,
            title: "Nena Maldici\u{f3}n (feat. Lenny Tav\u{e1}rez)".into(),
            artist: "Paulo Londra".into(),
            album: "Nena Maldici\u{f3}n (feat. Lenny Tav\u{e1}rez) - Single".into(),
            state: State::Playing,
            duration_ms: 228_000,
            position_ms: 0,
        };
        let mut c = Config::default();
        c.status_display = crate::config::SONG_ARTIST;
        let v = json::parse(&build(&t, Some(&meta()), &c, 0).unwrap().json()).unwrap();
        assert_eq!(v.str("details"), Some("Nena Maldici\u{f3}n \u{2014} Paulo Londra"));
        assert_eq!(v.str("state"), Some("feat. Lenny Tav\u{e1}rez"));
        assert!(v.str("state_url").is_none());
        assert!(v.get("assets").unwrap().str("large_text").is_none(), "no album line for a single");

        // Default layout: song, artist, and no album line either.
        let v = json::parse(&build(&t, None, &Config::default(), 0).unwrap().json()).unwrap();
        assert_eq!(v.str("state"), Some("Paulo Londra"));
        assert!(v.get("assets").unwrap().str("large_text").is_none());

        // Song — Artist with a real album and a featured artist: three lines.
        let mut t2 = t.clone();
        t2.album = "Homerun".into();
        let v = json::parse(&build(&t2, Some(&meta()), &c, 0).unwrap().json()).unwrap();
        assert_eq!(v.str("state"), Some("feat. Lenny Tav\u{e1}rez"));
        assert_eq!(v.get("assets").unwrap().str("large_text"), Some("Homerun"));
        // ...and without a featured artist the album is line 2, not repeated on line 3.
        let mut t3 = t2.clone();
        t3.title = "Party".into();
        let v = json::parse(&build(&t3, Some(&meta()), &c, 0).unwrap().json()).unwrap();
        assert_eq!(v.str("details"), Some("Party \u{2014} Paulo Londra"));
        assert_eq!(v.str("state"), Some("Homerun"));
        assert_eq!(v.str("state_url"), Some(meta().album_url.as_str()));
        assert!(v.get("assets").unwrap().str("large_text").is_none());
    }

    #[test]
    fn spotify() {
        let t = Track {
            player: Player::Spotify,
            title: "Tal Vez".into(),
            artist: "Paulo Londra".into(),
            album: "Homerun".into(),
            state: State::Playing,
            duration_ms: 263_000,
            position_ms: 51_000,
        };
        let mut c = Config { button_listen: true, ..Config::default() };
        // No lookup needed for Spotify's own links; Spotify's name and icon.
        let json = build(&t, None, &c, 0).unwrap().json();
        let v = json::parse(&json).unwrap();
        assert_eq!(v.str("name"), Some("Spotify"));
        assert_eq!(v.str("details_url"), Some("https://open.spotify.com/search/Tal%20Vez%20Paulo%20Londra/tracks"));
        assert_eq!(v.str("state_url"), Some("https://open.spotify.com/search/Paulo%20Londra/artists"));
        let assets = v.get("assets").unwrap();
        assert_eq!(assets.str("large_image"), Some(c.spotify_fallback_image.as_str()));
        assert_eq!(assets.str("large_url"), Some("https://open.spotify.com/search/Homerun%20Paulo%20Londra/albums"));
        assert_eq!(v.arr("buttons")[0].str("label"), Some("Listen on Spotify"));
        assert_eq!(v.arr("buttons")[0].str("url"), v.str("details_url"));

        // With the lookup: album art and song.link, but never Apple Music links.
        c.button_songlink = true;
        let json = build(&t, Some(&meta()), &c, 0).unwrap().json();
        let v = json::parse(&json).unwrap();
        assert_eq!(v.get("assets").unwrap().str("large_image"), Some(meta().artwork.as_str()));
        assert_eq!(v.arr("buttons")[1].str("url"), Some("https://song.link/i/3"));
        assert!(!json.contains("music.apple.com") && !json.contains("Apple Music"), "{json}");

        // Song — Artist: the second line (album) links to the album search.
        let c2 = Config { status_display: crate::config::SONG_ARTIST, ..Config::default() };
        let v = json::parse(&build(&t, None, &c2, 0).unwrap().json()).unwrap();
        assert_eq!(v.str("details"), Some("Tal Vez \u{2014} Paulo Londra"));
        assert_eq!(v.str("state_url"), Some("https://open.spotify.com/search/Homerun%20Paulo%20Londra/albums"));

        // Collaborations search for the main artist; odd characters are escaped.
        let mut t2 = t.clone();
        t2.title = "AC/DC & Me?".into();
        t2.artist = "A, B & C".into();
        let v = json::parse(&build(&t2, None, &Config::default(), 0).unwrap().json()).unwrap();
        assert_eq!(v.str("details_url"), Some("https://open.spotify.com/search/AC%2FDC%20%26%20Me%3F%20A/tracks"));
        assert_eq!(v.str("state_url"), Some("https://open.spotify.com/search/A/artists"));

        // Links off: no links at all.
        let c3 = Config { links: false, ..Config::default() };
        let json = build(&t, None, &c3, 0).unwrap().json();
        assert!(!json.contains("_url"), "{json}");
    }

    #[test]
    fn full_activity_is_valid_json() {
        let mut c = Config::default();
        c.button_listen = true;
        c.button_songlink = true;
        let a = build(&track(State::Playing), Some(&meta()), &c, 1_000_000).unwrap();
        let v = json::parse(&a.json()).expect("valid json");
        assert_eq!(v.str("details"), Some("GIMME A HUG"));
        assert_eq!(v.str("state"), Some("Drake"));
        assert_eq!(v.num("type"), Some(2));
        assert_eq!(v.str("name"), Some("Apple Music"));
        let ts = v.get("timestamps").unwrap();
        assert_eq!(ts.num("start"), Some(941_000));
        assert_eq!(ts.num("end"), Some(1_134_000));
        let assets = v.get("assets").unwrap();
        assert_eq!(assets.str("large_text"), Some("$ome $exy $ongs 4 U"));
        assert_eq!(v.arr("buttons").len(), 2);
    }

    #[test]
    fn paused_and_limits() {
        let c = Config::default();
        assert!(build(&track(State::Paused), None, &c, 0).is_none());
        let mut c2 = c.clone();
        c2.show_paused = true;
        let a = build(&track(State::Paused), None, &c2, 0).unwrap();
        assert!(!a.json().contains("timestamps"));
        let mut t = track(State::Playing);
        t.title = "x".repeat(300);
        t.artist = "Y".into();
        let v = json::parse(&build(&t, None, &c, 0).unwrap().json()).unwrap();
        assert_eq!(v.str("details").unwrap().encode_utf16().count(), 128);
        assert_eq!(v.str("state"), Some("Y\u{2800}"));
        t.title = "\u{1F3B5}".repeat(100); // 200 UTF-16 units
        let v = json::parse(&build(&t, None, &c, 0).unwrap().json()).unwrap();
        assert!(v.str("details").unwrap().encode_utf16().count() <= 128);
    }

    #[test]
    fn album_suffixes() {
        assert_eq!(album_display("Can U Dig That? - Single"), "Can U Dig That?");
        assert_eq!(album_display("Stella! - EP"), "Stella!");
        assert_eq!(album_display("PRIVATE SUITE (COMPLETE EP EDITION)"), "PRIVATE SUITE (COMPLETE EP EDITION)");
        assert_eq!(album_display(" - Single"), " - Single");
        assert_eq!(album_display("Singles"), "Singles");
    }

    #[test]
    fn progress_tolerance() {
        let c = Config::default();
        let a = build(&track(State::Playing), None, &c, 10_000).unwrap();
        let b = build(&track(State::Playing), None, &c, 11_500).unwrap();
        let mut seek = track(State::Playing);
        seek.position_ms += 30_000;
        let s = build(&seek, None, &c, 10_000).unwrap();
        assert!(a.same_as(&b, 2500));
        assert!(!a.same_as(&s, 2500));
        let with_art = build(&track(State::Playing), Some(&meta()), &c, 10_000).unwrap();
        assert!(with_art.same_song(&a) && !with_art.same_as(&a, 2500));
        let mut other = track(State::Playing);
        other.title = "Other".into();
        assert!(!build(&other, None, &c, 10_000).unwrap().same_song(&a));
        other = track(State::Playing);
        other.player = Player::Spotify;
        assert!(!build(&other, None, &c, 10_000).unwrap().same_song(&a));
    }

    #[test]
    fn templates() {
        let t = track(State::Playing);
        assert_eq!(render("  {title}  by {artist} ", &t), "GIMME A HUG by Drake");
        let mut t3 = t.clone();
        t3.title = "{artist} {x".into();
        assert_eq!(render("{title} | {album} | {nope}", &t3), "{artist} {x | $ome $exy $ongs 4 U | {nope}");
        let mut t2 = t.clone();
        t2.album.clear();
        assert_eq!(render("{album}", &t2), "");
    }
}
