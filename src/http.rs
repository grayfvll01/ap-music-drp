//! Tiny HTTPS GET on top of WinHTTP (ships with Windows, adds no size).

use crate::prelude::*;
use crate::sys::wide;
use core::ffi::c_void;
use windows::Win32::Networking::WinHttp::*;
use windows::core::PCWSTR;

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

pub struct Client {
    session: Handle,
}

impl Client {
    pub fn new() -> Option<Self> {
        let ua = wide(concat!("AppleMusicDiscordPresence/", env!("CARGO_PKG_VERSION")));
        let h = unsafe {
            WinHttpOpen(PCWSTR(ua.as_ptr()), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, PCWSTR::null(), PCWSTR::null(), 0)
        };
        if h.is_null() {
            return None;
        }
        unsafe {
            // resolve, connect, send, receive (ms)
            let _ = WinHttpSetTimeouts(h, 3000, 3000, 3000, 4000);
            // Let WinHTTP ask for gzip and unpack it: artist lookups are ~10x smaller.
            let gz = WINHTTP_DECOMPRESSION_FLAG_GZIP | WINHTTP_DECOMPRESSION_FLAG_DEFLATE;
            let _ = WinHttpSetOption(Some(h), WINHTTP_OPTION_DECOMPRESSION, Some(&gz.to_ne_bytes()));
        }
        Some(Client { session: Handle(h) })
    }

    /// GET `https://{host}{path}`: the body on HTTP 200, else the status
    /// code (0 when the request itself failed). Gives up once `deadline`
    /// (monotonic ms) has passed between steps. Replies up to 4 MB.
    pub fn get(&self, host: &str, path: &str, deadline: i64) -> Result<Vec<u8>, u32> {
        self.get_max(host, path, deadline, 4 << 20)
    }

    /// GET an `https://` URL (redirects are followed), up to `max` bytes.
    pub fn get_url(&self, url: &str, deadline: i64, max: usize) -> Result<Vec<u8>, u32> {
        let rest = url.strip_prefix("https://").ok_or(0u32)?;
        let (host, path) = match rest.split_once('/') {
            Some((h, p)) => (h, format!("/{p}")),
            None => (rest, "/".into()),
        };
        self.get_max(host, &path, deadline, max)
    }

    fn get_max(&self, host: &str, path: &str, deadline: i64, max: usize) -> Result<Vec<u8>, u32> {
        let late = || crate::sys::ticks() > deadline;
        if late() {
            return Err(0);
        }
        let (host, path) = (wide(host), wide(path));
        unsafe {
            let conn = Handle(WinHttpConnect(self.session.0, PCWSTR(host.as_ptr()), INTERNET_DEFAULT_HTTPS_PORT, 0));
            if conn.0.is_null() {
                return Err(0);
            }
            let req = Handle(WinHttpOpenRequest(
                conn.0,
                windows::core::w!("GET"),
                PCWSTR(path.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                core::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ));
            if req.0.is_null() {
                return Err(0);
            }
            WinHttpSendRequest(req.0, None, None, 0, 0, 0).map_err(|_| 0u32)?;
            if late() {
                return Err(0);
            }
            WinHttpReceiveResponse(req.0, core::ptr::null_mut()).map_err(|_| 0u32)?;
            let mut status = 0u32;
            let mut len = 4u32;
            WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some(&mut status as *mut u32 as *mut c_void),
                &mut len,
                core::ptr::null_mut(),
            )
            .map_err(|_| 0u32)?;
            if status != 200 {
                return Err(status);
            }
            let mut body = Vec::new();
            let mut chunk = vec![0u8; 16 * 1024];
            loop {
                let mut n = 0u32;
                WinHttpReadData(req.0, chunk.as_mut_ptr() as *mut c_void, chunk.len() as u32, &mut n)
                    .map_err(|_| 0u32)?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n as usize]);
                // A huge or trickling reply isn't worth waiting for.
                if body.len() > max || late() {
                    return Err(0);
                }
            }
            Ok(body)
        }
    }
}

/// Percent-encodes a query-string value (UTF-8, RFC 3986 unreserved kept).
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 15) as usize] as char);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn encode() {
        assert_eq!(super::encode("Dr. Dre & Snoop"), "Dr.+Dre+%26+Snoop");
        assert_eq!(super::encode("é$"), "%C3%A9%24");
    }
}
