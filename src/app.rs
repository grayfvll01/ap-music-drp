//! Background worker: Apple Music -> presence -> Discord, plus shared UI state.
//!
//! Rules that keep the status honest:
//! - Nothing is shown unless the Apple Music session says so right now.
//! - The previous song is never kept up for more than HOLD_MS while Apple
//!   Music is between states; clearing is immediate and never rate-limited.
//! - If Discord refuses an update, the connection is dropped, which is
//!   guaranteed to clear our activity.

use crate::config::Config;
use crate::prelude::*;
use crate::presence::{self, Activity};
use crate::smtc::{State, Track};
use crate::sys::Lock;
// Tests swap the outside world (clock, Apple Music, Discord, iTunes, config
// file) for a scripted simulation; see app_sim.rs.
#[cfg(test)]
#[path = "app_sim.rs"]
mod sim;
use core::sync::atomic::{AtomicBool, AtomicIsize, Ordering::SeqCst};
#[cfg(test)]
use sim::{Lookup, Smtc, config, discord, discord::Discord, sys};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
#[cfg(not(test))]
use {
    crate::discord::{self, Discord},
    crate::itunes::Lookup,
    crate::smtc::Smtc,
    crate::{config, sys},
};

/// Posted to the tray window when `SHARED.status` changes.
pub const WM_STATUS: u32 = WM_APP + 2;

/// Timestamps closer than this are "the same" (avoids needless updates).
const SEEK_TOLERANCE_MS: i64 = 2500;
/// A new track/state must look the same for this long before it's published
/// (Apple Music briefly reports half-updated metadata while switching).
const SETTLE_MS: i64 = 600;
/// Longest a loading / half-updated state may keep the previous song up.
const HOLD_MS: i64 = 5000;
/// Reconnect backoff (Discord limits new RPC connections per minute).
const BACKOFF_MIN_MS: i64 = 5_000;
const BACKOFF_MAX_MS: i64 = 120_000;
/// A payload Discord refused isn't retried for this long.
const REJECT_COOLDOWN_MS: i64 = 60_000;
/// Discord accepts about 5 activity updates per 20 s.
const RATE_COUNT: usize = 5;
const RATE_WINDOW_MS: i64 = 20_000;

#[derive(Clone, PartialEq)]
pub struct Status {
    pub playing: String,
    pub discord: String,
}

pub struct Shared {
    pub enabled: AtomicBool,
    pub quit: AtomicBool,
    pub done: AtomicBool,
    pub status: Lock<Status>,
    pub hwnd: AtomicIsize,
    wake: AtomicIsize,
}

pub static SHARED: Shared = Shared {
    enabled: AtomicBool::new(true),
    quit: AtomicBool::new(false),
    done: AtomicBool::new(false),
    status: Lock::new(Status { playing: String::new(), discord: String::new() }),
    hwnd: AtomicIsize::new(0),
    wake: AtomicIsize::new(0),
};

impl Shared {
    pub fn init(&self) {
        self.wake.store(sys::event_new(), SeqCst);
    }

    /// Interrupts the worker's sleep so it reacts immediately.
    pub fn wake(&self) {
        sys::event_set(self.wake.load(SeqCst));
    }

    fn sleep(&self, ms: u32) {
        sys::event_wait(self.wake.load(SeqCst), ms);
    }

    fn set_status(&self, st: Status) {
        if self.status.with(|s| {
            if *s == st {
                false
            } else {
                *s = st;
                true
            }
        }) {
            let h = HWND(self.hwnd.load(SeqCst) as _);
            unsafe {
                let _ = PostMessageW(Some(h), WM_STATUS, WPARAM(0), LPARAM(0));
            }
        }
    }
}

pub fn log(cfg: &Config, msg: &str) {
    if !cfg.log {
        return;
    }
    let path = config::dir() + "\\log.txt";
    if sys::stat(&path).is_some_and(|(_, size)| size > 1 << 20) {
        sys::delete_file(&path);
    }
    let t = sys::now_ms() / 1000;
    let line = format!("[{:02}:{:02}:{:02} UTC] {msg}\r\n", t / 3600 % 24, t / 60 % 60, t % 60);
    sys::append_file(&path, line.as_bytes());
}

/// Two-letter store code for iTunes lookups.
pub fn country(cfg: &Config) -> String {
    if cfg.country != "auto" {
        return cfg.country.clone();
    }
    let mut buf = [0u16; 16];
    let n = unsafe { windows::Win32::Globalization::GetUserDefaultGeoName(&mut buf) };
    let geo = if n > 1 { String::from_utf16_lossy(&buf[..(n as usize - 1).min(buf.len())]) } else { String::new() };
    if !(geo.len() == 2 && geo.bytes().all(|b| b.is_ascii_alphabetic())) {
        return "us".into();
    }
    let geo = geo.to_ascii_lowercase();
    // Territories without a store of their own use their parent country's.
    const PARENT: [(&str, &str); 25] = [
        ("pr", "us"),
        ("gu", "us"),
        ("vi", "us"),
        ("as", "us"),
        ("mp", "us"),
        ("um", "us"),
        ("gp", "fr"),
        ("mq", "fr"),
        ("gf", "fr"),
        ("re", "fr"),
        ("yt", "fr"),
        ("pm", "fr"),
        ("bl", "fr"),
        ("mf", "fr"),
        ("wf", "fr"),
        ("pf", "fr"),
        ("nc", "fr"),
        ("gg", "gb"),
        ("je", "gb"),
        ("im", "gb"),
        ("fo", "dk"),
        ("gl", "dk"),
        ("ax", "fi"),
        ("cw", "nl"),
        ("sx", "nl"),
    ];
    PARENT.iter().find(|(t, _)| *t == geo).map_or(geo.clone(), |(_, p)| p.to_string())
}

pub fn wants_lookup(cfg: &Config) -> bool {
    cfg.artwork || cfg.links || cfg.button_listen || cfg.button_songlink
}

fn describe(t: &Track) -> String {
    if t.artist.is_empty() { t.title.clone() } else { format!("{} \u{2014} {}", t.title, t.artist) }
}

/// What a track "is" for settling purposes (duration in whole seconds, since
/// the ms value can wobble).
type Ident = (String, String, String, State, i64);

fn ident(t: &Track) -> Ident {
    (t.title.clone(), t.artist.clone(), t.album.clone(), t.state, t.duration_ms / 1000)
}

enum Want {
    Show(Track),
    Clear,
    Hold,
}

struct Worker {
    cfg: Config,
    cfg_stamp: Option<(u64, u64)>,
    cfg_missing: bool,
    country: String,
    smtc: Smtc,
    lookup: Lookup,
    dc: Option<Discord>,
    dc_error: String,
    retry_at: i64,
    backoff: i64,
    /// What Discord currently shows for us (None = nothing).
    shown: Option<Activity>,
    /// Last payload Discord refused, and when.
    rejected: Option<(Activity, i64)>,
    /// Times of the last RATE_COUNT updates (a ring; oldest at `send_at`).
    sends: [i64; RATE_COUNT],
    send_at: usize,
    ident: Option<Ident>,
    ident_since: i64,
    /// Last time the situation was definite (a song to show, or nothing).
    last_definite: i64,
    last_logged: String,
    playing_line: String,
}

pub fn run() {
    let mut w = Worker::new();
    log(&w.cfg, concat!("started v", env!("CARGO_PKG_VERSION")));
    while !SHARED.quit.load(SeqCst) {
        let ms = w.tick();
        if ms > 0 {
            SHARED.sleep(ms);
        }
    }
    if let Some(d) = &mut w.dc {
        let _ = d.set_activity(None);
    }
    drop(w.dc.take());
    log(&w.cfg, "stopped");
    SHARED.done.store(true, SeqCst);
}

impl Worker {
    fn new() -> Self {
        let cfg = config::load().unwrap_or_default();
        Worker {
            country: country(&cfg),
            // Unknown, so the first tick reads the file again: covers a read that
            // failed just now (e.g. an editor mid-save).
            cfg_stamp: None,
            cfg_missing: false,
            cfg,
            smtc: Smtc::default(),
            lookup: Lookup::new(),
            dc: None,
            dc_error: String::new(),
            retry_at: 0,
            backoff: BACKOFF_MIN_MS,
            shown: None,
            rejected: None,
            sends: [-RATE_WINDOW_MS - 1; RATE_COUNT],
            send_at: 0,
            ident: None,
            ident_since: 0,
            last_definite: sys::ticks(),
            last_logged: String::new(),
            playing_line: String::new(),
        }
    }

    fn reload_config(&mut self) {
        let stamp = config::stamp();
        if stamp == self.cfg_stamp {
            return;
        }
        // Some editors briefly remove the file while saving: only treat it as
        // deleted (and recreate the defaults) if it's still gone next time.
        if stamp.is_none() && !self.cfg_missing {
            self.cfg_missing = true;
            return;
        }
        self.cfg_missing = false;
        // Unreadable right now (e.g. mid-save): keep the current settings and
        // look again next time. Same if it changed while being read.
        let Some(c) = config::load() else { return };
        let after = config::stamp();
        if stamp.is_some() && after != stamp {
            return;
        }
        self.cfg_stamp = after;
        if c.client_id != self.cfg.client_id {
            self.disconnect(); // closing the pipe clears the old app's presence
            self.retry_at = 0;
            self.backoff = BACKOFF_MIN_MS;
        }
        self.cfg = c;
        let cc = country(&self.cfg);
        if cc != self.country {
            self.lookup = Lookup::new(); // its artist cache is per store
            self.country = cc;
        }
        self.rejected = None;
        log(&self.cfg, "config reloaded");
    }

    fn disconnect(&mut self) {
        self.dc = None;
        self.shown = None;
    }

    fn schedule_reconnect(&mut self, mono: i64) {
        self.retry_at = mono + self.backoff;
        self.backoff = (self.backoff * 2).min(BACKOFF_MAX_MS);
    }

    fn ensure_connected(&mut self, mono: i64) {
        if self.dc.is_some() || mono < self.retry_at {
            return;
        }
        match Discord::connect(&self.cfg.client_id) {
            Ok(d) => {
                log(&self.cfg, "discord connected");
                self.dc = Some(d);
                self.dc_error.clear();
            }
            Err(e) => {
                if e != self.dc_error {
                    log(&self.cfg, &format!("discord: {e}"));
                }
                // No pipe at all: Discord is closed, so its connection limit
                // doesn't apply; keep checking every few seconds.
                if e == discord::NOT_RUNNING {
                    self.backoff = BACKOFF_MIN_MS;
                }
                self.dc_error = e;
                self.schedule_reconnect(mono);
            }
        }
    }

    /// One poll. Returns how long to sleep before the next one.
    fn tick(&mut self) -> u32 {
        let mono = sys::ticks();
        self.reload_config();

        // Notice a Discord restart first, so a hold below can't resurrect an
        // old song on a fresh connection.
        if self.dc.as_mut().is_some_and(|d| !d.alive()) {
            log(&self.cfg, "discord disconnected");
            self.disconnect();
            self.schedule_reconnect(mono);
        }

        let enabled = SHARED.enabled.load(SeqCst);
        let now = sys::now_ms();
        let track = if enabled {
            self.smtc.poll().unwrap_or_else(|e| {
                log(&self.cfg, &format!("media session error {:#x}", e.code().0));
                None // fail closed: show nothing
            })
        } else {
            None
        };

        let id = track.as_ref().map(ident);
        if id != self.ident {
            self.ident = id;
            self.ident_since = mono;
        }
        let settled = mono - self.ident_since >= SETTLE_MS;

        let want = match track {
            None => Want::Clear,
            Some(t) if t.state == State::Changing => Want::Hold,
            // Paused with show_paused off, or every line templated away.
            Some(t) if presence::build(&t, None, &self.cfg, now).is_none() => Want::Clear,
            Some(_) if !settled => Want::Hold,
            Some(t) => Want::Show(t),
        };

        let mut sleep_ms = self.cfg.poll_ms;
        let mut hold_expired = false;
        let mut shown_track = None;
        let holding = matches!(want, Want::Hold);
        let desired = match want {
            Want::Clear => {
                self.last_definite = mono;
                None
            }
            Want::Hold => {
                sleep_ms = sleep_ms.min(SETTLE_MS as u32);
                hold_expired = mono - self.last_definite > HOLD_MS;
                if hold_expired { None } else { self.shown.clone() }
            }
            Want::Show(t) => {
                // No point looking anything up for a song nobody will see.
                self.ensure_connected(mono);
                let mut act = None;
                if self.dc.is_some() {
                    let (size, lookup) = (self.cfg.artwork_size, wants_lookup(&self.cfg));
                    if lookup && !self.lookup.cached(&t, &self.country, size) {
                        // A lookup can take seconds: put the new song up
                        // straight away (replacing the old one, without art
                        // or links), then look it up. The next poll re-checks
                        // Apple Music and the toggle, and adds the art.
                        let now_playing = presence::build(&t, None, &self.cfg, now);
                        self.sync(now_playing, mono);
                        self.lookup.find(&t, &self.country, size);
                        return 0;
                    }
                    let meta = if lookup { self.lookup.find(&t, &self.country, size) } else { None };
                    act = presence::build(&t, meta.as_ref(), &self.cfg, now);
                }
                self.last_definite = mono;
                shown_track = Some(t);
                act
            }
        };

        let now_playing = match &shown_track {
            Some(t) => Some(describe(t)),
            None if self.ident.is_none() => Some(String::from("(nothing)")),
            None => None, // in between: not worth a log line
        };
        if let Some(k) = now_playing.filter(|k| *k != self.last_logged) {
            log(&self.cfg, &format!("track: {k}"));
            self.last_logged = k;
        }

        self.sync(desired, mono);

        // Tray status.
        self.playing_line = if !enabled {
            "Hidden from Discord".into()
        } else if let Some(t) = &shown_track {
            let icon = if t.state == State::Paused { '\u{23F8}' } else { '\u{25B6}' };
            format!("{icon} {}", describe(t))
        } else if self.ident.is_none() {
            "Apple Music: nothing playing".into()
        } else if hold_expired {
            "Apple Music: loading\u{2026}".into()
        } else if matches!(self.ident, Some((_, _, _, State::Paused, _))) {
            "Apple Music: paused".into()
        } else if holding {
            core::mem::take(&mut self.playing_line) // keep the last line while switching
        } else {
            "Apple Music: nothing to show".into() // every line templated away
        };
        let refusing =
            self.shown.is_none() && self.rejected.as_ref().is_some_and(|(_, at)| mono - at < REJECT_COOLDOWN_MS);
        let discord = if refusing {
            "Discord: rejected the update (retrying in a minute)".into()
        } else if self.dc.is_some() && self.dc_error.is_empty() {
            if self.shown.is_some() { "Discord: showing" } else { "Discord: connected" }.to_string()
        } else if !self.dc_error.is_empty() {
            format!("Discord: {}", self.dc_error)
        } else {
            "Discord: idle".into()
        };
        SHARED.set_status(Status { playing: self.playing_line.clone(), discord });
        sleep_ms
    }

    /// Makes Discord show `desired` (or nothing).
    fn sync(&mut self, desired: Option<Activity>, mono: i64) {
        let changed = match (&desired, &self.shown) {
            (None, None) => false,
            (Some(a), Some(b)) => !a.same_as(b, SEEK_TOLERANCE_MS),
            _ => true,
        };
        if !changed {
            return;
        }
        let refused = matches!((&desired, &self.rejected),
            (Some(a), Some((r, at))) if a.same_as(r, SEEK_TOLERANCE_MS) && mono - at < REJECT_COOLDOWN_MS);
        if refused {
            return;
        }
        let Some(d) = self.dc.as_mut() else {
            if desired.is_none() {
                self.shown = None; // nothing connected, nothing shown
            }
            return;
        };
        // The oldest of the last RATE_COUNT updates is outside the window.
        let slot_free = mono - self.sends[self.send_at] > RATE_WINDOW_MS;
        let payload = match &desired {
            // The user may have hidden it or quit while we were busy.
            Some(_) if !SHARED.enabled.load(SeqCst) || SHARED.quit.load(SeqCst) => return,
            Some(a) if slot_free => Some(a.json()),
            // Out of updates: never leave a different song up meanwhile.
            Some(a) if self.shown.as_ref().is_some_and(|s| !s.same_content(a)) => None,
            Some(_) => return, // same song, only the time moved: wait for a slot
            None => None,
        };
        let clearing = payload.is_none();
        self.sends[self.send_at] = mono;
        self.send_at = (self.send_at + 1) % RATE_COUNT;
        match d.set_activity(payload.as_deref()) {
            Ok(_) => {
                log(&self.cfg, &format!("set: {}", payload.as_deref().unwrap_or("(cleared)")));
                self.shown = if clearing { None } else { desired };
                self.rejected = None;
                self.backoff = BACKOFF_MIN_MS;
                self.dc_error.clear();
            }
            Err(discord::Error::Rejected(e)) => {
                log(&self.cfg, &format!("rejected: {e} :: {}", payload.as_deref().unwrap_or("(clear)")));
                // Dropping the pipe is the one sure way to clear our activity.
                self.disconnect();
                self.retry_at = mono;
                if !clearing {
                    self.rejected = desired.map(|a| (a, mono));
                }
                self.dc_error = format!("rejected: {e}");
            }
            Err(discord::Error::Disconnected(e)) => {
                log(&self.cfg, &format!("discord lost: {e}"));
                self.disconnect();
                self.dc_error = e;
                self.schedule_reconnect(mono);
            }
        }
    }
}
