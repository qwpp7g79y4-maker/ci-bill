//! SIGINT sets a flag so the process can stop without a half-written report.
//!
//! The handler is installed on Linux x86_64, where the glibc `sigaction` layout
//! below was measured (152 bytes). Other targets keep the default disposition.
//! `Read::read_to_end` retries [`std::io::ErrorKind::Interrupted`], so
//! [`StopOnSigint`] turns a caught SIGINT into a different error.

use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

/// Exit status for SIGINT. Ordinary success and error statuses stay as they are.
pub const EXIT_INTERRUPTED: u8 = 130;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

pub fn install() {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    install_handler();
}

/// Reads from `inner`, and returns a non-interrupted error once SIGINT was caught.
pub struct StopOnSigint<R> {
    inner: R,
}

impl<R> StopOnSigint<R> {
    pub fn new(inner: R) -> Self {
        Self { inner }
    }
}

impl<R: Read> Read for StopOnSigint<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if interrupted() {
            return Err(stopped());
        }
        match self.inner.read(buf) {
            Err(err) if err.kind() == io::ErrorKind::Interrupted && interrupted() => Err(stopped()),
            other => other,
        }
    }
}

fn stopped() -> io::Error {
    io::Error::other("interrupted by SIGINT")
}

/// Runs `body` while SIGINT is held back, then lets a pending SIGINT through.
///
/// The report write uses this so the signal cannot cut a write in half. On
/// targets without the handler this is just `body`.
pub fn with_sigint_blocked<T>(body: impl FnOnce() -> T) -> T {
    block_sigint();
    let guard = Unblock;
    let value = body();
    drop(guard);
    value
}

struct Unblock;

impl Drop for Unblock {
    fn drop(&mut self) {
        unblock_sigint();
    }
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn block_sigint() {}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
fn unblock_sigint() {}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SIGINT: i32 = 2;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SIG_BLOCK: i32 = 0;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const SIG_UNBLOCK: i32 = 1;

/// glibc `sigset_t` on this target: 1024 bits.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[repr(C)]
struct Sigset {
    _val: [u64; 16],
}

/// glibc `struct sigaction` on this target. Offsets were measured: handler 0,
/// mask 8, flags 136, restorer 144, size 152.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[repr(C)]
struct Sigaction {
    sa_handler: usize,
    sa_mask: Sigset,
    sa_flags: i32,
    sa_restorer: usize,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const _: () = {
    assert!(std::mem::size_of::<Sigset>() == 128);
    assert!(std::mem::size_of::<Sigaction>() == 152);
};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
unsafe extern "C" {
    fn sigaction(sig: i32, act: *const Sigaction, old: *mut Sigaction) -> i32;
    fn sigemptyset(set: *mut Sigset) -> i32;
    fn sigaddset(set: *mut Sigset, sig: i32) -> i32;
    fn sigprocmask(how: i32, set: *const Sigset, old: *mut Sigset) -> i32;
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn install_handler() {
    extern "C" fn on_sigint(_sig: i32) {
        // A single store is the signal-safe part. The rest of the stop runs
        // back on the main path, which can finish or discard the report.
        INTERRUPTED.store(true, Ordering::SeqCst);
    }

    // SAFETY: the struct matches glibc's `sigaction` on linux x86_64, flags
    // stay 0 so a blocked `read` returns EINTR, and glibc fills `sa_restorer`.
    unsafe {
        let mut action: Sigaction = std::mem::zeroed();
        if sigemptyset(&mut action.sa_mask) != 0 {
            return;
        }
        action.sa_handler = on_sigint as *const () as usize;
        action.sa_flags = 0;
        let _ = sigaction(SIGINT, &action, std::ptr::null_mut());
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn sigint_set() -> Option<Sigset> {
    // SAFETY: `Sigset` is glibc's `sigset_t` on this target. Zeroed memory is
    // an empty set; `sigemptyset` makes that explicit.
    unsafe {
        let mut set: Sigset = std::mem::zeroed();
        if sigemptyset(&mut set) != 0 || sigaddset(&mut set, SIGINT) != 0 {
            return None;
        }
        Some(set)
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn block_sigint() {
    let Some(set) = sigint_set() else {
        return;
    };
    // SAFETY: `set` is a filled glibc `sigset_t` and SIG_BLOCK is 0.
    unsafe {
        let _ = sigprocmask(SIG_BLOCK, &set, std::ptr::null_mut());
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn unblock_sigint() {
    let Some(set) = sigint_set() else {
        return;
    };
    // SAFETY: same set as `block_sigint`. Unblocking delivers a pending SIGINT
    // to `on_sigint`, which only stores the flag.
    unsafe {
        let _ = sigprocmask(SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}
