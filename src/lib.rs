//! Apple Music Discord Presence: Discord Rich Presence for Apple Music (and
//! Spotify) on Windows.
//!
//! Everything lives in this library so it can be unit-tested with `std`;
//! the shipped exe (`main.rs`) uses it without `std` to stay tiny.
#![cfg_attr(not(test), no_std)]
// In tests the worker runs against a simulation, leaving some OS helpers unused.
#![cfg_attr(test, allow(dead_code))]

extern crate alloc;

mod app;
mod autostart;
mod config;
mod discord;
mod http;
mod itunes;
mod json;
mod presence;
mod smtc;
mod sys;
mod tray;
mod update;

pub use tray::real_main;

/// Shown in the tray, notifications and Windows.
pub const APP_NAME: &str = "Apple Music Discord Presence";

mod prelude {
    pub use alloc::format;
    pub use alloc::string::{String, ToString};
    pub use alloc::vec;
    pub use alloc::vec::Vec;
}
