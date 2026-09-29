//! Test-only stand-ins for everything the worker talks to (clock, Apple
//! Music, Discord, iTunes, the config file), driven by a virtual clock, and
//! scenario tests of the worker's promises:
//! - Discord never shows a song that stopped playing more than STALE_MS ago.
//! - What's playing shows up promptly, and the loop never spins.

use super::*;
use crate::itunes::Meta;
use std::cell::{Cell, RefCell};

thread_local! {
    static CLOCK: Cell<i64> = const { Cell::new(10_000_000) };
    /// Apple Music right now: the track and when it (re)started playing.
    static PLAYER: RefCell<Option<(Track, i64)>> = const { RefCell::new(None) };
    static LOOKUP_MS: Cell<i64> = const { Cell::new(0) };
    static LOOKUP_FAIL: Cell<bool> = const { Cell::new(false) };
    static DC: RefCell<Dc> = RefCell::new(Dc::default());
    static CFG: RefCell<(u64, Config)> = RefCell::new((1, Config::default()));
}

fn clock() -> i64 {
    CLOCK.with(|c| c.get())
}

fn advance(ms: i64) {
    CLOCK.with(|c| c.set(c.get() + ms));
}

pub mod sys {
    pub fn ticks() -> i64 {
        super::clock()
    }
    pub fn now_ms() -> i64 {
        1_700_000_000_000 + super::clock()
    }
    pub fn event_new() -> isize {
        0
    }
    pub fn event_set(_: isize) {}
    pub fn event_wait(_: isize, _: u32) {}
    pub fn stat(_: &str) -> Option<(u64, u64)> {
        None
    }
    pub fn delete_file(_: &str) {}
    pub fn append_file(_: &str, _: &[u8]) {}
}

pub mod config {
    use crate::config::Config;
    pub fn dir() -> String {
        "X:\\nowhere".into()
    }
    pub fn stamp() -> Option<(u64, u64)> {
        Some((super::CFG.with(|c| c.borrow().0), 1))
    }
    pub fn load() -> Option<Config> {
        Some(super::CFG.with(|c| c.borrow().1.clone()))
    }
}

#[derive(Default)]
struct Dc {
    running: bool,
    /// Bumped when Discord restarts; old connections are dead.
    generation: u32,
    reject: bool,
    /// The details line Discord shows for us.
    showing: Option<String>,
    updates: u32,
}

pub mod discord {
    use super::{DC, Dc};
    pub const NOT_RUNNING: &str = "Discord is not running";
    pub enum Error {
        Disconnected(String),
        Rejected(String),
    }
    pub struct Discord(u32);

    fn with<R>(f: impl FnOnce(&mut Dc) -> R) -> R {
        DC.with(|d| f(&mut d.borrow_mut()))
    }

    impl Drop for Discord {
        fn drop(&mut self) {
            with(|d| {
                if d.generation == self.0 {
                    d.showing = None; // closing the pipe clears our activity
                }
            });
        }
    }

    impl Discord {
        pub fn connect(_: &str) -> Result<Self, String> {
            with(|d| if d.running { Ok(Discord(d.generation)) } else { Err(NOT_RUNNING.into()) })
        }
        pub fn alive(&mut self) -> bool {
            with(|d| d.running && d.generation == self.0)
        }
        pub fn set_activity(&mut self, a: Option<&str>) -> Result<String, Error> {
            if !self.alive() {
                return Err(Error::Disconnected("pipe closed".into()));
            }
            with(|d| {
                d.updates += 1;
                if a.is_some() && d.reject {
                    return Err(Error::Rejected("invalid payload".into()));
                }
                // (One-letter titles get padded with U+2800; compare the text.)
                let details = a
                    .and_then(crate::json::parse)
                    .and_then(|v| v.str("details").map(|s| s.trim_end_matches('\u{2800}').to_string()));
                d.showing = a.map(|_| details.unwrap_or_default());
                Ok(String::new())
            })
        }
    }
}

/// Like itunes::Lookup, but the "network" just takes LOOKUP_MS.
pub struct Lookup(Vec<(String, Option<Meta>, i64)>);

impl Lookup {
    pub fn new() -> Self {
        Lookup(Vec::new())
    }
    fn hit(&self, t: &Track) -> Option<Option<Meta>> {
        let (_, m, until) = self.0.iter().find(|(k, ..)| *k == t.title)?;
        (m.is_some() || clock() < *until).then(|| m.clone())
    }
    pub fn cached(&self, t: &Track, _: &str, _: u32) -> bool {
        self.hit(t).is_some()
    }
    pub fn find(&mut self, t: &Track, _: &str, _: u32) -> Option<Meta> {
        if let Some(m) = self.hit(t) {
            return m;
        }
        advance(LOOKUP_MS.with(|c| c.get()));
        let meta = (!LOOKUP_FAIL.with(|c| c.get())).then(|| Meta {
            artwork: "https://is1-ssl.mzstatic.com/a/512x512bb.jpg".into(),
            track_url: "https://music.apple.com/song/1".into(),
            artist_url: "https://music.apple.com/artist/2".into(),
            album_url: "https://music.apple.com/album/3".into(),
            track_id: 1,
        });
        let retry = if meta.is_some() { 600_000 } else { 30_000 };
        self.0.retain(|(k, ..)| *k != t.title);
        self.0.push((t.title.clone(), meta.clone(), clock() + retry));
        meta
    }
}

#[derive(Default)]
pub struct Smtc;

impl Smtc {
    pub fn poll(&mut self) -> windows::core::Result<Option<Track>> {
        Ok(PLAYER.with(|p| {
            p.borrow().as_ref().map(|(t, since)| {
                let mut t = t.clone();
                if t.state == State::Playing {
                    t.position_ms = (t.position_ms + clock() - since).min(t.duration_ms);
                }
                t
            })
        }))
    }
}

// ---- scenario driving ----

/// Longest a song may stay on Discord after it stopped playing: the hold
/// limit plus settling and one poll.
const STALE_MS: i64 = HOLD_MS + SETTLE_MS + 1000 + 200;

/// When each song last stopped being the one to show.
struct History {
    current: Option<String>,
    ended: Vec<(String, i64)>,
}

fn track(title: &str) -> Track {
    Track {
        title: title.into(),
        artist: "Artist".into(),
        album: "Album".into(),
        state: State::Playing,
        duration_ms: 240_000,
        position_ms: 0,
    }
}

impl History {
    fn set(&mut self, now_showable: Option<&str>) {
        if let Some(old) = self.current.take() {
            if Some(old.as_str()) != now_showable {
                self.ended.retain(|(t, _)| *t != old);
                self.ended.push((old, clock()));
            } else {
                self.current = Some(old);
                return;
            }
        }
        self.current = now_showable.map(String::from);
    }

    fn play(&mut self, title: &str) {
        PLAYER.with(|p| *p.borrow_mut() = Some((track(title), clock())));
        self.set(Some(title));
    }

    fn pause(&mut self) {
        PLAYER.with(|p| {
            if let Some((t, since)) = p.borrow_mut().as_mut() {
                t.position_ms += clock() - *since;
                t.state = State::Paused;
            }
        });
        self.set(None); // show_paused is off by default
    }

    fn changing(&mut self) {
        PLAYER.with(|p| {
            let mut t = track("");
            t.state = State::Changing;
            *p.borrow_mut() = Some((t, clock()));
        });
        self.set(None);
    }

    fn stop(&mut self) {
        PLAYER.with(|p| *p.borrow_mut() = None);
        self.set(None);
    }

    /// Fails if Discord shows a song that isn't current and ended too long ago.
    fn check(&self) {
        let Some(shown) = DC.with(|d| d.borrow().showing.clone()) else { return };
        if self.current.as_deref() == Some(shown.as_str()) {
            return;
        }
        let ended = self.ended.iter().find(|(t, _)| *t == shown).map(|(_, at)| *at);
        let ago = ended.map(|at| clock() - at);
        assert!(ago.is_some_and(|ago| ago <= STALE_MS), "Discord still shows {shown:?}, stopped {ago:?} ms ago");
    }
}

fn showing() -> Option<String> {
    DC.with(|d| d.borrow().showing.clone())
}

/// Runs the worker for `ms` of virtual time, checking the stale rule after
/// every poll and that it never loops without time passing.
fn run(w: &mut Worker, h: &History, ms: i64) {
    let end = clock() + ms;
    let mut spins = 0;
    while clock() < end {
        let before = clock();
        let sleep = w.tick() as i64;
        h.check();
        if sleep == 0 && clock() == before {
            spins += 1;
            assert!(spins < 5, "worker is spinning");
        } else {
            spins = 0;
        }
        advance(sleep);
    }
}

fn reset(running: bool, lookup_ms: i64) -> (Worker, History) {
    DC.with(|d| *d.borrow_mut() = Dc { running, ..Dc::default() });
    PLAYER.with(|p| *p.borrow_mut() = None);
    LOOKUP_MS.with(|c| c.set(lookup_ms));
    LOOKUP_FAIL.with(|c| c.set(false));
    CFG.with(|c| *c.borrow_mut() = (1, Config::default()));
    SHARED.enabled.store(true, SeqCst);
    (Worker::new(), History { current: None, ended: Vec::new() })
}

#[test]
fn worker_scenarios() {
    // Basic life cycle.
    let (mut w, mut h) = reset(true, 300);
    run(&mut w, &h, 2000);
    assert_eq!(showing(), None);
    h.play("A");
    run(&mut w, &h, 3000);
    assert_eq!(showing().as_deref(), Some("A"));
    h.pause();
    run(&mut w, &h, 1500);
    assert_eq!(showing(), None, "cleared on pause");
    h.play("A");
    run(&mut w, &h, 3000);
    assert_eq!(showing().as_deref(), Some("A"));
    h.stop();
    run(&mut w, &h, 1500);
    assert_eq!(showing(), None, "cleared when Apple Music closes");

    // Skipping fast while each lookup takes almost a second (the old song
    // must not survive the whole burst), then settling on the last one.
    for lookup_ms in [850, 2000, 0] {
        let (mut w, mut h) = reset(true, lookup_ms);
        h.play("start");
        run(&mut w, &h, 4000);
        for i in 0..25 {
            h.play(&format!("skip {i}"));
            run(&mut w, &h, 900 + (i % 3) * 200);
        }
        h.play("final");
        run(&mut w, &h, 25_000); // enough to get past the 5-per-20 s limit
        assert_eq!(showing().as_deref(), Some("final"), "lookup {lookup_ms} ms");
    }

    // A loading state that never ends takes the old song down.
    let (mut w, mut h) = reset(true, 0);
    h.play("A");
    run(&mut w, &h, 3000);
    h.changing();
    run(&mut w, &h, 10_000);
    assert_eq!(showing(), None);

    // Discord starts late: the song appears within a few seconds.
    let (mut w, mut h) = reset(false, 0);
    h.play("A");
    run(&mut w, &h, 180_000);
    DC.with(|d| d.borrow_mut().running = true);
    run(&mut w, &h, 7000);
    assert_eq!(showing().as_deref(), Some("A"), "after Discord starts");

    // Discord restarts mid-song.
    DC.with(|d| {
        let mut d = d.borrow_mut();
        d.generation += 1;
        d.showing = None;
    });
    run(&mut w, &h, 12_000);
    assert_eq!(showing().as_deref(), Some("A"), "after Discord restarts");

    // Hidden from the tray: cleared at once, and back when re-enabled.
    SHARED.enabled.store(false, SeqCst);
    run(&mut w, &h, 1100);
    assert_eq!(showing(), None, "hidden");
    SHARED.enabled.store(true, SeqCst);
    run(&mut w, &h, 3000);
    assert_eq!(showing().as_deref(), Some("A"));

    // Discord refuses the payload: nothing shown, no hammering, and the next
    // song is tried again.
    let (mut w, mut h) = reset(true, 0);
    DC.with(|d| d.borrow_mut().reject = true);
    h.play("A");
    run(&mut w, &h, 30_000);
    assert_eq!(showing(), None);
    assert!(DC.with(|d| d.borrow().updates) <= 3, "retried a refused payload");
    DC.with(|d| d.borrow_mut().reject = false);
    h.play("B");
    run(&mut w, &h, 3000);
    assert_eq!(showing().as_deref(), Some("B"));

    // iTunes down: still shows the song (without art), no spinning.
    let (mut w, mut h) = reset(true, 3000);
    LOOKUP_FAIL.with(|c| c.set(true));
    h.play("A");
    run(&mut w, &h, 60_000);
    assert_eq!(showing().as_deref(), Some("A"));

    // A config edit is picked up (here: keep showing while paused).
    CFG.with(|c| {
        let mut c = c.borrow_mut();
        c.0 += 1;
        c.1.show_paused = true;
    });
    run(&mut w, &h, 1500);
    PLAYER.with(|p| p.borrow_mut().as_mut().unwrap().0.state = State::Paused);
    run(&mut w, &h, 3000);
    assert_eq!(showing().as_deref(), Some("A"), "show_paused = true");

    // A new song replaces the old one directly: the status never blinks off
    // in between, even while its (slow) album-art lookup runs.
    let (mut w, mut h) = reset(true, 850);
    h.play("First");
    run(&mut w, &h, 4000);
    assert_eq!(showing().as_deref(), Some("First"));
    h.play("Second");
    let end = clock() + 5000;
    while clock() < end {
        let sleep = w.tick() as i64;
        h.check();
        assert!(showing().is_some(), "the status blinked off between songs");
        advance(sleep);
    }
    assert_eq!(showing().as_deref(), Some("Second"));

    SHARED.enabled.store(true, SeqCst);
}
