//! Caps on what one command may consume.
//!
//! A PDF can ask for gigabytes of memory in a few bytes, or keep an interpreter
//! busy for ever. An agent reading documents it did not write must be able to
//! survive that, so every command runs under a memory cap and a time limit and
//! fails with an ordinary error when it hits one.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static USED: AtomicUsize = AtomicUsize::new(0);
/// Zero means no cap.
static CAP: AtomicUsize = AtomicUsize::new(0);

/// Writes `message` to standard error and ends the process with status 1, at once.
///
/// This runs inside the allocator and beside threads that may be stuck, so it must
/// not allocate, take a lock or run exit handlers: `std::process::exit` does all
/// three, and from here it crashed on macOS and hung on Windows. The operating
/// system is asked directly instead.
#[cfg(unix)]
fn stop(message: &[u8]) -> ! {
    unsafe extern "C" {
        fn write(fd: i32, buffer: *const std::ffi::c_void, count: usize) -> isize;
        fn _exit(status: i32) -> !;
    }
    // SAFETY: `write` reads `message.len()` bytes from a live slice; `_exit` takes no pointers.
    unsafe {
        write(2, message.as_ptr().cast(), message.len());
        _exit(1)
    }
}

#[cfg(windows)]
fn stop(message: &[u8]) -> ! {
    use std::ffi::c_void;
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn WriteFile(
            file: *mut c_void,
            buffer: *const u8,
            count: u32,
            written: *mut u32,
            overlapped: *mut c_void,
        ) -> i32;
        fn GetCurrentProcess() -> *mut c_void;
        fn TerminateProcess(process: *mut c_void, status: u32) -> i32;
    }
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    let mut written = 0u32;
    // SAFETY: `WriteFile` reads `message.len()` bytes from a live slice and writes one
    // `u32`; `TerminateProcess` on the current process does not return.
    unsafe {
        WriteFile(
            GetStdHandle(STD_ERROR_HANDLE),
            message.as_ptr(),
            message.len() as u32,
            &mut written,
            std::ptr::null_mut(),
        );
        TerminateProcess(GetCurrentProcess(), 1);
    }
    loop {
        std::hint::spin_loop();
    }
}

#[cfg(not(any(unix, windows)))]
fn stop(_: &[u8]) -> ! {
    std::process::abort()
}

/// The system allocator with a running total, which ends the process at the cap.
///
/// Counting in the allocator, rather than limiting address space, works on every
/// platform and measures what is really held.
pub struct Capped;

impl Capped {
    fn charge(size: usize) {
        let used = USED.fetch_add(size, Ordering::Relaxed) + size;
        let cap = CAP.load(Ordering::Relaxed);
        if cap != 0 && used > cap {
            stop(
                b"{\"error\":\"memory limit exceeded while processing this file; raise it with --max-memory if the file is trusted\"}\n",
            );
        }
    }
}

// SAFETY: every method forwards to the system allocator with the arguments it was
// given; the bookkeeping around the calls touches only atomics.
unsafe impl GlobalAlloc for Capped {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::charge(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::charge(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        USED.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if new_size > layout.size() {
            Self::charge(new_size - layout.size());
        } else {
            USED.fetch_sub(layout.size() - new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Caps the memory held at any moment, in mebibytes. Zero lifts the cap.
pub fn set_max_memory(mebibytes: usize) {
    CAP.store(mebibytes.saturating_mul(1024 * 1024), Ordering::Relaxed);
}

/// Ends the process with an error once `seconds` have passed. Zero disables it.
///
/// A watchdog thread is the only way to stop a computation that never returns.
pub fn set_timeout(seconds: u64) {
    if seconds == 0 {
        return;
    }
    // Built now: by the time it is needed, allocating may no longer be possible.
    let message = format!(
        "{{\"error\":\"time limit of {seconds} s exceeded while processing this file; raise it with --timeout if the file is trusted\"}}\n"
    );
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        stop(message.as_bytes());
    });
}
