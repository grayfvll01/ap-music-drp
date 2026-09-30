//! Album art + Apple Music links via the public iTunes Search API.
//! Only the playing track's own title/artist/album are ever sent.

use crate::http::{self, Client};
use crate::json::{self, Json};
use crate::prelude::*;
use crate::smtc::Track;

#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub artwork: String,
    pub track_url: String,
    pub artist_url: String,
    pub album_url: String,
    pub track_id: u64,
}

pub struct Lookup {
    http: Option<Client>,
    /// (key, result, retry-after). A short list: a HashMap would add hashing
    /// code for no real gain at this size.
    cache: Vec<(String, Option<Meta>, i64)>,
    /// The last artist's song list (artist, response): the next track by the
    /// same artist costs no extra requests.
    artist_songs: Option<(String, Json)>,
}

const MISS_RETRY_MS: i64 = 10 * 60 * 1000;
const FAIL_RETRY_MS: i64 = 30 * 1000;
/// Songs whose length differs by more than this are different recordings.
const DURATION_SLACK_MS: i64 = 5000;
/// Id-based links redirect every viewer to their own store, so they never
/// carry a country, and they're always short.
const APPLE: &str = "https://music.apple.com/";

/// One lookup (up to 6 requests) never takes longer than this.
const LOOKUP_BUDGET_MS: i64 = 10_000;

/// A request failed in a way worth retrying soon (network, rate limit,
/// server error).
struct Transient;

fn fetch(http: &Client, path: &str, deadline: i64) -> Result<Json, Transient> {
    match http.get("itunes.apple.com", path, deadline) {
        Ok(body) => json::parse(&String::from_utf8_lossy(&body)).ok_or(Transient),
        // The store saying no (400: e.g. a region without its own store):
        // same as no results. 403/429 mean "slow down" and are retried.
        Err(400 | 404) => Ok(Json::Null),
        Err(_) => Err(Transient),
    }
}

fn key(t: &Track, country: &str, size: u32) -> String {
    format!("{}\0{}\0{}\0{}\0{}\0{}", t.title, t.artist, t.album, t.duration_ms / 1000, country, size)
}

impl Lookup {
    pub fn new() -> Self {
        Lookup { http: None, cache: Vec::new(), artist_songs: None }
    }

    fn hit(&self, key: &str) -> Option<Option<Meta>> {
        let (_, m, until) = self.cache.iter().find(|(k, ..)| *k == key)?;
        (m.is_some() || crate::sys::ticks() < *until).then(|| m.clone())
    }

    /// True if `find` would answer without going to the network.
    pub fn cached(&self, t: &Track, country: &str, size: u32) -> bool {
        self.hit(&key(t, country, size)).is_some()
    }

    /// The match for `t`. Every call leaves a cache entry (a failure is
    /// retried after FAIL_RETRY_MS), so `cached` is true afterwards.
    pub fn find(&mut self, t: &Track, country: &str, size: u32) -> Option<Meta> {
        let key = key(t, country, size);
        if let Some(m) = self.hit(&key) {
            return m;
        }
        if self.http.is_none() {
            self.http = Client::new();
        }
        let (meta, retry) = match self.search(t, country, size) {
            Ok(m) => (m, MISS_RETRY_MS),
            Err(Transient) => (None, FAIL_RETRY_MS),
        };
        self.cache.retain(|(k, ..)| *k != key);
        if self.cache.len() >= 64 {
            self.cache.remove(0);
        }
        // Counted from now, not from the start: a slow failure mustn't expire
        // before it's even been stored.
        self.cache.push((key, meta.clone(), crate::sys::ticks() + retry));
        meta
    }

    /// Tries, in order: a plain search; the album's own track list (search
    /// doesn't index every song); the artist's song list. Stops at the first
    /// exact match, else uses the best weaker one.
    fn search(&mut self, t: &Track, country: &str, size: u32) -> Result<Option<Meta>, Transient> {
        let Some(http) = self.http.as_ref() else { return Err(Transient) };
        let deadline = crate::sys::ticks() + LOOKUP_BUDGET_MS;
        let get = |path: &str| fetch(http, path, deadline);
        let q = |term: &str, entity: &str, limit: u32| {
            format!("/search?term={}&media=music&entity={entity}&limit={limit}&country={country}", http::encode(term))
        };
        let mut weak = None;
        let mut album_art = None;

        let v = get(&q(&format!("{} {}", t.title, t.artist), "song", 25))?;
        match best(&v, t, size, false) {
            Some((true, m)) => return Ok(Some(m)),
            Some((false, m)) => weak = Some(m),
            None => {}
        }

        if !t.album.is_empty() {
            let v = get(&q(&format!("{} {}", t.album, t.artist), "song", 25))?;
            if let Some(id) = album_id(&v, t) {
                let v = get(&format!("/lookup?id={id}&entity=song&country={country}"))?;
                match best(&v, t, size, true) {
                    Some((true, m)) => return Ok(Some(m)),
                    Some((false, m)) => weak = weak.or(Some(m)),
                    None => {}
                }
                album_art = album_meta(&v, size);
            }
        }

        let main = main_artist(&t.artist);
        if !main.is_empty() {
            if self.artist_songs.as_ref().is_none_or(|(a, _)| *a != t.artist) {
                // Collaborations are filed under the main artist ("ROA &
                // CDobleta" -> ROA); bands keep the full name ("Chase &
                // Status"), which may need its own search.
                let v = get(&q(main, "musicArtist", 10))?;
                let mut ids = named(&v, &t.artist);
                if main != t.artist {
                    if ids.is_empty() {
                        ids = named(&get(&q(&t.artist, "musicArtist", 10))?, &t.artist);
                    }
                    ids.extend(named(&v, main));
                }
                let mut list = String::new();
                for id in ids.iter().take(5) {
                    if !list.is_empty() {
                        list.push(',');
                    }
                    list += &id.to_string();
                }
                let songs = if list.is_empty() {
                    Json::Null
                } else {
                    get(&format!("/lookup?id={list}&entity=song&limit=200&country={country}"))?
                };
                self.artist_songs = Some((t.artist.clone(), songs));
            }
            if let Some((_, songs)) = &self.artist_songs {
                match best(songs, t, size, false) {
                    Some((true, m)) => return Ok(Some(m)),
                    Some((false, m)) => weak = weak.or(Some(m)),
                    None => {}
                }
            }
        }
        Ok(weak.or(album_art))
    }
}

/// Lowercase for comparisons: ASCII, Latin-1, Latin Extended-A, Greek and
/// Cyrillic, which covers song titles in practice. (`char::to_lowercase`
/// would add an 11 KB Unicode table to the exe.)
fn fold(c: char) -> char {
    let u = c as u32;
    let lower = match u {
        0x41..=0x5A | 0xC0..=0xDE if u != 0xD7 => u + 0x20, // A-Z, À-Þ (not ×)
        0x130 => 0x69,                                      // Turkish İ -> i
        // Latin Extended-A: upper/lower pairs (Ā ā, Č č, Ł ł, Ž ž, ...)
        0x100..=0x137 | 0x14A..=0x177 if u.is_multiple_of(2) => u + 1,
        0x139..=0x148 | 0x179..=0x17E if u % 2 == 1 => u + 1,
        0x386 => 0x3AC,                          // Greek Ά
        0x388..=0x38A => u + 0x25,               // Έ Ή Ί
        0x38C => 0x3CC,                          // Ό
        0x38E..=0x38F => u + 0x3F,               // Ύ Ώ
        0x391..=0x3AB if u != 0x3A2 => u + 0x20, // Α-Ϋ
        0x410..=0x42F => u + 0x20,               // Cyrillic А-Я
        0x400..=0x40F => u + 0x50,               // Ѐ-Џ
        _ => u,
    };
    char::from_u32(lower).unwrap_or(c)
}

/// Punctuation and symbols, which never distinguish two songs.
fn is_separator(c: char) -> bool {
    if c.is_ascii() {
        return !c.is_ascii_alphanumeric();
    }
    matches!(c, '\u{A0}'..='\u{BF}' | '\u{D7}' | '\u{F7}' | '\u{2000}'..='\u{206F}' | '\u{3000}'..='\u{303F}' | '\u{FF01}'..='\u{FF0F}')
}

/// Lowercase, keep letters/digits, turn everything else into single spaces.
pub(crate) fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\'' || c == '\u{2019}' {
            // "What’s" and "What's" should match; drop the apostrophe entirely.
        } else if !is_separator(c) {
            out.push(fold(c));
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_string()
}

/// Drops decorations that don't make it a different recording: "(feat. X)",
/// "[with Y]", "(2011 Remaster)", Spotify's " - Remastered 2011" and
/// " - Single"/" - EP". Tags like "(Live)", "(Remix)" or "(Instrumental)"
/// stay, since those are different songs.
fn loose(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(['(', '[']) {
        let close = if rest.as_bytes()[i] == b'(' { ')' } else { ']' };
        let Some(len) = rest[i..].find(close) else { break };
        let inner = norm(&rest[i + 1..i + len]);
        let credit = ["feat", "ft", "featuring", "with", "prod"]
            .iter()
            .any(|p| inner.strip_prefix(p).is_some_and(|r| r.is_empty() || r.starts_with(' ')));
        out.push_str(&rest[..i]);
        if !credit && !inner.contains("remaster") {
            out.push_str(&rest[i..=i + len]);
        }
        rest = &rest[i + len + 1..];
    }
    out.push_str(rest);
    let mut out = out.trim();
    for suffix in [" - Single", " - EP"] {
        out = out.strip_suffix(suffix).unwrap_or(out).trim_end();
    }
    if let Some((base, tag)) = out.split_once(" - ")
        && norm(tag).contains("remaster")
    {
        out = base.trim_end();
    }
    norm(out)
}

/// `exact` if the normalised strings are equal, `loose_eq` if they're equal
/// once decorations are removed, `partial` if one contains the other as whole
/// words, else 0.
fn similarity(a: &str, b: &str, exact: i32, loose_eq: i32, partial: i32) -> i32 {
    if a.is_empty() || b.is_empty() {
        return 0;
    }
    let (na, nb) = (norm(a), norm(b));
    if !na.is_empty() && na == nb {
        return exact;
    }
    let (la, lb) = (loose(a), loose(b));
    if !la.is_empty() && la == lb {
        return loose_eq;
    }
    let pad = |s: &str| format!(" {s} ");
    if partial > 0 && !la.is_empty() && !lb.is_empty() && (pad(&na).contains(&pad(&lb)) || pad(&nb).contains(&pad(&la)))
    {
        return partial;
    }
    0
}

/// Best matching song in an iTunes response, and whether it's an exact
/// match. Doesn't guess: the title must match (never partially), the artist
/// must too (unless these are the verified album's own tracks), a loose
/// title match needs the exact artist, and the length must agree.
fn best(v: &Json, t: &Track, size: u32, album_verified: bool) -> Option<(bool, Meta)> {
    let mut top: Option<(i32, bool, &Json)> = None;
    for r in v.arr("results") {
        if r.str("kind") != Some("song") {
            continue;
        }
        let title = similarity(r.str("trackName").unwrap_or(""), &t.title, 10, 7, 0);
        let artist = similarity(r.str("artistName").unwrap_or(""), &t.artist, 6, 5, 3);
        let album = similarity(r.str("collectionName").unwrap_or(""), &t.album, 4, 3, 1);
        let artist_ok = album_verified || artist > 0;
        let loose_ok = title == 10 || artist == 6 || album_verified;
        let length_ok = t.duration_ms <= 0
            || r.num("trackTimeMillis").is_none_or(|ms| (ms - t.duration_ms).abs() <= DURATION_SLACK_MS);
        if title == 0 || !artist_ok || !loose_ok || !length_ok {
            continue;
        }
        let explicit = (r.str("trackExplicitness") != Some("cleaned")) as i32;
        let score = title * 4 + artist * 3 + album * 3 + explicit;
        if top.is_none_or(|(s, ..)| score > s) {
            top = Some((score, title == 10, r));
        }
    }
    let (_, exact, r) = top?;
    Some((exact, meta(r, size)))
}

fn meta(r: &Json, size: u32) -> Meta {
    let art = r.str("artworkUrl100").unwrap_or("");
    let artwork = match art.rsplit_once('/') {
        Some((base, _)) if art.starts_with("https://") && art.contains(".mzstatic.com/") => {
            format!("{base}/{size}x{size}bb.jpg")
        }
        _ => String::new(),
    };
    let id = |k: &str| r.num(k).filter(|&n| n > 0);
    let link = |kind: &str, k: &str| id(k).map(|n| format!("{APPLE}{kind}/{n}")).unwrap_or_default();
    Meta {
        artwork,
        track_url: link("song", "trackId"),
        artist_url: link("artist", "artistId"),
        album_url: link("album", "collectionId"),
        track_id: id("trackId").unwrap_or(0) as u64,
    }
}

/// Collection id of the result whose album (and artist) matches the track.
fn album_id(v: &Json, t: &Track) -> Option<u64> {
    let mut top: Option<(i32, i64)> = None;
    for r in v.arr("results") {
        let album = similarity(r.str("collectionName").unwrap_or(""), &t.album, 10, 7, 0);
        let artist = similarity(r.str("artistName").unwrap_or(""), &t.artist, 6, 5, 3).max(similarity(
            r.str("collectionArtistName").unwrap_or(""),
            &t.artist,
            6,
            5,
            3,
        ));
        let (Some(id), true) = (r.num("collectionId"), album > 0 && artist > 0) else { continue };
        if top.is_none_or(|(s, _)| album + artist > s) {
            top = Some((album + artist, id));
        }
    }
    top.map(|(_, id)| id as u64)
}

/// "ROA & CDobleta" / "ROA, Omar Courtz & Bryant Myers" -> "ROA". (Band names
/// like "Tyler, The Creator" are handled by also accepting the full name.)
pub(crate) fn main_artist(artist: &str) -> &str {
    let end = [" & ", ", ", " x ", " feat. "].iter().filter_map(|sep| artist.find(sep)).min().unwrap_or(artist.len());
    artist[..end].trim()
}

/// Ids of the artists named exactly `name` (several artists can share one).
fn named(v: &Json, name: &str) -> Vec<i64> {
    let want = norm(name);
    v.arr("results")
        .iter()
        .filter(|r| r.str("artistName").is_some_and(|n| norm(n) == want))
        .filter_map(|r| r.num("artistId"))
        .collect()
}

/// Album-level art and links when the exact track isn't listed.
fn album_meta(v: &Json, size: u32) -> Option<Meta> {
    let c = v.arr("results").iter().find(|r| r.str("wrapperType") == Some("collection"))?;
    Some(meta(c, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smtc::{Player, State};

    fn track(title: &str, artist: &str, album: &str) -> Track {
        Track {
            player: Player::AppleMusic,
            title: title.into(),
            artist: artist.into(),
            album: album.into(),
            state: State::Playing,
            duration_ms: 0,
            position_ms: 0,
        }
    }

    fn pick(v: &Json, t: &Track, album_verified: bool) -> Option<Meta> {
        best(v, t, 512, album_verified).map(|(_, m)| m)
    }

    #[test]
    fn matching() {
        let v = json::parse(r#"{"results":[
            {"kind":"song","trackName":"Other Song","artistName":"Drake","collectionName":"X","trackId":1},
            {"kind":"song","trackName":"GIMME A HUG","artistName":"Drake","collectionName":"$ome $exy $ongs 4 U","trackExplicitness":"cleaned","trackId":2,
             "artworkUrl100":"https://is1-ssl.mzstatic.com/image/thumb/a/b.jpg/100x100bb.jpg","artistId":271256,"collectionId":9},
            {"kind":"song","trackName":"GIMME A HUG","artistName":"Drake","collectionName":"$ome $exy $ongs 4 U","trackExplicitness":"explicit","trackId":3,
             "artworkUrl100":"https://is1-ssl.mzstatic.com/image/thumb/a/c.jpg/100x100bb.jpg","artistId":271256,"collectionId":9}
        ]}"#).unwrap();
        let (exact, m) = best(&v, &track("GIMME A HUG", "Drake", "$ome $exy $ongs 4 U"), 512, false).unwrap();
        assert!(exact);
        assert_eq!(m.track_id, 3);
        assert_eq!(m.artwork, "https://is1-ssl.mzstatic.com/image/thumb/a/c.jpg/512x512bb.jpg");
        assert_eq!(m.track_url, "https://music.apple.com/song/3");
        assert_eq!(m.artist_url, "https://music.apple.com/artist/271256");
        assert_eq!(m.album_url, "https://music.apple.com/album/9");
        assert!(pick(&v, &track("Nope", "Nobody", ""), false).is_none());
    }

    #[test]
    fn album_fallback() {
        let t = track("Bellakéame", "ROA", "PRIVATE SUITE (COMPLETE EP EDITION)");
        let search = json::parse(r#"{"results":[
            {"kind":"song","trackName":"11:11","artistName":"ROA","collectionName":"PRIVATE SUITE (COMPLETE EP EDITION)","collectionId":1874368512},
            {"kind":"song","trackName":"x","artistName":"Someone","collectionName":"PRIVATE SUITE (COMPLETE EP EDITION)","collectionId":5}
        ]}"#).unwrap();
        assert_eq!(album_id(&search, &t), Some(1874368512));
        let lookup = json::parse(r#"{"results":[
            {"wrapperType":"collection","collectionName":"PRIVATE SUITE (COMPLETE EP EDITION)","artworkUrl100":"https://is1-ssl.mzstatic.com/b/100x100bb.jpg",
             "collectionId":1874368512,"artistId":1},
            {"wrapperType":"track","kind":"song","trackName":"Bellakéame","artistName":"ROA","collectionName":"PRIVATE SUITE (COMPLETE EP EDITION)","trackId":1874368536}
        ]}"#).unwrap();
        assert_eq!(pick(&lookup, &t, true).unwrap().track_id, 1874368536);
        let other = track("Unlisted", "ROA", "PRIVATE SUITE (COMPLETE EP EDITION)");
        assert!(pick(&lookup, &other, true).is_none());
        let m = album_meta(&lookup, 300).unwrap();
        assert_eq!(m.artwork, "https://is1-ssl.mzstatic.com/b/300x300bb.jpg");
        assert_eq!(m.album_url, "https://music.apple.com/album/1874368512");
        assert_eq!((m.track_id, m.track_url.as_str()), (0, ""));
    }

    /// Hits the real iTunes API: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_lookups() {
        let mut l = Lookup::new();
        for (title, artist, album, secs) in [
            ("GIMME A HUG", "Drake", "$ome $exy $ongs 4 U", 193), // plain search
            ("Bellakéame", "ROA", "PRIVATE SUITE (COMPLETE EP EDITION)", 123), // via album
            ("NunK es Tarde", "ROA", "NunK es Tarde - Single", 0), // via artist
            ("Who Am I (What’s My Name)?", "Snoop Dogg", "Doggystyle (30th Anniversary Edition)", 0),
            ("EARFQUAKE", "Tyler, The Creator", "IGOR", 0), // band name with a comma
        ] {
            let mut t = track(title, artist, album);
            t.duration_ms = secs * 1000;
            let m = l.find(&t, "us", 512).unwrap_or_else(|| panic!("no match for {title}"));
            assert!(m.artwork.starts_with("https://") && m.artwork.ends_with("/512x512bb.jpg"), "{title}");
            assert!(m.track_url.starts_with("https://music.apple.com/song/"), "{title}: {}", m.track_url);
            assert!(m.track_id > 0, "{title}");
        }
        // Territories without a store answer 400: that's "no match", not an error.
        let pr = track("GIMME A HUG", "Drake", "");
        assert_eq!(l.find(&pr, "pr", 512), None);
        assert!(l.cached(&pr, "pr", 512));
        // A band with "&" in its name, and the right version of the song.
        let mut t = track("Blind Faith (feat. Liam Bailey)", "Chase & Status", "No More Idols");
        t.duration_ms = 233_000;
        let m = l.find(&t, "us", 512).expect("Chase & Status");
        assert!(m.track_id > 0 && m.album_url.starts_with("https://music.apple.com/album/"));
    }

    #[test]
    fn artist_fallback() {
        assert_eq!(main_artist("ROA & CDobleta"), "ROA");
        assert_eq!(main_artist("ROA, Omar Courtz & Bryant Myers"), "ROA");
        assert_eq!(main_artist("Drake"), "Drake");
        let v = json::parse(
            r#"{"results":[
            {"wrapperType":"artist","artistName":"ROA","artistId":1617105458},
            {"wrapperType":"artist","artistName":"Roa","artistId":1517622780},
            {"wrapperType":"artist","artistName":"ROA Band","artistId":7}
        ]}"#,
        )
        .unwrap();
        assert_eq!(named(&v, "ROA"), [1617105458, 1517622780]);
        let v = json::parse(
            r#"{"results":[
            {"artistName":"Tyler","artistId":184964},
            {"artistName":"Tyler, The Creator","artistId":420368335}
        ]}"#,
        )
        .unwrap();
        assert_eq!(named(&v, "Tyler, The Creator"), [420368335]);
        assert_eq!(named(&v, main_artist("Tyler, The Creator")), [184964]);
        let songs = json::parse(r#"{"results":[
            {"wrapperType":"artist","artistName":"ROA"},
            {"wrapperType":"track","kind":"song","trackName":"NunK es Tarde","artistName":"ROA","collectionName":"Urbano Pegado","trackId":1},
            {"wrapperType":"track","kind":"song","trackName":"NunK es Tarde","artistName":"ROA","collectionName":"NunK es Tarde - Single","trackId":2}
        ]}"#).unwrap();
        assert_eq!(pick(&songs, &track("NunK es Tarde", "ROA", "NunK es Tarde - Single"), false).unwrap().track_id, 2);
    }

    #[test]
    fn no_guessing() {
        let v = json::parse(r#"{"results":[
            {"kind":"song","trackName":"Love Yourself","artistName":"Justin Bieber","collectionName":"Purpose","trackId":11},
            {"kind":"song","trackName":"Tarde","artistName":"ROA","collectionName":"X","trackId":12},
            {"kind":"song","trackName":"Intro","artistName":"Somebody Else","collectionName":"Live","trackId":13},
            {"kind":"song","trackName":"Hello (Live at the BBC)","artistName":"Adele","collectionName":"Y","trackId":14},
            {"kind":"song","trackName":"Tamale (Instrumental)","artistName":"Tyler, The Creator","collectionName":"Wolf + Instrumentals","trackId":15},
            {"kind":"song","trackName":"Meet the Grahams (beat)","artistName":"ALF RUSS BEATS","collectionName":"Meet the Grahams - Single","trackId":16,"trackTimeMillis":135000},
            {"kind":"song","trackName":"Sprint","artistName":"Runner","collectionName":"Z","trackId":17,"trackTimeMillis":100000}
        ]}"#).unwrap();
        assert!(pick(&v, &track("Love", "Justin Bieber", ""), false).is_none());
        assert!(pick(&v, &track("NunK es Tarde", "ROA", ""), false).is_none());
        assert!(pick(&v, &track("Intro", "My Local Band", "Live"), false).is_none());
        assert!(pick(&v, &track("Hello", "Adele", ""), false).is_none());
        assert!(pick(&v, &track("Tamale", "Tyler, The Creator", "Wolf"), false).is_none());
        assert!(pick(&v, &track("meet the grahams", "Kendrick Lamar", "meet the grahams - Single"), false).is_none());
        let mut t = track("Sprint", "Runner", "");
        t.duration_ms = 200_000;
        assert!(pick(&v, &t, false).is_none()); // same name, different length
        t.duration_ms = 101_000;
        assert_eq!(pick(&v, &t, false).unwrap().track_id, 17);
    }

    #[test]
    fn links_never_carry_country() {
        let r = json::parse(
            r#"{"trackId":5,"artistId":6,"collectionId":7,"artworkUrl100":"https://evil.example/us/100x100bb.jpg",
            "trackViewUrl":"https://music.apple.com/us/album/x/7?i=5&uo=4"}"#,
        )
        .unwrap();
        let m = meta(&r, 512);
        assert_eq!(m.track_url, "https://music.apple.com/song/5");
        assert_eq!(m.artist_url, "https://music.apple.com/artist/6");
        assert_eq!(m.album_url, "https://music.apple.com/album/7");
        assert_eq!(m.artwork, ""); // only Apple's image host is trusted
        let none = meta(&json::parse("{}").unwrap(), 512);
        assert_eq!((none.track_url.as_str(), none.artist_url.as_str(), none.album_url.as_str()), ("", "", ""));
    }

    #[test]
    fn normalising() {
        assert_eq!(norm("Who Am I (What’s My Name)?"), "who am i whats my name");
        assert_eq!(norm("BELLAKÉAME — ¡Qué Tal!"), "bellakéame qué tal");
        assert_eq!(norm("ΑΓΆΠΗ Любовь ПЕСНЯ"), norm("αγάπη любовь песня"));
        assert_eq!(norm("夜に駆ける"), "夜に駆ける");
        assert_eq!(norm("ŁÓDŹ ŻÓŁĆ Č"), "łódź żółć č");
        // Must agree with the real Unicode lowercase for everything it maps.
        for c in (0u32..0x500).filter_map(char::from_u32) {
            let f = fold(c);
            if f != c {
                assert_eq!(Some(f), c.to_lowercase().next(), "{c:?}");
            }
        }
        assert_eq!(norm("×÷"), "");
        assert_eq!(loose("Touch Away (feat. October London) - Single"), "touch away");
        assert_eq!(loose("Ten Crack Commandments (2007 Remaster)"), "ten crack commandments");
        assert_eq!(loose("Song [with Friend]"), "song");
        assert_eq!(loose("Song (Live)"), "song live");
        assert_eq!(loose("Song (Featured)"), "song featured");
        // Spotify's version suffixes.
        assert_eq!(loose("Bohemian Rhapsody - Remastered 2011"), "bohemian rhapsody");
        assert_eq!(loose("Song - 2009 Remaster"), "song");
        assert_eq!(loose("Song - Live at Wembley"), "song live at wembley");
        assert_eq!(similarity("Bohemian Rhapsody", "Bohemian Rhapsody - Remastered 2011", 10, 7, 0), 7);
        assert_eq!(similarity("Regulate (feat. Nate Dogg)", "Regulate", 10, 7, 4), 7);
        assert_eq!(similarity("Dr. Dre & Snoop Dogg", "Dr. Dre", 6, 5, 3), 3);
        assert_eq!(similarity("Dreamer", "Dre", 6, 5, 3), 0);
        assert_eq!(similarity("Love Yourself", "Love", 10, 7, 0), 0);
    }
}
