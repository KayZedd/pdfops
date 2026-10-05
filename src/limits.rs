//! Caps on what one command may consume.
//!
//! A PDF can ask for gigabytes of memory in a few bytes, or keep an interpreter
//! busy for ever. An agent reading documents it did not write must be able to
//! survive that, so every command runs under a memory cap and a time limit and
//! fails with an ordinary error when it hits one.

use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

static USED: AtomicUsize = AtomicUsize::new(0);
/// Zero means no cap.
static CAP: AtomicUsize = AtomicUsize::new(0);

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
            // No allocation is possible here, so the message is fixed and written raw.
            let _ = std::io::stderr().write_all(
                b"{\"error\":\"memory limit exceeded while processing this file; raise it with --max-memory if the file is trusted\"}\n",
            );
            std::process::exit(1);
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
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        eprintln!(
            "{{\"error\":\"time limit of {seconds} s exceeded while processing this file; raise it with --timeout if the file is trusted\"}}"
        );
        std::process::exit(1);
    });
}
