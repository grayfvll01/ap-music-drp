//! The few OS services the app needs, straight from Win32. This replaces
//! Rust's `std` (which alone is ~100 KB of the exe): files, time, threads,
//! a lock, a wake-up event, console output, and the heap allocator.

use crate::prelude::*;
use core::cell::UnsafeCell;
use core::ffi::c_void;
use windows::Win32::Foundation::{CloseHandle, FILETIME, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::Console::{CONSOLE_MODE, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE, WriteConsoleW};
use windows::Win32::System::Environment::GetCommandLineW;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemInformation::{GetSystemTimeAsFileTime, GetTickCount64};
use windows::Win32::System::Threading::*;
use windows::core::PCWSTR;

/// NUL-terminated UTF-16 copy of `s` for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// 100ns ticks between 1601-01-01 (FILETIME epoch) and 1970-01-01.
pub const EPOCH_DIFF: i64 = 116_444_736_000_000_000;

pub fn now_ms() -> i64 {
    let ft = unsafe { GetSystemTimeAsFileTime() };
    let ticks = ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64;
    (ticks - EPOCH_DIFF) / 10_000
}

pub fn sleep(ms: u32) {
    unsafe { Sleep(ms) }
}

/// Monotonic milliseconds (unaffected by clock changes), for timers.
pub fn ticks() -> i64 {
    unsafe { GetTickCount64() as i64 }
}

pub fn env(name: &str) -> Option<String> {
    let name = wide(name);
    let mut buf = vec![0u16; 1024];
    let n =
        unsafe { windows::Win32::System::Environment::GetEnvironmentVariableW(PCWSTR(name.as_ptr()), Some(&mut buf)) };
    (n > 0 && (n as usize) < buf.len()).then(|| String::from_utf16_lossy(&buf[..n as usize]))
}

pub fn exe_path() -> String {
    let mut buf = vec![0u16; 1024];
    let n = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

/// True if the command line contains `flag` as its own argument.
#[cfg_attr(test, allow(dead_code))]
pub fn has_arg(flag: &str) -> bool {
    let cmd = unsafe { GetCommandLineW() };
    let cmd = unsafe { cmd.to_string() }.unwrap_or_default();
    cmd.split_whitespace().skip(1).any(|a| a == flag)
}

struct File(HANDLE);

impl Drop for File {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn open(path: &str, access: u32, disposition: FILE_CREATION_DISPOSITION) -> Option<File> {
    let p = wide(path);
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    unsafe { CreateFileW(PCWSTR(p.as_ptr()), access, share, None, disposition, FILE_ATTRIBUTE_NORMAL, None) }
        .ok()
        .map(File)
}

fn write_all(h: HANDLE, mut data: &[u8]) -> bool {
    while !data.is_empty() {
        let mut n = 0u32;
        if unsafe { WriteFile(h, Some(data), Some(&mut n), None) }.is_err() || n == 0 {
            return false;
        }
        data = &data[n as usize..];
    }
    true
}

pub fn read_file(path: &str) -> Option<Vec<u8>> {
    let f = open(path, GENERIC_READ.0, OPEN_EXISTING)?;
    let mut out = Vec::new();
    let mut chunk = vec![0u8; 8192];
    loop {
        let mut n = 0u32;
        unsafe { ReadFile(f.0, Some(&mut chunk), Some(&mut n), None) }.ok()?;
        if n == 0 {
            return Some(out);
        }
        out.extend_from_slice(&chunk[..n as usize]);
        if out.len() > 1 << 20 {
            return None;
        }
    }
}

/// Creates `path` with `data`; fails if it already exists.
pub fn create_file(path: &str, data: &[u8]) -> bool {
    open(path, GENERIC_WRITE.0, CREATE_NEW).is_some_and(|f| write_all(f.0, data))
}

/// Replaces `path`'s contents with `data`.
pub fn write_file(path: &str, data: &[u8]) -> bool {
    open(path, GENERIC_WRITE.0, CREATE_ALWAYS).is_some_and(|f| write_all(f.0, data))
}

pub fn append_file(path: &str, data: &[u8]) {
    if let Some(f) = open(path, FILE_APPEND_DATA.0, OPEN_ALWAYS) {
        write_all(f.0, data);
    }
}

/// Moves `from` over `to` in one step (readers see the old or the new file).
pub fn replace(from: &str, to: &str) -> bool {
    let (a, b) = (wide(from), wide(to));
    unsafe { MoveFileExW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr()), MOVEFILE_REPLACE_EXISTING) }.is_ok()
}

/// Moves a file; false if it couldn't (e.g. there's nothing at `from`).
pub fn rename(from: &str, to: &str) -> bool {
    let (a, b) = (wide(from), wide(to));
    unsafe { MoveFileExW(PCWSTR(a.as_ptr()), PCWSTR(b.as_ptr()), MOVEFILE_COPY_ALLOWED) }.is_ok()
}

pub fn delete_file(path: &str) {
    let p = wide(path);
    unsafe {
        let _ = DeleteFileW(PCWSTR(p.as_ptr()));
    }
}

pub fn create_dir(path: &str) {
    let p = wide(path);
    unsafe {
        let _ = CreateDirectoryW(PCWSTR(p.as_ptr()), None);
    }
}

/// (last write time, size) — enough to notice that a file changed.
pub fn stat(path: &str) -> Option<(u64, u64)> {
    let p = wide(path);
    let mut d = WIN32_FILE_ATTRIBUTE_DATA::default();
    unsafe { GetFileAttributesExW(PCWSTR(p.as_ptr()), GetFileExInfoStandard, &mut d as *mut _ as *mut c_void) }.ok()?;
    let t = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    Some((t(d.ftLastWriteTime), ((d.nFileSizeHigh as u64) << 32) | d.nFileSizeLow as u64))
}

/// Writes to the console (UTF-16, so any text shows correctly) or, when
/// redirected, to stdout as UTF-8.
pub fn print(s: &str) {
    unsafe {
        let Ok(h) = GetStdHandle(STD_OUTPUT_HANDLE) else { return };
        if h.is_invalid() || h.0.is_null() {
            return;
        }
        let mut mode = CONSOLE_MODE::default();
        if GetConsoleMode(h, &mut mode).is_ok() {
            let w: Vec<u16> = s.encode_utf16().collect();
            let _ = WriteConsoleW(h, &w, None, None);
        } else {
            write_all(h, s.as_bytes());
        }
    }
}

/// A mutex around `T` (SRW lock; no poisoning, since panics abort).
pub struct Lock<T> {
    srw: UnsafeCell<SRWLOCK>,
    val: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Lock<T> {}
unsafe impl<T: Send> Send for Lock<T> {}

impl<T> Lock<T> {
    pub const fn new(v: T) -> Self {
        Lock { srw: UnsafeCell::new(SRWLOCK_INIT), val: UnsafeCell::new(v) }
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        unsafe {
            AcquireSRWLockExclusive(self.srw.get());
            let r = f(&mut *self.val.get());
            ReleaseSRWLockExclusive(self.srw.get());
            r
        }
    }
}

/// Wake-up event, stored as a raw handle so it can live in a `static`.
pub fn event_new() -> isize {
    unsafe { CreateEventW(None, false, false, PCWSTR::null()) }.map(|h| h.0 as isize).unwrap_or(0)
}

pub fn event_set(h: isize) {
    if h != 0 {
        unsafe {
            let _ = SetEvent(HANDLE(h as _));
        }
    }
}

/// Sleeps up to `ms`, returning early if the event is set.
pub fn event_wait(h: isize, ms: u32) {
    unsafe {
        if h == 0 {
            Sleep(ms);
        } else {
            WaitForSingleObject(HANDLE(h as _), ms);
        }
    }
}

/// Runs `f` on a new thread.
pub fn spawn(f: fn()) -> bool {
    unsafe extern "system" fn trampoline(p: *mut c_void) -> u32 {
        let f: fn() = unsafe { core::mem::transmute(p) };
        f();
        0
    }
    match unsafe {
        CreateThread(None, 0, Some(trampoline), Some(f as *const () as *const c_void), THREAD_CREATION_FLAGS(0), None)
    } {
        Ok(h) => {
            unsafe {
                let _ = CloseHandle(h);
            }
            true
        }
        Err(_) => false,
    }
}

/// The process heap as Rust's allocator (HeapAlloc is 16-byte aligned on x64).
#[cfg(not(test))]
mod heap {
    use core::alloc::{GlobalAlloc, Layout};
    use windows::Win32::System::Memory::{
        GetProcessHeap, HEAP_FLAGS, HEAP_ZERO_MEMORY, HeapAlloc, HeapFree, HeapReAlloc,
    };

    struct Heap;

    unsafe impl GlobalAlloc for Heap {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            if l.align() > 16 {
                return core::ptr::null_mut();
            }
            unsafe { HeapAlloc(GetProcessHeap().unwrap_or_default(), HEAP_FLAGS(0), l.size()) as *mut u8 }
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            if l.align() > 16 {
                return core::ptr::null_mut();
            }
            unsafe { HeapAlloc(GetProcessHeap().unwrap_or_default(), HEAP_ZERO_MEMORY, l.size()) as *mut u8 }
        }
        unsafe fn dealloc(&self, p: *mut u8, _: Layout) {
            unsafe {
                let _ = HeapFree(GetProcessHeap().unwrap_or_default(), HEAP_FLAGS(0), Some(p as *const _));
            }
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            if l.align() > 16 {
                return core::ptr::null_mut();
            }
            unsafe {
                HeapReAlloc(GetProcessHeap().unwrap_or_default(), HEAP_FLAGS(0), Some(p as *const _), new) as *mut u8
            }
        }
    }

    #[global_allocator]
    static HEAP: Heap = Heap;
}
