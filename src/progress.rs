//! Progress events for commands that take a while.
//!
//! A command normally says nothing until its one JSON document is ready. With
//! streaming on, each finished unit of work (a page, an output file) is reported
//! as a line of JSON on standard output first, and the result follows as the
//! last line. Pages are worked on in parallel, so events come in the order the
//! work finishes, not in page order; each one names its page.

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

static STREAMING: AtomicBool = AtomicBool::new(false);

/// Turns progress events on for the rest of the process.
pub fn enable() {
    STREAMING.store(true, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    STREAMING.load(Ordering::Relaxed)
}

/// Counts finished units of one step and reports each.
pub struct Progress {
    step: &'static str,
    total: usize,
    done: Mutex<usize>,
}

impl Progress {
    pub fn new(step: &'static str, total: usize) -> Self {
        Progress {
            step,
            total,
            done: Mutex::new(0),
        }
    }

    /// Reports one more finished unit. `detail` is an object whose entries join the event.
    pub fn tick(&self, detail: Value) {
        if !enabled() {
            return;
        }
        // Counting and printing under one lock keeps `done` rising from line to line.
        let mut done = self.done.lock().unwrap_or_else(|e| e.into_inner());
        *done += 1;
        let mut event =
            json!({"event": "progress", "step": self.step, "done": *done, "total": self.total});
        if let (Some(event), Value::Object(detail)) = (event.as_object_mut(), detail) {
            event.extend(detail);
        }
        let mut out = std::io::stdout().lock();
        // A reader that has gone away is not a reason to stop the work.
        let _ = writeln!(out, "{event}");
        let _ = out.flush();
    }
}
