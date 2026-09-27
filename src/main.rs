//! The exe: no `std`, no C runtime. All the app is in the library (lib.rs).
#![no_std]
#![no_main]
#![windows_subsystem = "windows"]

mod rt;

#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    // Exiting closes the Discord pipe, which clears the presence.
    unsafe { windows::Win32::System::Threading::ExitProcess(3) }
}
