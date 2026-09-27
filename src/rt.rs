//! The C runtime pieces a `no_std` Rust exe still needs, so none is linked
//! (saves ~25 KB): the entry point, the mem* functions the compiler calls,
//! and a few symbols referenced by the prebuilt `core`/`alloc` libraries.

use core::arch::asm;

/// Process entry point (`/ENTRY:rawentry`, see build.rs).
#[unsafe(no_mangle)]
extern "system" fn rawentry() -> ! {
    let code = apple_music_discord_presence::real_main();
    unsafe { windows::Win32::System::Threading::ExitProcess(code) }
}

// `rep movsb` / `rep stosb` are fast on every x64 CPU from the last decade and
// can't be turned back into calls to these same functions by the optimiser.

#[unsafe(no_mangle)]
unsafe extern "C" fn memcpy(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    unsafe {
        asm!("rep movsb", inout("rdi") dst => _, inout("rsi") src => _, inout("rcx") n => _, options(nostack, preserves_flags))
    };
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memmove(dst: *mut u8, src: *const u8, n: usize) -> *mut u8 {
    unsafe {
        if (dst as usize).wrapping_sub(src as usize) >= n {
            // No harmful overlap: copy forwards.
            asm!("rep movsb", inout("rdi") dst => _, inout("rsi") src => _, inout("rcx") n => _, options(nostack, preserves_flags));
        } else {
            asm!(
                "std",
                "rep movsb",
                "cld",
                inout("rdi") dst.add(n - 1) => _,
                inout("rsi") src.add(n - 1) => _,
                inout("rcx") n => _,
                options(nostack)
            );
        }
    }
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memset(dst: *mut u8, c: i32, n: usize) -> *mut u8 {
    unsafe {
        asm!("rep stosb", inout("rdi") dst => _, inout("rcx") n => _, in("al") c as u8, options(nostack, preserves_flags))
    };
    dst
}

#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    for i in 0..n {
        // Volatile so this loop isn't "optimised" into a call to memcmp.
        let (x, y) = unsafe { (core::ptr::read_volatile(a.add(i)), core::ptr::read_volatile(b.add(i))) };
        if x != y {
            return x as i32 - y as i32;
        }
    }
    0
}

#[unsafe(no_mangle)]
unsafe extern "C" fn bcmp(a: *const u8, b: *const u8, n: usize) -> i32 {
    unsafe { memcmp(a, b, n) }
}

#[unsafe(no_mangle)]
unsafe extern "C" fn strlen(s: *const u8) -> usize {
    let mut n = 0;
    while unsafe { core::ptr::read_volatile(s.add(n)) } != 0 {
        n += 1;
    }
    n
}

#[unsafe(no_mangle)]
unsafe extern "C" fn wcslen(s: *const u16) -> usize {
    let mut n = 0;
    while unsafe { core::ptr::read_volatile(s.add(n)) } != 0 {
        n += 1;
    }
    n
}

// Stack probe the compiler calls before any function with a frame of 4 KB or
// more: touches each page in turn so Windows' guard page grows the stack.
// Size in RAX; preserves every register. Same as LLVM compiler-rt's.
core::arch::global_asm!(
    ".globl __chkstk",
    "__chkstk:",
    "push rcx",
    "push rax",
    "cmp rax, 0x1000",
    "lea rcx, [rsp + 24]",
    "jb 2f",
    "3:",
    "sub rcx, 0x1000",
    "test [rcx], rcx",
    "sub rax, 0x1000",
    "cmp rax, 0x1000",
    "ja 3b",
    "2:",
    "sub rcx, rax",
    "test [rcx], rcx",
    "pop rax",
    "pop rcx",
    "ret",
);

/// Tells the MSVC toolchain floating point is "initialised" (nothing to do).
#[unsafe(no_mangle)]
static _fltused: i32 = 0;

/// The prebuilt `core`/`alloc` are compiled for unwinding, so some of their
/// functions name the C++ exception handler. With `panic = "abort"` nothing
/// is ever unwound; if Windows asks (e.g. while an access violation is
/// crashing the process), "not handled here" is the right answer.
#[unsafe(no_mangle)]
extern "C" fn __CxxFrameHandler3() -> i32 {
    1 // ExceptionContinueSearch
}
