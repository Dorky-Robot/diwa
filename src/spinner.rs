//! Terminal spinner for long-running operations.
//!
//! Writes to stderr so stdout stays clean for piped output.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const FRAMES: &[&str] = &[
    "\u{2838}", "\u{2834}", "\u{2826}", "\u{2807}", "\u{280b}", "\u{2819}",
];

pub struct Spinner {
    stop: Arc<AtomicBool>,
    message: Arc<Mutex<String>>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Spinner {
    /// Start a spinner with a message. Renders to stderr.
    pub fn start(message: &str) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let msg = Arc::new(Mutex::new(message.to_string()));

        let stop_clone = stop.clone();
        let msg_clone = msg.clone();

        let handle = thread::spawn(move || {
            let mut i = 0;
            while !stop_clone.load(Ordering::Relaxed) {
                let frame = FRAMES[i % FRAMES.len()];
                let text = msg_clone.lock().unwrap().clone();
                eprint!("\r\x1b[2K\x1b[90m{frame} {text}\x1b[0m");
                let _ = std::io::stderr().flush();
                i += 1;
                thread::sleep(Duration::from_millis(100));
            }
        });

        Spinner {
            stop,
            message: msg,
            handle: Some(handle),
        }
    }

    /// Update the spinner message.
    pub fn set_message(&self, message: &str) {
        *self.message.lock().unwrap() = message.to_string();
    }

    /// Stop the spinner and clear the line.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        eprint!("\r\x1b[2K");
        let _ = std::io::stderr().flush();
    }
}
