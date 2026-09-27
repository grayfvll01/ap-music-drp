//! Minimal Discord local IPC client (`\\.\pipe\discord-ipc-N`).
//! Frame: u32 LE opcode, u32 LE length, UTF-8 JSON.
//!
//! All pipe I/O is overlapped with a timeout, so a Discord (or proxy) that
//! stops reading can never freeze the worker with a song still showing.

use crate::json::{self, Json};
use crate::prelude::*;
use crate::sys;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::{PeekNamedPipe, WaitNamedPipeW};
use windows::Win32::System::Threading::WaitForSingleObject;
use windows::core::PCWSTR;

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;
const OP_PONG: u32 = 4;
const TIMEOUT_MS: u32 = 5000;
pub const NOT_RUNNING: &str = "Discord is not running";

pub enum Error {
    /// The pipe is gone, stuck, or Discord closed it: reconnect later.
    Disconnected(String),
    /// Discord answered but rejected the payload.
    Rejected(String),
}

pub struct Discord {
    pipe: HANDLE,
    nonce: u64,
}

impl Drop for Discord {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.pipe);
        }
    }
}

fn err_text(v: &Json) -> String {
    let d = v.get("data").unwrap_or(v);
    let msg = d.str("message").unwrap_or("unknown error");
    match d.num("code") {
        Some(c) => format!("{msg} ({c})"),
        None => msg.to_string(),
    }
}

impl Discord {
    /// Uses the first `discord-ipc-N` (0-9) that completes the handshake, as
    /// other clients do. A pipe that's busy is waited for briefly.
    pub fn connect(client_id: &str) -> Result<Self, String> {
        let mut last = String::from(NOT_RUNNING);
        for i in 0..10 {
            let name = sys::wide(&format!(r"\\.\pipe\discord-ipc-{i}"));
            let mut busy_retries = 0;
            loop {
                // Identification-level security: the pipe server can see who
                // we are but can't act as us.
                let flags = FILE_FLAG_OVERLAPPED | SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION;
                let h = unsafe {
                    CreateFileW(
                        PCWSTR(name.as_ptr()),
                        (GENERIC_READ | GENERIC_WRITE).0,
                        FILE_SHARE_NONE,
                        None,
                        OPEN_EXISTING,
                        flags,
                        None,
                    )
                };
                match h {
                    Ok(pipe) => {
                        let mut d = Discord { pipe, nonce: 0 };
                        match d.handshake(client_id) {
                            Ok(()) => return Ok(d),
                            Err(e) => last = e,
                        }
                        break;
                    }
                    Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() && busy_retries < 3 => {
                        busy_retries += 1;
                        unsafe {
                            let _ = WaitNamedPipeW(PCWSTR(name.as_ptr()), 1000);
                        }
                    }
                    Err(_) => break, // not there / not ours: try the next number
                }
            }
        }
        Err(last)
    }

    fn handshake(&mut self, client_id: &str) -> Result<(), String> {
        let mut p = String::from(r#"{"v":1,"client_id":"#);
        json::push_str(&mut p, client_id);
        p.push('}');
        self.write(OP_HANDSHAKE, &p)?;
        loop {
            let (op, raw, v) = self.read()?;
            match op {
                OP_FRAME if v.str("evt") == Some("READY") => return Ok(()),
                OP_PING => self.write(OP_PONG, &raw)?,
                _ => return Err(format!("Discord refused the connection: {}", err_text(&v))),
            }
        }
    }

    /// Sets (`Some(activity_json)`) or clears (`None`) the activity; returns
    /// Discord's raw reply.
    pub fn set_activity(&mut self, activity: Option<&str>) -> Result<String, Error> {
        self.nonce += 1;
        let nonce = self.nonce.to_string();
        let payload = format!(
            r#"{{"cmd":"SET_ACTIVITY","args":{{"pid":{},"activity":{}}},"nonce":"{}"}}"#,
            unsafe { windows::Win32::System::Threading::GetCurrentProcessId() },
            activity.unwrap_or("null"),
            nonce
        );
        self.write(OP_FRAME, &payload).map_err(Error::Disconnected)?;
        loop {
            let (op, raw, v) = self.read().map_err(Error::Disconnected)?;
            match op {
                OP_PING => self.write(OP_PONG, &raw).map_err(Error::Disconnected)?,
                OP_CLOSE => return Err(Error::Disconnected(err_text(&v))),
                OP_FRAME if v.str("nonce") == Some(nonce.as_str()) => {
                    return if v.str("evt") == Some("ERROR") { Err(Error::Rejected(err_text(&v))) } else { Ok(raw) };
                }
                _ => {} // unrelated dispatch
            }
        }
    }

    /// Non-blocking health check; also answers any PING Discord sent.
    pub fn alive(&mut self) -> bool {
        loop {
            let mut avail = 0u32;
            if unsafe { PeekNamedPipe(self.pipe, None, 0, None, Some(&mut avail), None) }.is_err() {
                return false;
            }
            if avail < 8 {
                return true;
            }
            match self.read() {
                Ok((OP_PING, raw, _)) => {
                    if self.write(OP_PONG, &raw).is_err() {
                        return false;
                    }
                }
                Ok((OP_CLOSE, ..)) | Err(_) => return false,
                Ok(_) => {}
            }
        }
    }

    /// One overlapped read or write, waiting at most `ms`.
    fn io(&mut self, write: bool, buf: &mut [u8], ms: u32) -> Result<usize, String> {
        // Only one operation is ever in flight, so the pipe handle itself is
        // signalled when it completes; no event object needed.
        let mut ov = OVERLAPPED::default();
        let started = unsafe {
            if write {
                WriteFile(self.pipe, Some(buf), None, Some(&mut ov))
            } else {
                ReadFile(self.pipe, Some(buf), None, Some(&mut ov))
            }
        };
        if let Err(e) = started
            && e.code() != ERROR_IO_PENDING.to_hresult()
        {
            return Err("pipe closed".into());
        }
        let mut n = 0u32;
        if unsafe { WaitForSingleObject(self.pipe, ms) } != WAIT_OBJECT_0 {
            unsafe {
                let _ = CancelIoEx(self.pipe, Some(&ov));
                let _ = GetOverlappedResult(self.pipe, &ov, &mut n, true);
            }
            return Err("Discord did not respond".into());
        }
        unsafe { GetOverlappedResult(self.pipe, &ov, &mut n, false) }.map_err(|_| String::from("pipe closed"))?;
        if n == 0 {
            return Err("pipe closed".into());
        }
        Ok(n as usize)
    }

    /// Reads or writes all of `buf` within TIMEOUT_MS overall.
    fn io_all(&mut self, write: bool, buf: &mut [u8]) -> Result<(), String> {
        let deadline = sys::ticks() + TIMEOUT_MS as i64;
        let mut off = 0;
        while off < buf.len() {
            let left = deadline - sys::ticks();
            if left <= 0 {
                return Err("Discord did not respond".into());
            }
            off += self.io(write, &mut buf[off..], left as u32)?;
        }
        Ok(())
    }

    fn write(&mut self, op: u32, body: &str) -> Result<(), String> {
        let mut buf = Vec::with_capacity(8 + body.len());
        buf.extend_from_slice(&op.to_le_bytes());
        buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
        buf.extend_from_slice(body.as_bytes());
        self.io_all(true, &mut buf)
    }

    fn read(&mut self) -> Result<(u32, String, Json), String> {
        let mut hdr = [0u8; 8];
        self.io_all(false, &mut hdr)?;
        let op = u32::from_le_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
        let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        if len > 1 << 20 {
            return Err("oversized frame".into());
        }
        let mut body = vec![0u8; len];
        self.io_all(false, &mut body)?;
        let raw = String::from_utf8_lossy(&body).into_owned();
        let v = json::parse(&raw).unwrap_or(Json::Null);
        Ok((op, raw, v))
    }
}
